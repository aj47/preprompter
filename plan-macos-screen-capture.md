# macOS Screen Capture Daemon - Implementation Plan

## Overview
Build a lightweight screen capture daemon for macOS Apple Silicon that captures screenshots at ~1 fps/3s, detects user inactivity, and uploads to S3-compatible storage with JSONL metadata logs.

### Goals & Success Criteria
- ✅ Capture frames using ScreenCaptureKit (macOS 12.3+) via `sck-rs` crate
- ✅ Idle detection via mouse/keyboard monitoring using CGEventTap
- ✅ Upload to S3-compatible storage (AWS S3, Cloudflare R2, MinIO)
- ✅ JSONL logs with frame metadata
- ✅ Low CPU/memory footprint (<5% CPU when active, near-zero when idle)

### Scope Boundaries
- **In Scope**: Capture, idle detection, S3 upload, JSONL logging
- **Out of Scope**: OCR, local AI processing, cloud agent, UI/tray app

---

## Prerequisites
- macOS 12.3+ (for ScreenCaptureKit via `sck-rs`)
- Rust 1.75+ (2024 edition)
- Screen Recording permission granted in System Settings

---

## Project Structure
```
preprompter/
├── Cargo.toml
├── src/
│   ├── main.rs                 # Entry point, daemon orchestration
│   ├── config.rs               # Configuration loading (TOML/env)
│   ├── capture/
│   │   ├── mod.rs              # Capture module exports
│   │   └── screen.rs           # Screen capture using sck-rs
│   ├── idle/
│   │   ├── mod.rs              # Idle detection module exports
│   │   └── detector.rs         # CGEventTap-based idle monitoring
│   ├── storage/
│   │   ├── mod.rs              # Storage module exports
│   │   ├── s3.rs               # S3 upload client
│   │   └── local.rs            # Local file staging (optional)
│   └── logging/
│       ├── mod.rs              # Logging module exports
│       └── jsonl.rs            # JSONL metadata writer
├── config/
│   └── default.toml            # Default configuration
└── tests/
    ├── capture_tests.rs
    ├── idle_tests.rs
    └── storage_tests.rs
```

---

## Dependencies (Cargo.toml)
```toml
[package]
name = "preprompter"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
tokio = { version = "1.44", features = ["full", "tracing"] }
sck-rs = { git = "https://github.com/screenpipe/sck-rs" }
image = { version = "0.25", default-features = false, features = ["jpeg"] }
aws-sdk-s3 = "1.77"
aws-config = { version = "1.6", features = ["behavior-version-latest"] }
toml = "0.8"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
chrono = { version = "0.4", features = ["serde"] }
anyhow = "1.0"
thiserror = "2.0"
core-foundation = "0.10"
core-graphics = "0.24"

[dev-dependencies]
tempfile = "3.15"
tokio-test = "0.4"

[profile.release]
lto = true
codegen-units = 1
opt-level = "s"
strip = true
```

---

## Core Modules Breakdown

### 1. Screen Capture Module (`src/capture/screen.rs`)
**Purpose**: Capture screenshots using ScreenCaptureKit via `sck-rs`

```rust
pub struct ScreenCapture { monitor_id: u32, quality: u8 }
pub struct CapturedFrame { pub data: Vec<u8>, pub width: u32, pub height: u32, pub timestamp: DateTime<Utc>, pub monitor_id: u32 }

impl ScreenCapture {
    pub fn new(monitor_id: u32, quality: u8) -> Result<Self>;
    pub async fn capture(&self) -> Result<CapturedFrame>;
    pub fn list_monitors() -> Result<Vec<MonitorInfo>>;
}
```
- Use `sck-rs::Monitor` for ScreenCaptureKit access (macOS 12.3+)
- Convert raw RGBA to JPEG using `image` crate, quality configurable (default: 80)
- Handle permission denied errors gracefully

### 2. Idle Detection Module (`src/idle/detector.rs`)
**Purpose**: Monitor mouse/keyboard activity, signal active/idle state

```rust
pub struct IdleDetector { idle_threshold: Duration, last_activity: Arc<AtomicU64> }
pub enum ActivityState { Active, Idle { since: DateTime<Utc> } }

impl IdleDetector {
    pub fn new(idle_threshold: Duration) -> Result<Self>;
    pub fn start(&self) -> Result<JoinHandle<()>>;  // Spawns CGEventTap thread
    pub fn state(&self) -> ActivityState;
    pub fn subscribe(&self) -> broadcast::Receiver<ActivityState>;
}
```
- Use `core-graphics::CGEventTap` for system-wide event monitoring
- Track `kCGEventKeyDown`, `kCGEventMouseMoved`, `kCGEventLeftMouseDown`, etc.
- Update atomic timestamp on each event (lock-free), run in dedicated CFRunLoop thread

