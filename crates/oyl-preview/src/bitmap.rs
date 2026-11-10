//! Page bitmaps: rasterizing a page off the main thread and turning the
//! result into a GPU-ready image.

use std::sync::Arc;

use anyhow::{Context as _, ensure};
use gpui_kit::{RenderImage, Size, size};
use image::{Frame, RgbaImage};
use oyl_model::typeset::{PageImage, RenderedDocument};

use crate::raster::{HeldBitmap, RasterKey, rgba_to_bgra};

/// One rasterized page, ready to paint.
#[derive(Clone)]
pub(crate) struct PageBitmap {
    pub image: Arc<RenderImage>,
    pub fingerprint: u64,
    pub pixels_per_pt: f32,
    pub width_px: u32,
    pub height_px: u32,
}

impl PageBitmap {
    /// Bytes of pixel data held.
    pub fn bytes(&self) -> usize {
        self.width_px as usize * self.height_px as usize * 4
    }

    /// What the job planner needs to know about it.
    pub fn held(&self) -> HeldBitmap {
        HeldBitmap {
            fingerprint: self.fingerprint,
            pixels_per_pt: self.pixels_per_pt,
        }
    }

    /// The area it covers on its page, in points (its pixel size at its
    /// own resolution).
    pub fn size_pt(&self) -> Size<f32> {
        size(
            self.width_px as f32 / self.pixels_per_pt,
            self.height_px as f32 / self.pixels_per_pt,
        )
    }
}

/// Rasterizes one page. Blocking and CPU-bound: run it on the background
/// executor.
pub(crate) fn rasterize(
    document: &dyn RenderedDocument,
    key: RasterKey,
) -> anyhow::Result<PageBitmap> {
    let page = document
        .render_page(key.page, key.pixels_per_pt)
        .with_context(|| format!("rendering page {}", key.page + 1))?;
    let (width_px, height_px) = (page.width, page.height);
    let frame = to_frame(page)?;
    Ok(PageBitmap {
        image: Arc::new(RenderImage::new([frame])),
        fingerprint: key.fingerprint,
        pixels_per_pt: key.pixels_per_pt,
        width_px,
        height_px,
    })
}

/// Checks a page image and converts its RGBA pixels into a BGRA frame.
pub(crate) fn to_frame(page: PageImage) -> anyhow::Result<Frame> {
    let PageImage {
        width,
        height,
        mut rgba,
    } = page;
    ensure!(
        width > 0 && height > 0,
        "the page image is empty ({width}×{height})"
    );
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .context("the page image is too large")?;
    ensure!(
        rgba.len() == expected,
        "the page image has {} bytes, expected {expected} for {width}×{height}",
        rgba.len()
    );
    rgba_to_bgra(&mut rgba);
    let buffer =
        RgbaImage::from_raw(width, height, rgba).context("the page image buffer is malformed")?;
    Ok(Frame::new(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_bgra_and_sized() {
        let page = PageImage {
            width: 2,
            height: 1,
            rgba: vec![10, 20, 30, 255, 40, 50, 60, 255],
        };
        let frame = to_frame(page).unwrap();
        assert_eq!(frame.buffer().dimensions(), (2, 1));
        assert_eq!(
            frame.buffer().as_raw(),
            &vec![30, 20, 10, 255, 60, 50, 40, 255]
        );
    }

    #[test]
    fn malformed_images_are_rejected() {
        let short = PageImage {
            width: 2,
            height: 2,
            rgba: vec![0; 15],
        };
        assert!(to_frame(short).is_err());
        let empty = PageImage {
            width: 0,
            height: 4,
            rgba: Vec::new(),
        };
        assert!(to_frame(empty).is_err());
    }

    #[test]
    fn bitmap_reports_bytes_and_point_size() {
        let frame = to_frame(PageImage {
            width: 4,
            height: 6,
            rgba: vec![255; 96],
        })
        .unwrap();
        let bitmap = PageBitmap {
            image: Arc::new(RenderImage::new([frame])),
            fingerprint: 9,
            pixels_per_pt: 2.,
            width_px: 4,
            height_px: 6,
        };
        assert_eq!(bitmap.bytes(), 96);
        assert_eq!(bitmap.size_pt(), size(2., 3.));
        assert_eq!(bitmap.held().fingerprint, 9);
    }
}
