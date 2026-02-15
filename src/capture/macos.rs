//! Screen capture implementation using macOS ScreenCaptureKit.

use anyhow::{Context, Result};
use chrono::Utc;
use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, Rgba};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::{CapturedFrame, MonitorInfo};

/// Screen capture manager using ScreenCaptureKit.
pub struct ScreenCapture {
    monitor_id: u32,
    jpeg_quality: u8,
}

impl ScreenCapture {
    /// Create a new screen capture instance.
    pub fn new(monitor_id: u32, jpeg_quality: u8) -> Result<Self> {
        let quality = jpeg_quality.clamp(1, 100);
        Ok(Self {
            monitor_id,
            jpeg_quality: quality,
        })
    }

    /// List all available monitors.
    pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("Failed to get shareable content: {:?}", e))?;

        let displays = content.displays();
        let mut monitors = Vec::with_capacity(displays.len());

        for (idx, display) in displays.iter().enumerate() {
            monitors.push(MonitorInfo {
                id: display.display_id(),
                width: display.width() as u32,
                height: display.height() as u32,
                is_primary: idx == 0,
            });
        }

        Ok(monitors)
    }

    /// Capture a single frame from the configured monitor.
    pub async fn capture(&self) -> Result<CapturedFrame> {
        let start = Instant::now();
        let timestamp = Utc::now();
        let quality = self.jpeg_quality;
        let monitor_id = self.monitor_id;

        let result = tokio::task::spawn_blocking(move || {
            capture_frame_blocking(monitor_id, quality)
        })
        .await
        .context("Capture task panicked")?
        .context("Capture failed")?;

        let capture_duration_ms = start.elapsed().as_millis() as u64;

        Ok(CapturedFrame {
            data: result.0,
            width: result.1,
            height: result.2,
            timestamp,
            monitor_id: result.3,
            capture_duration_ms,
        })
    }
}

/// Frame handler that stores captured frame data
struct FrameHandler {
    frame_data: Arc<Mutex<Option<Vec<u8>>>>,
    captured: Arc<AtomicBool>,
    quality: u8,
}

impl SCStreamOutputTrait for FrameHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, output_type: SCStreamOutputType) {
        if output_type != SCStreamOutputType::Screen {
            return;
        }

        if self.captured.swap(true, Ordering::SeqCst) {
            return;
        }

        if let Some(pixel_buffer) = sample.image_buffer() {
            if let Some(jpeg_data) = encode_pixel_buffer_to_jpeg(&pixel_buffer, self.quality) {
                if let Ok(mut guard) = self.frame_data.lock() {
                    *guard = Some(jpeg_data);
                }
            }
        }
    }
}

/// Blocking capture implementation
fn capture_frame_blocking(monitor_id: u32, quality: u8) -> Result<(Vec<u8>, u32, u32, u32)> {
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("Failed to get shareable content: {:?}", e))?;

    let displays = content.displays();
    if displays.is_empty() {
        anyhow::bail!("No displays available for capture");
    }

    let display = displays
        .iter()
        .find(|d| d.display_id() == monitor_id)
        .or_else(|| displays.first())
        .ok_or_else(|| anyhow::anyhow!("Monitor {} not found", monitor_id))?;

    let display_id = display.display_id();
    let width = display.width() as u32;
    let height = display.height() as u32;

    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build();

    let config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA);

    let frame_data: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let captured = Arc::new(AtomicBool::new(false));

    let handler = FrameHandler {
        frame_data: frame_data.clone(),
        captured: captured.clone(),
        quality,
    };

    let mut stream = SCStream::new(&filter, &config);
    stream.add_output_handler(handler, SCStreamOutputType::Screen);

    stream
        .start_capture()
        .map_err(|e| anyhow::anyhow!("Failed to start capture: {:?}", e))?;

    let timeout = std::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        if captured.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let _ = stream.stop_capture();

    let data = frame_data
        .lock()
        .map_err(|_| anyhow::anyhow!("Lock poisoned"))?
        .take()
        .ok_or_else(|| anyhow::anyhow!("No frame captured - check Screen Recording permission"))?;

    Ok((data, width, height, display_id))
}

/// Encode a pixel buffer to JPEG format.
fn encode_pixel_buffer_to_jpeg(
    pixel_buffer: &screencapturekit::cv::CVPixelBuffer,
    quality: u8,
) -> Option<Vec<u8>> {
    let guard = pixel_buffer.lock(CVPixelBufferLockFlags::READ_ONLY).ok()?;

    let width = guard.width();
    let height = guard.height();
    let bytes_per_row = guard.bytes_per_row();

    let pixel_data = guard.as_slice();
    if pixel_data.is_empty() {
        return None;
    }

    let mut rgba_data = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row_start = y * bytes_per_row;
        for x in 0..width {
            let pixel_start = row_start + x * 4;
            if pixel_start + 3 < pixel_data.len() {
                rgba_data.push(pixel_data[pixel_start + 2]); // R
                rgba_data.push(pixel_data[pixel_start + 1]); // G
                rgba_data.push(pixel_data[pixel_start]); // B
                rgba_data.push(pixel_data[pixel_start + 3]); // A
            }
        }
    }

    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(width as u32, height as u32, rgba_data)?;

    let mut jpeg_buffer = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buffer, quality);

    if encoder.encode_image(&img).is_err() {
        return None;
    }

    Some(jpeg_buffer.into_inner())
}