### 3. S3 Upload Module (`src/storage/s3.rs`)
**Purpose**: Upload frames to S3-compatible storage

```rust
pub struct S3Uploader { client: aws_sdk_s3::Client, bucket: String, prefix: Option<String> }
pub struct S3Config { pub endpoint_url: Option<String>, pub bucket: String, pub region: String, ... }
pub struct UploadResult { pub key: String, pub etag: String, pub uploaded_at: DateTime<Utc> }

impl S3Uploader {
    pub async fn new(config: &S3Config) -> Result<Self>;
    pub async fn upload_frame(&self, frame: &CapturedFrame) -> Result<UploadResult>;
    pub async fn upload_batch(&self, frames: Vec<CapturedFrame>) -> Result<Vec<UploadResult>>;
}
```
- Path format: `{prefix}/YYYY/MM/DD/HH/frame-{timestamp_ms}.jpg`
- Support immediate and batch upload modes, retry with exponential backoff

### 4. JSONL Logger Module (`src/logging/jsonl.rs`)
**Purpose**: Write metadata for each captured/uploaded frame

```rust
pub struct JsonlLogger { writer: BufWriter<File>, path: PathBuf }
#[derive(Serialize)]
pub struct FrameLogEntry { pub timestamp: DateTime<Utc>, pub frame_id: String, pub s3_key: String, ... }
```
**Log Path**: `{data_dir}/logs/YYYY-MM-DD.jsonl`

### 5. Main Daemon (`src/main.rs`)
Orchestrates: Load config → Init tracing → Check permissions → Init modules → Start idle detection → Main capture loop with tokio::select! for interval/state/signals

---

## Configuration Schema (`config/default.toml`)
```toml
[capture]
monitor_id = 0
interval_seconds = 3
jpeg_quality = 80

[idle]
threshold_seconds = 60
check_interval_ms = 500

[s3]
bucket = "my-screen-captures"
region = "us-east-1"
endpoint_url = ""
prefix = ""

[upload]
mode = "immediate"
batch_size = 10
retry_attempts = 3

[logging]
data_dir = "~/.preprompter"
level = "info"
```

## JSONL Log Format Specification

Each line is a JSON object representing one captured frame:
```json
{
  "timestamp": "2026-02-14T10:30:45.123Z",
  "frame_id": "20260214-103045123",
  "s3_key": "2026/02/14/10/frame-1739528045123.jpg",
  "s3_bucket": "my-screen-captures",
  "width": 2560,
  "height": 1600,
  "monitor_id": 1,
  "file_size_bytes": 245832,
  "capture_duration_ms": 45,
  "upload_duration_ms": 234,
  "idle_seconds_before": 0
}
```

**Session markers** (optional):
```json
{"event": "session_start", "timestamp": "2026-02-14T09:00:00Z", "version": "0.1.0"}
{"event": "idle_start", "timestamp": "2026-02-14T10:35:00Z", "idle_after_seconds": 60}
{"event": "idle_end", "timestamp": "2026-02-14T10:40:00Z", "idle_duration_seconds": 300}
{"event": "session_end", "timestamp": "2026-02-14T18:00:00Z", "frames_captured": 8640}
```

---

## Implementation Phases

### Phase 1: Core Capture (Days 1-2)
**Goal**: Capture screenshots and save locally

| Task | File | Est. Hours |
|------|------|------------|
| Project setup, Cargo.toml | `Cargo.toml` | 0.5 |
| Config loading | `src/config.rs` | 1 |
| Screen capture using sck-rs | `src/capture/screen.rs` | 3 |
| Basic main loop (capture to local files) | `src/main.rs` | 2 |
| Unit tests for capture | `tests/capture_tests.rs` | 1 |

**Deliverable**: Working binary that captures frames to local disk

### Phase 2: Idle Detection (Days 3-4)
**Goal**: Pause capture when user is idle

| Task | File | Est. Hours |
|------|------|------------|
| CGEventTap setup | `src/idle/detector.rs` | 4 |
| State management & broadcast | `src/idle/detector.rs` | 2 |
| Integration with main loop | `src/main.rs` | 1 |
| Tests (mock event source) | `tests/idle_tests.rs` | 1 |

