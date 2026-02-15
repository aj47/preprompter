//! Screen capture module with platform-specific implementations.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;

use chrono::{DateTime, Utc};

/// Information about a display/monitor.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

/// A captured frame with metadata.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    /// JPEG-encoded frame data.
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Capture timestamp.
    pub timestamp: DateTime<Utc>,
    /// Monitor ID that was captured.
    pub monitor_id: u32,
    /// Duration it took to capture and encode the frame.
    pub capture_duration_ms: u64,
}

impl CapturedFrame {
    /// Generate a unique frame ID based on timestamp.
    pub fn frame_id(&self) -> String {
        self.timestamp.format("%Y%m%d-%H%M%S%3f").to_string()
    }

    /// Generate S3 key path for this frame.
    pub fn s3_key(&self, prefix: Option<&str>) -> String {
        let date_path = self.timestamp.format("%Y/%m/%d/%H").to_string();
        let filename = format!("frame-{}.jpg", self.timestamp.timestamp_millis());
        match prefix {
            Some(p) if !p.is_empty() => format!("{}/{}/{}", p.trim_end_matches('/'), date_path, filename),
            _ => format!("{}/{}", date_path, filename),
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::ScreenCapture;
#[cfg(target_os = "linux")]
pub use linux::ScreenCapture;
