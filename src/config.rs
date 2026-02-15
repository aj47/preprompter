//! Configuration loading from TOML files and environment variables.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Root configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub capture: CaptureConfig,
    #[serde(default)]
    pub idle: IdleConfig,
    #[serde(default)]
    pub s3: S3Config,
    #[serde(default)]
    pub upload: UploadConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// Screen capture configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    /// Monitor ID to capture (0 = primary monitor, -1 = all monitors).
    #[serde(default)]
    pub monitor_id: i32,
    /// Capture interval in seconds.
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: u64,
    /// JPEG quality (1-100).
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
    /// Resolution scale (0.25 = 25%, 0.5 = 50%, 1.0 = full).
    #[serde(default = "default_resolution_scale")]
    pub resolution_scale: f32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            monitor_id: 0,
            interval_seconds: default_interval_seconds(),
            jpeg_quality: default_jpeg_quality(),
            resolution_scale: default_resolution_scale(),
        }
    }
}

impl CaptureConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_seconds)
    }
}

/// Idle detection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleConfig {
    /// Idle threshold in seconds.
    #[serde(default = "default_idle_threshold")]
    pub threshold_seconds: u64,
    /// Check interval in milliseconds.
    #[serde(default = "default_check_interval_ms")]
    pub check_interval_ms: u64,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            threshold_seconds: default_idle_threshold(),
            check_interval_ms: default_check_interval_ms(),
        }
    }
}

impl IdleConfig {
    pub fn threshold(&self) -> Duration {
        Duration::from_secs(self.threshold_seconds)
    }

    pub fn check_interval(&self) -> Duration {
        Duration::from_millis(self.check_interval_ms)
    }
}

/// S3-compatible storage configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    /// S3 bucket name.
    #[serde(default = "default_bucket")]
    pub bucket: String,
    /// AWS region.
    #[serde(default = "default_region")]
    pub region: String,
    /// Custom endpoint URL (for R2, MinIO, etc.).
    #[serde(default)]
    pub endpoint_url: Option<String>,
    /// Key prefix for uploaded frames.
    #[serde(default)]
    pub prefix: Option<String>,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: default_bucket(),
            region: default_region(),
            endpoint_url: None,
            prefix: None,
        }
    }
}

/// Upload behavior configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadConfig {
    /// Upload mode: "immediate" or "batch".
    #[serde(default = "default_upload_mode")]
    pub mode: UploadMode,
    /// Batch size for batch mode.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Number of retry attempts.
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: u32,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            mode: default_upload_mode(),
            batch_size: default_batch_size(),
            retry_attempts: default_retry_attempts(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UploadMode {
    #[default]
    Immediate,
    Batch,
}

/// Logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Data directory for logs and local staging.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Log level (trace, debug, info, warn, error).
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            level: default_log_level(),
        }
    }
}

impl LoggingConfig {
    /// Returns the logs directory path.
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Returns the local staging directory path.
    pub fn staging_dir(&self) -> PathBuf {
        self.data_dir.join("staging")
    }
}

// Default value functions
fn default_interval_seconds() -> u64 {
    3
}

fn default_jpeg_quality() -> u8 {
    80
}

fn default_resolution_scale() -> f32 {
    1.0
}

fn default_idle_threshold() -> u64 {
    60
}

fn default_check_interval_ms() -> u64 {
    500
}

fn default_bucket() -> String {
    "my-screen-captures".to_string()
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_upload_mode() -> UploadMode {
    UploadMode::Immediate
}

fn default_batch_size() -> usize {
    10
}

fn default_retry_attempts() -> u32 {
    3
}

fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".preprompter"))
        .unwrap_or_else(|| PathBuf::from(".preprompter"))
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            capture: CaptureConfig::default(),
            idle: IdleConfig::default(),
            s3: S3Config::default(),
            upload: UploadConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read config file: {:?}", path.as_ref()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| "Failed to parse config file")?;
        Ok(config)
    }

    /// Load configuration with environment variable overrides.
    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let mut config = if let Some(path) = config_path {
            Self::from_file(path)?
        } else {
            // Try default config locations
            let default_paths = [
                PathBuf::from("config/default.toml"),
                dirs::config_dir()
                    .map(|d| d.join("preprompter/config.toml"))
                    .unwrap_or_default(),
            ];

            let mut loaded = None;
            for path in &default_paths {
                if path.exists() {
                    loaded = Some(Self::from_file(path)?);
                    break;
                }
            }
            loaded.unwrap_or_default()
        };

        // Apply environment variable overrides
        config.apply_env_overrides();

        // Expand home directory in data_dir
        config.logging.data_dir = expand_tilde(&config.logging.data_dir);

        Ok(config)
    }

    /// Apply environment variable overrides.
    fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("PREPROMPTER_CAPTURE_INTERVAL") {
            if let Ok(v) = val.parse() {
                self.capture.interval_seconds = v;
            }
        }
        if let Ok(val) = std::env::var("PREPROMPTER_JPEG_QUALITY") {
            if let Ok(v) = val.parse() {
                self.capture.jpeg_quality = v;
            }
        }
        if let Ok(val) = std::env::var("PREPROMPTER_IDLE_THRESHOLD") {
            if let Ok(v) = val.parse() {
                self.idle.threshold_seconds = v;
            }
        }
        if let Ok(val) = std::env::var("PREPROMPTER_S3_BUCKET") {
            self.s3.bucket = val;
        }
        if let Ok(val) = std::env::var("PREPROMPTER_S3_REGION") {
            self.s3.region = val;
        }
        if let Ok(val) = std::env::var("PREPROMPTER_S3_ENDPOINT") {
            self.s3.endpoint_url = Some(val);
        }
        if let Ok(val) = std::env::var("PREPROMPTER_DATA_DIR") {
            self.logging.data_dir = PathBuf::from(val);
        }
        if let Ok(val) = std::env::var("PREPROMPTER_LOG_LEVEL") {
            self.logging.level = val;
        }
    }

    /// Validate configuration values.
    pub fn validate(&self) -> Result<()> {
        if self.capture.jpeg_quality == 0 || self.capture.jpeg_quality > 100 {
            anyhow::bail!("JPEG quality must be between 1 and 100");
        }
        if self.capture.interval_seconds == 0 {
            anyhow::bail!("Capture interval must be greater than 0");
        }
        if self.idle.threshold_seconds == 0 {
            anyhow::bail!("Idle threshold must be greater than 0");
        }
        if self.s3.bucket.is_empty() {
            anyhow::bail!("S3 bucket name cannot be empty");
        }
        Ok(())
    }
}

/// Expand ~ to home directory.
fn expand_tilde(path: &Path) -> PathBuf {
    if let Some(path_str) = path.to_str() {
        if path_str.starts_with("~") {
            if let Some(home) = dirs::home_dir() {
                return home.join(&path_str[2..]);
            }
        }
    }
    path.to_path_buf()
}

