//! Idle detection module with platform-specific implementations.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;

use chrono::{DateTime, Utc};

/// User activity state.
#[derive(Debug, Clone, PartialEq)]
pub enum ActivityState {
    /// User is currently active.
    Active,
    /// User has been idle since the given time.
    Idle { since: DateTime<Utc> },
}

#[cfg(target_os = "macos")]
pub use macos::IdleDetector;
#[cfg(target_os = "linux")]
pub use linux::IdleDetector;
