//! JSONL metadata writer for captured frames.

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use tracing::{debug, info};

use crate::capture::CapturedFrame;

/// Log entry for a captured frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameLogEntry {
    /// Capture timestamp.
    pub timestamp: DateTime<Utc>,
    /// Unique frame identifier.
    pub frame_id: String,
    /// S3 key where the frame was uploaded.
    pub s3_key: String,
    /// S3 bucket name.
    pub s3_bucket: String,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Monitor ID that was captured.
    pub monitor_id: u32,
    /// File size in bytes.
    pub file_size_bytes: usize,
    /// Time to capture the frame in milliseconds.
    pub capture_duration_ms: u64,
    /// Time to upload the frame in milliseconds.
    pub upload_duration_ms: u64,
    /// Seconds idle before this capture (0 if not idle).
    pub idle_seconds_before: u64,
}

/// Session event types for JSONL logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum SessionEvent {
    #[serde(rename = "session_start")]
    SessionStart {
        timestamp: DateTime<Utc>,
        version: String,
    },
    #[serde(rename = "session_end")]
    SessionEnd {
        timestamp: DateTime<Utc>,
        frames_captured: u64,
    },
    #[serde(rename = "idle_start")]
    IdleStart {
        timestamp: DateTime<Utc>,
        idle_after_seconds: u64,
    },
    #[serde(rename = "idle_end")]
    IdleEnd {
        timestamp: DateTime<Utc>,
        idle_duration_seconds: u64,
    },
}

/// JSONL logger for frame metadata.
pub struct JsonlLogger {
    logs_dir: PathBuf,
    current_file: Option<BufWriter<File>>,
    current_date: Option<String>,
    idle_start_time: Option<DateTime<Utc>>,
}

impl JsonlLogger {
    /// Create a new JSONL logger.
    pub fn new(logs_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&logs_dir)
            .with_context(|| format!("Failed to create logs directory: {:?}", logs_dir))?;

        Ok(Self {
            logs_dir,
            current_file: None,
            current_date: None,
            idle_start_time: None,
        })
    }

    /// Get or create the log file for today.
    fn get_writer(&mut self) -> Result<&mut BufWriter<File>> {
        let today = Local::now().format("%Y-%m-%d").to_string();

        // Check if we need to rotate to a new file
        if self.current_date.as_ref() != Some(&today) {
            let log_path = self.logs_dir.join(format!("{}.jsonl", today));
            
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .with_context(|| format!("Failed to open log file: {:?}", log_path))?;

            self.current_file = Some(BufWriter::new(file));
            self.current_date = Some(today.clone());
            
            debug!("Opened log file: {:?}", log_path);
        }

        self.current_file
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No log file available"))
    }

    /// Write a line to the JSONL log.
    fn write_line<T: Serialize>(&mut self, entry: &T) -> Result<()> {
        let line = serde_json::to_string(entry)?;
        let writer = self.get_writer()?;
        writeln!(writer, "{}", line)?;
        writer.flush()?;
        Ok(())
    }

    /// Log a captured frame.
    pub fn log_frame(
        &mut self,
        frame: &CapturedFrame,
        s3_key: &str,
        s3_bucket: &str,
        upload_duration_ms: u64,
        idle_seconds_before: u64,
    ) -> Result<()> {
        let entry = FrameLogEntry {
            timestamp: frame.timestamp,
            frame_id: frame.frame_id(),
            s3_key: s3_key.to_string(),
            s3_bucket: s3_bucket.to_string(),
            width: frame.width,
            height: frame.height,
            monitor_id: frame.monitor_id,
            file_size_bytes: frame.data.len(),
            capture_duration_ms: frame.capture_duration_ms,
            upload_duration_ms,
            idle_seconds_before,
        };

        self.write_line(&entry)
    }

    /// Log session start event.
    pub fn log_session_start(&mut self, version: &str) -> Result<()> {
        let event = SessionEvent::SessionStart {
            timestamp: Utc::now(),
            version: version.to_string(),
        };
        info!("Session started");
        self.write_line(&event)
    }

    /// Log session end event.
    pub fn log_session_end(&mut self, frames_captured: u64) -> Result<()> {
        let event = SessionEvent::SessionEnd {
            timestamp: Utc::now(),
            frames_captured,
        };
        info!("Session ended, {} frames captured", frames_captured);
        self.write_line(&event)
    }

    /// Log idle start event.
    pub fn log_idle_start(&mut self, idle_after_seconds: u64) -> Result<()> {
        self.idle_start_time = Some(Utc::now());
        let event = SessionEvent::IdleStart {
            timestamp: Utc::now(),
            idle_after_seconds,
        };
        self.write_line(&event)
    }

    /// Log idle end event.
    pub fn log_idle_end(&mut self) -> Result<()> {
        let idle_duration = self
            .idle_start_time
            .map(|start| (Utc::now() - start).num_seconds().max(0) as u64)
            .unwrap_or(0);

        self.idle_start_time = None;

        let event = SessionEvent::IdleEnd {
            timestamp: Utc::now(),
            idle_duration_seconds: idle_duration,
        };
        self.write_line(&event)
    }

    /// Get the current idle start time.
    pub fn idle_start_time(&self) -> Option<DateTime<Utc>> {
        self.idle_start_time
    }
}

impl Drop for JsonlLogger {
    fn drop(&mut self) {
        // Flush any remaining data
        if let Some(ref mut writer) = self.current_file {
            let _ = writer.flush();
        }
    }
}