**Deliverable**: Daemon sleeps when user is inactive

### Phase 3: S3 Upload (Days 5-6)
**Goal**: Upload frames to S3-compatible storage

| Task | File | Est. Hours |
|------|------|------------|
| S3 client initialization | `src/storage/s3.rs` | 2 |
| Upload with path formatting | `src/storage/s3.rs` | 2 |
| Retry logic | `src/storage/s3.rs` | 1 |
| Batch upload mode | `src/storage/s3.rs` | 2 |
| Integration tests (LocalStack/MinIO) | `tests/storage_tests.rs` | 2 |

**Deliverable**: Frames uploaded to S3 with structured paths

### Phase 4: JSONL Logging & Polish (Days 7-8)
**Goal**: Complete logging, error handling, release build

| Task | File | Est. Hours |
|------|------|------------|
| JSONL writer with rotation | `src/logging/jsonl.rs` | 2 |
| Session event logging | `src/logging/jsonl.rs` | 1 |
| Error recovery & graceful shutdown | `src/main.rs` | 2 |
| Configuration validation | `src/config.rs` | 1 |
| Release build & testing | - | 2 |
| Documentation (README) | `README.md` | 1 |

**Deliverable**: Production-ready daemon

---

## Testing Strategy

### Unit Tests
| Module | Test Focus |
|--------|-----------|
| `capture` | Frame dimensions, JPEG encoding, monitor selection |
| `idle` | State transitions, threshold timing |
| `storage` | Path formatting, retry behavior |
| `logging` | JSON serialization, file rotation |

### Integration Tests
- **Capture + Upload**: Full pipeline with MinIO container
- **Idle + Capture**: Verify capture pauses/resumes correctly
- **Config Loading**: Test TOML parsing with various configs

### Manual Testing
1. Grant Screen Recording permission
2. Run daemon, verify frames appear in S3
3. Stop mouse/keyboard, verify capture pauses
4. Resume activity, verify capture resumes
5. Check JSONL logs for correct metadata
6. Test with R2/MinIO endpoints

### Performance Testing
- Memory usage over 8-hour run (should stay flat)
- CPU usage while active (<5%)
- CPU usage while idle (<0.5%)

---

## Rollback Plan

### Code Rollback
```bash
git revert HEAD~N  # Revert N commits
git checkout <previous-tag>
```

### S3 Data
- Frames are append-only; no rollback needed
- Delete by date prefix: `aws s3 rm s3://bucket/2026/02/14/ --recursive`

### Configuration
- Keep backup of `config/default.toml` before changes
- Environment variables override file config (easy rollback)

---

## Estimated Effort

| Phase | Duration | Complexity |
|-------|----------|------------|
| Phase 1: Core Capture | 2 days | Low |
| Phase 2: Idle Detection | 2 days | Medium |
| Phase 3: S3 Upload | 2 days | Low |
| Phase 4: Logging & Polish | 2 days | Low |
| **Total** | **8 days** | **Medium** |

### Risk Factors
- **CGEventTap complexity**: macOS event taps require careful threading
- **Permission prompts**: Screen Recording permission UX
- **S3 compatibility**: R2/MinIO may have subtle differences

### Mitigation
- Start with `core-graphics` examples for event tap
- Test on fresh macOS install for permission flow
- Test early with target S3 provider (R2 vs AWS)

---

## File Changes Summary

### Files to Create
| File | Purpose |
|------|---------|
| `Cargo.toml` | Project dependencies and build config |
| `src/main.rs` | Entry point and daemon orchestration |
| `src/config.rs` | Configuration loading from TOML/env |
| `src/capture/mod.rs` | Module re-exports |
| `src/capture/screen.rs` | Screen capture implementation |
| `src/idle/mod.rs` | Module re-exports |
| `src/idle/detector.rs` | Idle detection with CGEventTap |
| `src/storage/mod.rs` | Module re-exports |
| `src/storage/s3.rs` | S3 upload client |
| `src/storage/local.rs` | Local file staging (optional) |
| `src/logging/mod.rs` | Module re-exports |
| `src/logging/jsonl.rs` | JSONL metadata writer |
| `config/default.toml` | Default configuration file |
| `tests/capture_tests.rs` | Capture module tests |
| `tests/idle_tests.rs` | Idle detection tests |
| `tests/storage_tests.rs` | S3 storage tests |

### Files to Modify
None (greenfield project)

### Files to Delete
None

