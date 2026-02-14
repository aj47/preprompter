# Preprompter

A macOS daemon that captures screenshots at regular intervals and uploads them to S3-compatible storage. Automatically pauses when the user is idle.

## Features

- 📸 **Screen Capture** - Uses macOS ScreenCaptureKit for efficient, low-overhead capture
- 😴 **Idle Detection** - Automatically pauses capture when you're away (via IOKit HIDIdleTime)
- ☁️ **S3 Upload** - Works with AWS S3, Cloudflare R2, MinIO, or any S3-compatible storage
- 📝 **JSONL Logging** - Structured metadata logs with daily rotation
- ⚙️ **Configurable** - TOML config files with environment variable overrides

## Quick Start

### 1. Install Rust (if needed)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Build

```bash
git clone https://github.com/aj47/preprompter.git
cd preprompter
cargo build --release
```

### 3. Configure

Create `~/.config/preprompter/config.toml`:

```toml
[capture]
interval_seconds = 3
jpeg_quality = 80

[idle]
threshold_seconds = 60

[s3]
bucket = "my-screenshots"
region = "us-east-1"
# endpoint = "https://your-r2-endpoint.r2.cloudflarestorage.com"  # For R2/MinIO

[upload]
enabled = true

[logging]
directory = "~/.local/share/preprompter/logs"
```

### 4. Set AWS Credentials

```bash
export AWS_ACCESS_KEY_ID="your-access-key"
export AWS_SECRET_ACCESS_KEY="your-secret-key"
```

### 5. Run

```bash
./target/release/preprompter
```

## Configuration

### Environment Variable Overrides

All config options can be overridden with `PREPROMPTER_` prefixed environment variables:

```bash
PREPROMPTER_S3_BUCKET=my-bucket
PREPROMPTER_CAPTURE_INTERVAL_SECONDS=5
PREPROMPTER_IDLE_THRESHOLD_SECONDS=120
```

### Config File Locations

The daemon searches for config in order:
1. `--config /path/to/config.toml` (CLI argument)
2. `~/.config/preprompter/config.toml`
3. `./config/default.toml`

## S3 Key Structure

Screenshots are organized by time:
```
2026/02/14/10/frame-1739528045123.jpg
```

## Log Format (JSONL)

Each captured frame is logged as a JSON line:
```json
{
  "timestamp": "2026-02-14T10:30:45.123Z",
  "frame_id": "20260214-103045123",
  "s3_key": "2026/02/14/10/frame-1739528045123.jpg",
  "width": 2560,
  "height": 1600,
  "file_size_bytes": 245832
}
```

## Using with Cloudflare R2

```toml
[s3]
bucket = "my-screenshots"
region = "auto"
endpoint = "https://ACCOUNT_ID.r2.cloudflarestorage.com"
```

## Requirements

- macOS 12.3+ (ScreenCaptureKit)
- Screen Recording permission (System Settings → Privacy & Security → Screen Recording)

## License

MIT

