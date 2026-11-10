//! Small previews of figures, decoded and scaled off the main thread, so a
//! folder of large photos costs a few hundred kilobytes, not their full
//! size, on the GPU.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::RenderImage;
use image::{Frame, ImageReader, RgbaImage};

/// Decodes the image at `path` and scales it to fit `max_side` pixels.
/// Blocking: run it on the background executor.
pub(crate) fn thumbnail(path: &Path, max_side: u32) -> anyhow::Result<Arc<RenderImage>> {
    let decoded = ImageReader::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .with_guessed_format()
        .with_context(|| format!("reading {}", path.display()))?
        .decode()
        .with_context(|| format!("decoding {}", path.display()))?;
    let scaled = if decoded.width() > max_side || decoded.height() > max_side {
        decoded.thumbnail(max_side, max_side)
    } else {
        decoded
    };
    let rgba = scaled.into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut pixels = rgba.into_raw();
    // GPUI's images are BGRA.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(width, height, pixels)
        .context("the thumbnail buffer has the wrong size")?;
    Ok(Arc::new(RenderImage::new([Frame::new(buffer)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_images_are_scaled_to_fit_and_swapped_to_bgra() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.png");
        let mut image = RgbaImage::new(800, 200);
        for pixel in image.pixels_mut() {
            *pixel = image::Rgba([10, 20, 30, 255]);
        }
        image.save(&path).unwrap();
        let thumb = thumbnail(&path, 400).unwrap();
        let size = thumb.size(0);
        assert_eq!((size.width.0, size.height.0), (400, 100));
        let bytes = thumb.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn unreadable_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(thumbnail(&path, 400).is_err());
        assert!(thumbnail(&dir.path().join("missing.png"), 400).is_err());
    }
}
