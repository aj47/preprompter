//! Preprompter - macOS Screen Capture Daemon
//!
//! A lightweight screen capture daemon for macOS that captures screenshots,
//! detects user inactivity, and uploads to S3-compatible storage.
//! Includes a menu bar icon for status and control.

mod capture;
mod config;
mod idle;
mod logging;
mod storage;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use system_status_bar_macos::{Menu, MenuItem, StatusItem};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::capture::ScreenCapture;
use crate::config::Config;
use crate::idle::{ActivityState, IdleDetector};
use crate::logging::JsonlLogger;
use crate::storage::S3Uploader;

/// Application version.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Commands from menu bar to capture loop
#[derive(Debug, Clone)]
enum MenuCommand {
    ToggleCapture,
    Quit,
}

fn main() -> Result<()> {
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

    // Channel for menu commands
    let (cmd_tx, cmd_rx) = mpsc::channel::<MenuCommand>(10);

    // Shared state for capture status
    let capture_enabled = Arc::new(AtomicBool::new(true));
    let capture_enabled_clone = capture_enabled.clone();
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    // Spawn tokio runtime in a separate thread
    let config_clone = config.clone();
    let capture_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        runtime.block_on(async move {
            if let Err(e) = run_capture_loop(config_clone, cmd_rx, capture_enabled_clone, running_clone).await {
                error!("Capture loop error: {}", e);
            }
        });
    });

    // Create menu bar icon on main thread (required for macOS)
    let cmd_tx_toggle = cmd_tx.clone();
    let cmd_tx_quit = cmd_tx.clone();
    let capture_enabled_menu = capture_enabled.clone();

    let toggle_item = MenuItem::new(
        "Pause Capture",
        Some(Box::new(move || {
            let is_enabled = capture_enabled_menu.load(Ordering::SeqCst);
            capture_enabled_menu.store(!is_enabled, Ordering::SeqCst);
            let _ = cmd_tx_toggle.blocking_send(MenuCommand::ToggleCapture);
        })),
        None,
    );

    let quit_item = MenuItem::new(
        "Quit Preprompter",
        Some(Box::new(move || {
            let _ = cmd_tx_quit.blocking_send(MenuCommand::Quit);
        })),
        None,
    );

    let menu = Menu::new(vec![toggle_item, quit_item]);
    let _status_item = StatusItem::new("📷", menu);

    info!("Menu bar initialized - click 📷 to toggle/quit");

    // Run macOS event loop on main thread (required for menu bar)
    // The sync_infinite_event_loop needs a receiver for event loop messages
    // But since our menu items handle events via callbacks, we just need a dummy channel
    let (_event_sender, event_receiver) = std::sync::mpsc::channel::<()>();

    // This blocks until the app quits - runs the macOS event loop
    system_status_bar_macos::sync_infinite_event_loop(event_receiver, |_| {
        // No-op callback - menu items handle their own events
    });

    // This is reached when event loop terminates
    let _ = capture_thread.join();

    info!("Preprompter shutdown complete");
    Ok(())
}

/// Run the capture loop (runs in tokio runtime)
async fn run_capture_loop(
    config: Config,
    mut cmd_rx: mpsc::Receiver<MenuCommand>,
    capture_enabled: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
) -> Result<()> {
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
        config.capture.resolution_scale,
    )?;

    info!(
        "Capture settings: monitor_id={}, resolution_scale={:.0}%",
        if config.capture.monitor_id < 0 { "all".to_string() } else { config.capture.monitor_id.to_string() },
        config.capture.resolution_scale * 100.0
    );

    let idle_detector = IdleDetector::new(config.idle.threshold())?;
    let s3_uploader = S3Uploader::new(&config.s3).await?;
    let mut jsonl_logger = JsonlLogger::new(config.logging.logs_dir())?;

    // Log session start
    jsonl_logger.log_session_start(VERSION)?;

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
                // Skip capture if paused or idle
                if !capture_enabled.load(Ordering::SeqCst) || is_idle {
                    continue;
                }

                // Capture frame(s) - multi-monitor or single
                let frames_result = if screen_capture.captures_all_monitors() {
                    screen_capture.capture_all().await
                } else {
                    screen_capture.capture().await.map(|f| vec![f])
                };

                match frames_result {
                    Ok(frames) => {
                        for frame in frames {
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
                                        "Captured frame {} (mon:{}) -> {} ({} bytes, capture={}ms, upload={}ms)",
                                        frame_id, frame.monitor_id, result.key, file_size, capture_ms, result.upload_duration_ms
                                    );
                                }
                                Err(e) => {
                                    error!("Failed to upload frame {}: {}", frame_id, e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to capture frame: {}", e);
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    MenuCommand::ToggleCapture => {
                        let enabled = capture_enabled.load(Ordering::SeqCst);
                        info!("Capture {}", if enabled { "resumed" } else { "paused" });
                    }
                    MenuCommand::Quit => {
                        info!("Quit command received");
                        running.store(false, Ordering::SeqCst);
                        break;
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

    // Exit the process to close the menu bar
    std::process::exit(0);
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

