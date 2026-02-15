//! Screen capture implementation for Windows using DXGI Desktop Duplication.

use anyhow::{Context, Result};
use chrono::Utc;
use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, Rgba};
use std::io::Cursor;
use std::time::Instant;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Gdi::*;

use super::{CapturedFrame, MonitorInfo};

/// Screen capture manager using Windows GDI (with DXGI monitor enumeration).
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

    /// List all available monitors using DXGI adapter enumeration.
    pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
        let mut monitors = Vec::new();

        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()
                .context("Failed to create DXGI factory")?;

            let mut adapter_idx = 0u32;
            while let Ok(adapter) = factory.EnumAdapters1(adapter_idx) {
                let mut output_idx = 0u32;
                while let Ok(output) = adapter.EnumOutputs(output_idx) {
                    let desc = output.GetDesc()
                        .context("Failed to get output description")?;

                    let width = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32;
                    let height = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32;

                    monitors.push(MonitorInfo {
                        id: (adapter_idx * 100 + output_idx),
                        width,
                        height,
                        is_primary: monitors.is_empty(), // First monitor is primary
                    });
                    output_idx += 1;
                }
                adapter_idx += 1;
            }
        }

        // Fallback: use GetSystemMetrics if DXGI enumeration found nothing
        if monitors.is_empty() {
            unsafe {
                let width = GetSystemMetrics(SM_CXSCREEN) as u32;
                let height = GetSystemMetrics(SM_CYSCREEN) as u32;
                monitors.push(MonitorInfo {
                    id: 0,
                    width,
                    height,
                    is_primary: true,
                });
            }
        }

        Ok(monitors)
    }

    /// Capture a single frame from the configured monitor.
    pub async fn capture(&self) -> Result<CapturedFrame> {
        let start = Instant::now();
        let timestamp = Utc::now();
        let quality = self.jpeg_quality;
        let monitor_id = self.monitor_id;

        // Run the blocking capture in a separate thread
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

/// Blocking screen capture using Windows GDI BitBlt.
/// This is the most compatible approach across all Windows versions.
fn capture_frame_blocking(monitor_id: u32, quality: u8) -> Result<(Vec<u8>, u32, u32, u32)> {
    unsafe {
        // Get the desktop device context
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            anyhow::bail!("Failed to get screen device context");
        }

        let width = GetSystemMetrics(SM_CXSCREEN) as u32;
        let height = GetSystemMetrics(SM_CYSCREEN) as u32;

        // Create a compatible DC and bitmap
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            ReleaseDC(None, hdc_screen);
            anyhow::bail!("Failed to create compatible DC");
        }

        let hbitmap = CreateCompatibleBitmap(hdc_screen, width as i32, height as i32);
        if hbitmap.is_invalid() {
            DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            anyhow::bail!("Failed to create compatible bitmap");
        }

        let old_bitmap = SelectObject(hdc_mem, hbitmap);

        // BitBlt the screen contents
        let success = BitBlt(
            hdc_mem,
            0,
            0,
            width as i32,
            height as i32,
            hdc_screen,
            0,
            0,
            SRCCOPY,
        );

        if !success.as_bool() {
            SelectObject(hdc_mem, old_bitmap);
            DeleteObject(hbitmap);
            DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            anyhow::bail!("BitBlt failed");
        }

        // Get bitmap data
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // Top-down bitmap
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut pixel_data = vec![0u8; (width * height * 4) as usize];

        let lines = GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            height,
            Some(pixel_data.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        // Cleanup GDI objects
        SelectObject(hdc_mem, old_bitmap);
        DeleteObject(hbitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        if lines == 0 {
            anyhow::bail!("GetDIBits failed - could not read screen pixels");
        }

        // Convert BGRA to RGBA
        let mut rgba_data = Vec::with_capacity(pixel_data.len());
        for chunk in pixel_data.chunks_exact(4) {
            rgba_data.push(chunk[2]); // R
            rgba_data.push(chunk[1]); // G
            rgba_data.push(chunk[0]); // B
            rgba_data.push(255);      // A
        }

        // Encode to JPEG
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_raw(width, height, rgba_data)
                .context("Failed to create image buffer")?;

        let mut jpeg_buffer = Cursor::new(Vec::new());
        let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buffer, quality);
        encoder
            .encode_image(&img)
            .context("Failed to encode frame as JPEG")?;

        Ok((jpeg_buffer.into_inner(), width, height, monitor_id))
    }
}
