//! Idle detection for Linux using X11 XScreenSaver extension.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::screensaver::ConnectionExt as ScreensaverConnectionExt;
use x11rb::rust_connection::RustConnection;

use super::ActivityState;

/// Shared state for idle detection.
struct IdleState {
    /// Timestamp of last activity (Unix epoch milliseconds).
    last_activity_ms: AtomicU64,
    /// Whether the detector is running.
    running: AtomicBool,
    /// Current idle state.
    is_idle: AtomicBool,
}

impl IdleState {
    fn new() -> Self {
        let now_ms = Utc::now().timestamp_millis() as u64;
        Self {
            last_activity_ms: AtomicU64::new(now_ms),
            running: AtomicBool::new(false),
            is_idle: AtomicBool::new(false),
        }
    }

    fn update_activity(&self) {
        let now_ms = Utc::now().timestamp_millis() as u64;
        self.last_activity_ms.store(now_ms, Ordering::SeqCst);
    }

    fn idle_duration(&self) -> Duration {
        let last_ms = self.last_activity_ms.load(Ordering::SeqCst);
        let now_ms = Utc::now().timestamp_millis() as u64;
        let elapsed_ms = now_ms.saturating_sub(last_ms);
        Duration::from_millis(elapsed_ms)
    }
}

/// Idle detector using X11 XScreenSaver extension.
pub struct IdleDetector {
    /// Idle threshold duration.
    threshold: Duration,
    /// Shared state.
    state: Arc<IdleState>,
    /// Broadcast sender for state changes.
    state_tx: broadcast::Sender<ActivityState>,
}

impl IdleDetector {
    /// Create a new idle detector with the given threshold.
    pub fn new(threshold: Duration) -> Result<Self> {
        // Verify X11 connection and screensaver extension availability
        let (conn, screen_num) = RustConnection::connect(None)
            .context("Failed to connect to X11 display. Is DISPLAY set?")?;

        let screen = &conn.setup().roots[screen_num];

        // Test that XScreenSaver extension works
        conn.screensaver_query_info(screen.root)
            .context("XScreenSaver extension not available")?
            .reply()
            .context("Failed to query XScreenSaver info")?;

        drop(conn);

        let (state_tx, _) = broadcast::channel(16);

        Ok(Self {
            threshold,
            state: Arc::new(IdleState::new()),
            state_tx,
        })
    }

    /// Subscribe to activity state changes.
    pub fn subscribe(&self) -> broadcast::Receiver<ActivityState> {
        self.state_tx.subscribe()
    }

    /// Get the current activity state.
    pub fn state(&self) -> ActivityState {
        if self.state.is_idle.load(Ordering::SeqCst) {
            let last_ms = self.state.last_activity_ms.load(Ordering::SeqCst);
            let since = DateTime::from_timestamp_millis(last_ms as i64)
                .unwrap_or_else(Utc::now);
            ActivityState::Idle { since }
        } else {
            ActivityState::Active
        }
    }

    /// Start the idle detector.
    pub fn start(&self) -> Result<()> {
        if self.state.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        info!("Starting idle detector with threshold {:?}", self.threshold);

        let state_clone = self.state.clone();
        thread::Builder::new()
            .name("idle-monitor".to_string())
            .spawn(move || {
                run_idle_monitor(state_clone);
            })?;

        let state_clone = self.state.clone();
        let threshold = self.threshold;
        let state_tx = self.state_tx.clone();
        thread::Builder::new()
            .name("idle-checker".to_string())
            .spawn(move || {
                run_idle_checker(state_clone, threshold, state_tx);
            })?;

        Ok(())
    }

    /// Stop the idle detector.
    pub fn stop(&self) {
        self.state.running.store(false, Ordering::SeqCst);
        info!("Idle detector stopped");
    }
}

/// Get the system idle time using X11 XScreenSaver extension.
fn get_x11_idle_time() -> Option<Duration> {
    let (conn, screen_num) = RustConnection::connect(None).ok()?;
    let screen = &conn.setup().roots[screen_num];

    let reply = conn
        .screensaver_query_info(screen.root)
        .ok()?
        .reply()
        .ok()?;

    // ms_since_user_input is the idle time in milliseconds
    Some(Duration::from_millis(reply.ms_since_user_input as u64))
}

/// Run the idle detection loop using XScreenSaver polling.
fn run_idle_monitor(state: Arc<IdleState>) {
    info!("Starting idle monitor using X11 XScreenSaver");

    let poll_interval = Duration::from_millis(500);

    while state.running.load(Ordering::SeqCst) {
        thread::sleep(poll_interval);

        match get_x11_idle_time() {
            Some(idle_time) => {
                if idle_time < poll_interval {
                    state.update_activity();
                }
            }
            None => {
                warn!("Failed to query X11 idle time");
            }
        }
    }

    debug!("Idle monitor thread exiting");
}

/// Run the idle state checker thread.
fn run_idle_checker(
    state: Arc<IdleState>,
    threshold: Duration,
    state_tx: broadcast::Sender<ActivityState>,
) {
    let check_interval = Duration::from_millis(500);
    let mut was_idle = false;

    while state.running.load(Ordering::SeqCst) {
        thread::sleep(check_interval);

        let idle_duration = state.idle_duration();
        let is_now_idle = idle_duration >= threshold;

        if is_now_idle != was_idle {
            state.is_idle.store(is_now_idle, Ordering::SeqCst);

            let new_state = if is_now_idle {
                let since = Utc::now() - chrono::Duration::from_std(idle_duration).unwrap_or_default();
                debug!("User became idle (idle for {:?})", idle_duration);
                ActivityState::Idle { since }
            } else {
                debug!("User became active");
                ActivityState::Active
            };

            let _ = state_tx.send(new_state);
            was_idle = is_now_idle;
        }
    }

    debug!("Idle checker thread exiting");
}

impl Drop for IdleDetector {
    fn drop(&mut self) {
        self.stop();
    }
}
