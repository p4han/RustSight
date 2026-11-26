#[cfg(windows)]
pub mod windows;

use crate::backend::{CapturedFrame, WindowCaptureBackend};
use crate::error::{Result, RustSightError};
use base64::Engine;
use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::imageops::FilterType as ResizeFilter;
use image::{Delay, Frame, ImageEncoder, RgbaImage};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

pub fn create_capture_backend() -> Box<dyn WindowCaptureBackend> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsCaptureBackend::new())
    }
    #[cfg(not(windows))]
    {
        compile_error!("Only Windows is currently supported; macOS/Linux backends can be added behind WindowCaptureBackend.")
    }
}

/// Crop a `CapturedFrame` to a sub-rectangle `(x, y, width, height)`, clamping to frame bounds.
pub fn crop_frame(frame: &CapturedFrame, x: u32, y: u32, width: u32, height: u32) -> Result<CapturedFrame> {
    if frame.width == 0 || frame.height == 0 {
        return Err(RustSightError::Image("Cannot crop empty frame".into()));
    }
    let cx = x.min(frame.width.saturating_sub(1));
    let cy = y.min(frame.height.saturating_sub(1));
    let cw = width.max(1).min(frame.width - cx);
    let ch = height.max(1).min(frame.height - cy);
    let mut rgba = vec![0u8; (cw * ch * 4) as usize];
    let src_stride = (frame.width * 4) as usize;
    let dst_stride = (cw * 4) as usize;
    for row in 0..ch as usize {
        let src_off = (cy as usize + row) * src_stride + (cx as usize * 4);
        let dst_off = row * dst_stride;
        rgba[dst_off..dst_off + dst_stride]
            .copy_from_slice(&frame.rgba[src_off..src_off + dst_stride]);
    }
    Ok(CapturedFrame {
        width: cw,
        height: ch,
        rgba,
    })
}

/// Downscale a frame with Lanczos3 if `max_width > 0 && frame.width > max_width`.
/// Returns `(scaled_frame, coord_scale)` where `coord_scale = orig_width / new_width`.
pub fn downscale_for_mcp(frame: CapturedFrame, max_width: u32) -> Result<(CapturedFrame, f64)> {
    if max_width == 0 || frame.width <= max_width || frame.width == 0 || frame.height == 0 {
        return Ok((frame, 1.0));
    }
    let scale = frame.width as f64 / max_width as f64;
    let new_w = max_width;
    let new_h = ((frame.height as f64) / scale).round().max(1.0) as u32;
    let src = RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .ok_or_else(|| RustSightError::Image("Invalid RGBA buffer dimensions".into()))?;
    let resized = image::imageops::resize(&src, new_w, new_h, ResizeFilter::Lanczos3);
    Ok((
        CapturedFrame {
            width: new_w,
            height: new_h,
            rgba: resized.into_raw(),
        },
        scale,
    ))
}

/// Encode a `CapturedFrame` (RGBA8) into high-compression PNG bytes.
pub fn encode_png(frame: &CapturedFrame) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive);
    encoder
        .write_image(
            &frame.rgba,
            frame.width,
            frame.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| RustSightError::Image(e.to_string()))?;
    Ok(buf)
}

/// Encode a `CapturedFrame` into a base64-encoded PNG string for MCP `Content::image`.
pub fn encode_png_base64(frame: &CapturedFrame) -> Result<String> {
    let png = encode_png(frame)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
}

/// Save a sequence of `CapturedFrame`s as an animated GIF on disk.
pub fn encode_gif(frames: &[CapturedFrame], delay_ms: u32, path: &Path) -> Result<()> {
    if frames.is_empty() {
        return Err(RustSightError::Image(
            "Cannot encode GIF from 0 frames".to_string(),
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    // Speed 2 gives crisp NeuQuant color quantization without banding.
    let mut encoder = GifEncoder::new_with_speed(writer, 2);
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|e| RustSightError::Image(e.to_string()))?;

    let delay = Delay::from_numer_denom_ms(delay_ms.max(10), 1);
    let target_w = frames[0].width;
    let target_h = frames[0].height;

    for f in frames {
        let img = if f.width == target_w && f.height == target_h {
            RgbaImage::from_raw(f.width, f.height, f.rgba.clone())
                .ok_or_else(|| RustSightError::Image("Invalid RGBA buffer dimensions".into()))?
        } else {
            let src = RgbaImage::from_raw(f.width, f.height, f.rgba.clone())
                .ok_or_else(|| RustSightError::Image("Invalid RGBA buffer dimensions".into()))?;
            image::imageops::resize(
                &src,
                target_w,
                target_h,
                image::imageops::FilterType::Triangle,
            )
        };
        encoder
            .encode_frame(Frame::from_parts(img, 0, 0, delay))
            .map_err(|e| RustSightError::Image(e.to_string()))?;
    }

    Ok(())
}

/// Returns `true` if two frames differ by more than `threshold_ratio` of pixels.
/// A pixel counts as changed if any R/G/B channel differs by more than `4`.
pub fn frames_differ(a: &CapturedFrame, b: &CapturedFrame, threshold_ratio: f64) -> bool {
    if a.width != b.width || a.height != b.height {
        return true;
    }
    let total_pixels = (a.width as usize) * (a.height as usize);
    if total_pixels == 0 {
        return false;
    }

    let mut changed = 0usize;
    for (pa, pb) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        let dr = pa[0].abs_diff(pb[0]);
        let dg = pa[1].abs_diff(pb[1]);
        let db = pa[2].abs_diff(pb[2]);
        if dr > 4 || dg > 4 || db > 4 {
            changed += 1;
        }
    }

    (changed as f64) / (total_pixels as f64) > threshold_ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_encoding_and_frame_diff() {
        let a = CapturedFrame {
            width: 4,
            height: 4,
            rgba: vec![100u8; 4 * 4 * 4],
        };
        let b = a.clone();
        assert!(!frames_differ(&a, &b, 0.001));

        let mut c = a.clone();
        c.rgba[0] = 255;
        assert!(frames_differ(&a, &c, 0.001));

        let png = encode_png(&a).expect("png encode");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
