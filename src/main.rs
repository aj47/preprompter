//! Preprompter - Cross-Platform Screen Capture Daemon
//!
//! A lightweight screen capture daemon that captures screenshots,
//! detects user inactivity, and uploads to S3-compatible storage.
//! Supports macOS and Linux (X11).

mod capture;
mod config;
mod idle;
mod logging;
mod storage;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info, warn};

use crate::capture::ScreenCapture;
use crate::config::Config;
use crate::idle::{ActivityState, IdleDetector};
use crate::logging::JsonlLogger;
use crate::storage::S3Uploader;

/// Application version.
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from);

    // Load configuration
    let config = Config::load(config_path.as_deref())?;
    config.validate()?;

    // Initialize tracing
    init_tracing(&config.logging.level)?;

    info!("Starting preprompter v{}", VERSION);
    info!("Configuration loaded: capture interval={}s, idle threshold={}s",
        config.capture.interval_seconds,
        config.idle.threshold_seconds
    );

    // Ensure data directories exist
    std::fs::create_dir_all(config.logging.logs_dir())?;
    std::fs::create_dir_all(config.logging.staging_dir())?;

    // List available monitors
    match ScreenCapture::list_monitors() {
        Ok(monitors) => {
            info!("Available monitors:");
            for m in &monitors {
                info!("  Monitor {}: {}x{}{}", m.id, m.width, m.height,
                    if m.is_primary { " (primary)" } else { "" });
            }
        }
        Err(e) => warn!("Could not list monitors: {}", e),
    }

    // Initialize components
    let screen_capture = ScreenCapture::new(
        config.capture.monitor_id,
        config.capture.jpeg_quality,
    )?;

    let idle_detector = IdleDetector::new(config.idle.threshold())?;
    let s3_uploader = S3Uploader::new(&config.s3).await?;
    let mut jsonl_logger = JsonlLogger::new(config.logging.logs_dir())?;

    // Log session start
    jsonl_logger.log_session_start(VERSION)?;

    // Setup shutdown signal handling
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    tokio::spawn(async move {
        let _ = signal::ctrl_c().await;
        info!("Received shutdown signal");
        running_clone.store(false, Ordering::SeqCst);
    });

    // Start idle detection
    let mut activity_rx = idle_detector.subscribe();
    idle_detector.start()?;

    // Main capture loop
    let mut interval = tokio::time::interval(config.capture.interval());
    let mut frames_captured: u64 = 0;
    let mut is_idle = false;

    info!("Entering main capture loop");

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            _ = interval.tick() => {
                // Skip capture if idle
                if is_idle {
                    continue;
                }

                // Capture frame
                match screen_capture.capture().await {
                    Ok(frame) => {
                        let frame_id = frame.frame_id();
                        let file_size = frame.data.len();
                        let capture_ms = frame.capture_duration_ms;

                        // Upload to S3
                        match s3_uploader.upload_frame(&frame).await {
                            Ok(result) => {
                                frames_captured += 1;

                                // Log frame metadata
                                if let Err(e) = jsonl_logger.log_frame(
                                    &frame,
                                    &result.key,
                                    &config.s3.bucket,
                                    result.upload_duration_ms,
                                    0, // idle_seconds_before
                                ) {
                                    warn!("Failed to log frame: {}", e);
                                }

                                info!(
                                    "Captured frame {} -> {} ({} bytes, capture={}ms, upload={}ms)",
                                    frame_id, result.key, file_size, capture_ms, result.upload_duration_ms
                                );
                            }
                            Err(e) => {
                                error!("Failed to upload frame {}: {}", frame_id, e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to capture frame: {}", e);
                    }
                }
            }
            Ok(state) = activity_rx.recv() => {
                match state {
                    ActivityState::Active => {
                        if is_idle {
                            info!("User activity resumed");
                            is_idle = false;
                            let _ = jsonl_logger.log_idle_end();
                        }
                    }
                    ActivityState::Idle { since } => {
                        if !is_idle {
                            info!("User idle since {}", since);
                            is_idle = true;
                            let _ = jsonl_logger.log_idle_start(config.idle.threshold_seconds);
                        }
                    }
                }
            }
        }
    }

    // Cleanup
    info!("Shutting down...");
    jsonl_logger.log_session_end(frames_captured)?;
    idle_detector.stop();

    info!("Captured {} frames total. Goodbye!", frames_captured);
    Ok(())
}

/// Initialize tracing subscriber with the given log level.
fn init_tracing(level: &str) -> Result<()> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).with_thread_ids(false))
        .with(filter)
        .init();

    Ok(())
}
