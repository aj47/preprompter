//! Idle detection using IOKit HIDIdleTime for system-wide idle monitoring (macOS).

use anyhow::Result;
use chrono::{DateTime, Utc};
use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, info};

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

/// Idle detector using IOKit HIDIdleTime.
pub struct IdleDetector {
    /// Idle threshold duration.
    threshold: Duration,
    /// Shared state.
    state: Arc<IdleState>,
    /// Broadcast sender for state changes.
    state_tx: broadcast::Sender<ActivityState>,
    /// Event tap thread handle.
    event_tap_handle: Option<JoinHandle<()>>,
    /// Checker thread handle.
    checker_handle: Option<JoinHandle<()>>,
}

impl IdleDetector {
    /// Create a new idle detector with the given threshold.
    pub fn new(threshold: Duration) -> Result<Self> {
        let (state_tx, _) = broadcast::channel(16);

        Ok(Self {
            threshold,
            state: Arc::new(IdleState::new()),
            state_tx,
            event_tap_handle: None,
            checker_handle: None,
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
        let _monitor_handle = thread::Builder::new()
            .name("idle-monitor".to_string())
            .spawn(move || {
                run_idle_monitor(state_clone);
            })?;

        let state_clone = self.state.clone();
        let threshold = self.threshold;
        let state_tx = self.state_tx.clone();
        let _checker_handle = thread::Builder::new()
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

/// Get the system idle time using IOKit HIDIdleTime.
fn get_system_idle_time() -> Option<Duration> {
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceGetMatchingService(
            main_port: u32,
            matching: core_foundation::base::CFTypeRef,
        ) -> u32;
        fn IOServiceMatching(name: *const std::os::raw::c_char) -> core_foundation::base::CFTypeRef;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: core_foundation::string::CFStringRef,
            allocator: core_foundation::base::CFAllocatorRef,
            options: u32,
        ) -> core_foundation::base::CFTypeRef;
        fn IOObjectRelease(object: u32) -> i32;
    }

    unsafe {
        let service_name = std::ffi::CString::new("IOHIDSystem").ok()?;
        let matching = IOServiceMatching(service_name.as_ptr());
        if matching.is_null() {
            return None;
        }

        let service = IOServiceGetMatchingService(0, matching);
        if service == 0 {
            return None;
        }

        let key = CFString::new("HIDIdleTime");
        let property = IORegistryEntryCreateCFProperty(
            service,
            key.as_concrete_TypeRef(),
            std::ptr::null(),
            0,
        );

        IOObjectRelease(service);

        if property.is_null() {
            return None;
        }

        let cf_number: CFNumber = CFNumber::wrap_under_create_rule(property as *mut _);
        let nanoseconds: i64 = cf_number.to_i64()?;

        Some(Duration::from_nanos(nanoseconds as u64))
    }
}

/// Run the idle detection loop using IOKit HIDIdleTime polling.
fn run_idle_monitor(state: Arc<IdleState>) {
    info!("Starting idle monitor using IOKit HIDIdleTime");

    let poll_interval = Duration::from_millis(500);

    while state.running.load(Ordering::SeqCst) {
        thread::sleep(poll_interval);

        if let Some(idle_time) = get_system_idle_time() {
            if idle_time < poll_interval {
                state.update_activity();
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
