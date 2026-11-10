//! Shared rasterization helpers: scale validation, bitmap limits and the
//! conversion to the contract's pixel format (opaque, straight RGBA8).

use anyhow::{Result, bail};

/// Largest bitmap side we agree to allocate, in pixels.
pub(crate) const MAX_SIDE: u32 = 16_384;
/// Largest bitmap we agree to allocate, in pixels (256 MiB of RGBA).
pub(crate) const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// The pixel size of a page of `width_pt` × `height_pt` at `pixels_per_pt`,
/// rounded to the nearest pixel (at least one), or an error when the scale is
/// not usable or the bitmap would be unreasonably large.
pub(crate) fn pixel_size(width_pt: f32, height_pt: f32, pixels_per_pt: f32) -> Result<(u32, u32)> {
    if !pixels_per_pt.is_finite() || pixels_per_pt <= 0.0 {
        bail!("invalid render scale {pixels_per_pt}");
    }
    let width = (f64::from(width_pt) * f64::from(pixels_per_pt))
        .round()
        .max(1.0);
    let height = (f64::from(height_pt) * f64::from(pixels_per_pt))
        .round()
        .max(1.0);
    if !width.is_finite() || !height.is_finite() {
        bail!("invalid page size {width_pt}×{height_pt} pt");
    }
    if width > f64::from(MAX_SIDE)
        || height > f64::from(MAX_SIDE)
        || width * height > MAX_PIXELS as f64
    {
        bail!(
            "a {width}×{height} px bitmap is too large; render at a smaller scale \
             (at most {MAX_SIDE} px per side)"
        );
    }
    Ok((width as u32, height as u32))
}

/// Turns premultiplied RGBA8 into opaque straight RGBA8 by compositing it
/// onto white paper. Fully opaque pixels are left untouched.
pub(crate) fn premultiplied_onto_white(mut data: Vec<u8>) -> Vec<u8> {
    for pixel in data.as_chunks_mut::<4>().0 {
        let alpha = pixel[3];
        if alpha != u8::MAX {
            // Premultiplied `c` over white: c + 255 · (1 − a/255) = c + 255 − a.
            let white = u8::MAX - alpha;
            pixel[0] = pixel[0].saturating_add(white);
            pixel[1] = pixel[1].saturating_add(white);
            pixel[2] = pixel[2].saturating_add(white);
            pixel[3] = u8::MAX;
        }
    }
    data
}

/// The human-readable message of a caught panic.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_size_rounds_and_validates() {
        assert_eq!(pixel_size(595.28, 841.89, 1.0).unwrap(), (595, 842));
        assert_eq!(pixel_size(595.28, 841.89, 2.0).unwrap(), (1191, 1684));
        assert_eq!(pixel_size(0.1, 0.1, 1.0).unwrap(), (1, 1));
        assert!(pixel_size(595.0, 842.0, 0.0).is_err());
        assert!(pixel_size(595.0, 842.0, f32::NAN).is_err());
        assert!(pixel_size(595.0, 842.0, 1000.0).is_err());
    }

    #[test]
    fn composites_onto_white() {
        // Transparent, half-covered black, opaque red.
        let data = vec![0, 0, 0, 0, 0, 0, 0, 128, 255, 0, 0, 255];
        let out = premultiplied_onto_white(data);
        assert_eq!(
            out,
            vec![255, 255, 255, 255, 127, 127, 127, 255, 255, 0, 0, 255]
        );
    }
}
