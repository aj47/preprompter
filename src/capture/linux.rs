//! Screen capture implementation for Linux using X11 (XCB).

use anyhow::{Context, Result};
use chrono::Utc;
use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, Rgba};
use std::io::Cursor;
use std::time::Instant;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrConnectionExt;
use x11rb::protocol::xproto::{self, ConnectionExt, ImageFormat};
use x11rb::rust_connection::RustConnection;

use super::{CapturedFrame, MonitorInfo};

/// Screen capture manager using X11 XCB.
pub struct ScreenCapture {
    monitor_id: u32,
    jpeg_quality: u8,
}

impl ScreenCapture {
    /// Create a new screen capture instance.
    pub fn new(monitor_id: u32, jpeg_quality: u8) -> Result<Self> {
        let quality = jpeg_quality.clamp(1, 100);

        // Verify X11 connection is available
        let (conn, _) = RustConnection::connect(None)
            .context("Failed to connect to X11 display. Is DISPLAY set?")?;
        drop(conn);

        Ok(Self {
            monitor_id,
            jpeg_quality: quality,
        })
    }

    /// List all available monitors using RandR.
    pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
        let (conn, screen_num) = RustConnection::connect(None)
            .context("Failed to connect to X11 display")?;

        let screen = &conn.setup().roots[screen_num];
        let resources = conn
            .randr_get_screen_resources_current(screen.root)?
            .reply()
            .context("Failed to get screen resources")?;

        let mut monitors = Vec::new();
        let mut primary_output = None;

        // Try to get primary output
        if let Ok(reply) = conn.randr_get_output_primary(screen.root)?.reply() {
            primary_output = Some(reply.output);
        }

        for (idx, crtc) in resources.crtcs.iter().enumerate() {
            if let Ok(info) = conn.randr_get_crtc_info(*crtc, 0)?.reply() {
                if info.width > 0 && info.height > 0 {
                    let is_primary = if let Some(primary) = primary_output {
                        info.outputs.first().map(|o| *o == primary).unwrap_or(false)
                    } else {
                        idx == 0
                    };

                    monitors.push(MonitorInfo {
                        id: *crtc as u32,
                        width: info.width as u32,
                        height: info.height as u32,
                        is_primary,
                    });
                }
            }
        }

        // Fallback: if no CRTCs found, use the whole screen
        if monitors.is_empty() {
            monitors.push(MonitorInfo {
                id: 0,
                width: screen.width_in_pixels as u32,
                height: screen.height_in_pixels as u32,
                is_primary: true,
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

        // Run the blocking X11 capture in a separate thread
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

/// Blocking X11 screen capture implementation.
fn capture_frame_blocking(monitor_id: u32, quality: u8) -> Result<(Vec<u8>, u32, u32, u32)> {
    let (conn, screen_num) = RustConnection::connect(None)
        .context("Failed to connect to X11 display")?;

    let screen = &conn.setup().roots[screen_num];

    // Determine capture region from monitor_id
    let (x, y, width, height, actual_id) = get_capture_region(&conn, screen, monitor_id)?;

    // Capture screen pixels using GetImage
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            screen.root,
            x as i16,
            y as i16,
            width as u16,
            height as u16,
            !0, // all planes
        )?
        .reply()
        .context("Failed to capture screen image via X11 GetImage")?;

    let pixel_data = reply.data;
    let depth = reply.depth;

    // Convert pixel data to RGBA then encode to JPEG
    let rgba_data = convert_to_rgba(&pixel_data, width, height, depth)?;

    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(width, height, rgba_data)
            .context("Failed to create image buffer from captured pixels")?;

    let mut jpeg_buffer = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buffer, quality);
    encoder
        .encode_image(&img)
        .context("Failed to encode captured frame as JPEG")?;

    Ok((jpeg_buffer.into_inner(), width, height, actual_id))
}

/// Get the capture region for a given monitor ID.
/// Falls back to the full screen if the monitor ID is not found.
fn get_capture_region(
    conn: &RustConnection,
    screen: &xproto::Screen,
    monitor_id: u32,
) -> Result<(u32, u32, u32, u32, u32)> {
    // Try to use RandR to find the specific monitor
    if let Ok(resources) = conn
        .randr_get_screen_resources_current(screen.root)?
        .reply()
    {
        for crtc in &resources.crtcs {
            if let Ok(info) = conn.randr_get_crtc_info(*crtc, 0)?.reply() {
                if info.width > 0 && info.height > 0 && *crtc as u32 == monitor_id {
                    return Ok((
                        info.x as u32,
                        info.y as u32,
                        info.width as u32,
                        info.height as u32,
                        *crtc as u32,
                    ));
                }
            }
        }
    }

    // Fallback: capture the entire root window
    Ok((
        0,
        0,
        screen.width_in_pixels as u32,
        screen.height_in_pixels as u32,
        0,
    ))
}

/// Convert X11 pixel data to RGBA format.
/// X11 typically returns BGRA (32-bit depth) or BGR (24-bit depth).
fn convert_to_rgba(data: &[u8], width: u32, height: u32, depth: u8) -> Result<Vec<u8>> {
    let pixel_count = (width * height) as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);

    match depth {
        32 | 24 => {
            // 32-bit: BGRA or BGRx (4 bytes per pixel)
            let bytes_per_pixel = 4;
            for i in 0..pixel_count {
                let offset = i * bytes_per_pixel;
                if offset + 3 <= data.len() {
                    rgba.push(data[offset + 2]); // R (from B position)
                    rgba.push(data[offset + 1]); // G
                    rgba.push(data[offset]);     // B (from R position)
                    rgba.push(255);              // A (fully opaque)
                }
            }
        }
        16 => {
            // 16-bit: RGB565 (2 bytes per pixel)
            for i in 0..pixel_count {
                let offset = i * 2;
                if offset + 1 < data.len() {
                    let pixel = u16::from_le_bytes([data[offset], data[offset + 1]]);
                    let r = ((pixel >> 11) & 0x1F) as u8;
                    let g = ((pixel >> 5) & 0x3F) as u8;
                    let b = (pixel & 0x1F) as u8;
                    rgba.push((r << 3) | (r >> 2));
                    rgba.push((g << 2) | (g >> 4));
                    rgba.push((b << 3) | (b >> 2));
                    rgba.push(255);
                }
            }
        }
        _ => {
            anyhow::bail!("Unsupported X11 pixel depth: {}", depth);
        }
    }

    Ok(rgba)
}
