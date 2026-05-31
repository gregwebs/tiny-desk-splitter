//! Image-processing helpers: grayscale / black-and-white conversion used to
//! preprocess frames for OCR, and frame darkness analysis.
//!
//! NOTE: this module shares its name with the `image` crate. The crate is
//! referred to here with a leading `::` (`::image`); the module is reached from
//! elsewhere in the binary as `crate::image`.

use ::image::{DynamicImage, GrayImage, Luma};
use anyhow::{Context, Result};
use std::path::Path;

/// `magick ... -threshold 55%`: a pixel becomes white when its gray value exceeds
/// 55% of the quantum range. We replicate magick's Q16 pipeline (see below), so the
/// cutoff is 0.55 * 65535 = 36044.25 — a 16-bit gray > 36044.25 (i.e. >= 36045)
/// becomes white. Verified by sweeping magick's actual flip point.
const BW_THRESHOLD_FRACTION: f64 = 0.55;
const Q16_MAX: f64 = u16::MAX as f64; // 65535: ImageMagick's Q16 quantum range

/// Rec.709 / sRGB luminance coefficients, matching ImageMagick's `-colorspace gray`.
/// NOTE: these are deliberately more precise than the image crate's `to_luma8`
/// coefficients (0.2126/0.7152/0.0722). The extra digits, plus quantizing to 16-bit
/// before thresholding, are what make the binarized output (and therefore
/// tesseract's OCR) match the previous `magick` pipeline. The coarser coefficients
/// of a truncating `to_luma8` shifted overlay detection by a frame on some concerts.
const LUMA_R: f64 = 0.212656;
const LUMA_G: f64 = 0.715158;
const LUMA_B: f64 = 0.072186;

/// Reproduce `magick -colorspace gray -channel rgb -threshold 55% +channel`:
/// Rec.709 grayscale, scaled to magick's 16-bit (Q16) quantum and rounded the way
/// magick does, then a hard 55% threshold to pure black/white. Tuned for tesseract
/// overlay OCR — this matches magick's output to within a few boundary pixels per
/// frame (luma within ~1 Q16 step of the cutoff), which OCR is insensitive to.
pub fn to_black_and_white(img: &DynamicImage) -> GrayImage {
    let cutoff = BW_THRESHOLD_FRACTION * Q16_MAX; // 36044.25
    let rgb = img.to_rgb8();
    let mut out = GrayImage::new(rgb.width(), rgb.height());
    for (x, y, px) in rgb.enumerate_pixels() {
        let [r, g, b] = px.0;
        // Rec.709 luma in 0..=255, then scale to Q16 and round as magick does.
        let luma = LUMA_R * r as f64 + LUMA_G * g as f64 + LUMA_B * b as f64;
        let gray_q16 = (luma / u8::MAX as f64 * Q16_MAX).round();
        let value = if gray_q16 > cutoff { u8::MAX } else { 0 };
        out.put_pixel(x, y, Luma([value]));
    }
    out
}

/// Read `src`, threshold to B/W, write to `dst` (format inferred from extension).
pub fn write_black_and_white(src: &Path, dst: &Path) -> Result<()> {
    let img = ::image::open(src)
        .with_context(|| format!("opening frame for B/W conversion: {}", src.display()))?;
    to_black_and_white(&img)
        .save(dst)
        .with_context(|| format!("writing B/W frame: {}", dst.display()))?;
    Ok(())
}

/// Fraction of samples at or below `threshold` brightness (0-255) in raw image
/// bytes — i.e. how "dark" a frame is, used to find (near-)black frames at scene
/// boundaries. Each byte is treated as one channel sample, so an all-black RGB
/// frame and an all-black grayscale frame both report ~1.0.
pub fn grayscale_darkness(frame_data: &[u8], threshold: u8) -> f64 {
    let pixel_count = frame_data.len();
    if pixel_count == 0 {
        return 0.0;
    }

    let dark_pixels = frame_data
        .iter()
        .filter(|&&pixel| pixel <= threshold)
        .count();
    dark_pixels as f64 / pixel_count as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{DynamicImage, Rgb, RgbImage};

    /// Build a 1x1 RGB image of a single color and return its B/W luma value.
    fn bw_of(r: u8, g: u8, b: u8) -> u8 {
        let mut img = RgbImage::new(1, 1);
        img.put_pixel(0, 0, Rgb([r, g, b]));
        let bw = to_black_and_white(&DynamicImage::ImageRgb8(img));
        bw.get_pixel(0, 0)[0]
    }

    #[test]
    fn threshold_boundary_around_55_percent() {
        // 0.55 * 255 = 140.25, so gray 140 -> black and gray 141 -> white.
        assert_eq!(bw_of(140, 140, 140), 0, "gray 140 should be black");
        assert_eq!(bw_of(141, 141, 141), 255, "gray 141 should be white");
    }

    #[test]
    fn pure_black_and_white_pass_through() {
        assert_eq!(bw_of(0, 0, 0), 0);
        assert_eq!(bw_of(255, 255, 255), 255);
    }

    #[test]
    fn rec709_luma_straddles_cutoff_per_channel() {
        // Rec.709 luma: red ~54 (black), green ~182 (white), blue ~18 (black).
        assert_eq!(bw_of(255, 0, 0), 0, "pure red luma ~54 -> black");
        assert_eq!(bw_of(0, 255, 0), 255, "pure green luma ~182 -> white");
        assert_eq!(bw_of(0, 0, 255), 0, "pure blue luma ~18 -> black");
    }

    #[test]
    fn output_is_binary_grayscale() {
        let mut img = RgbImage::new(4, 4);
        for (i, px) in img.pixels_mut().enumerate() {
            let v = (i * 16) as u8; // 0..=240 spread across pixels
            *px = Rgb([v, v, v]);
        }
        let bw = to_black_and_white(&DynamicImage::ImageRgb8(img));
        assert_eq!(bw.dimensions(), (4, 4));
        for px in bw.pixels() {
            assert!(px[0] == 0 || px[0] == 255, "value {} not binary", px[0]);
        }
    }

    #[test]
    fn grayscale_darkness_fraction() {
        assert_eq!(grayscale_darkness(&[], 25), 0.0, "empty -> 0");
        assert_eq!(grayscale_darkness(&[0, 0, 0, 0], 25), 1.0, "all dark");
        assert_eq!(grayscale_darkness(&[255, 255], 25), 0.0, "all bright");
        assert_eq!(grayscale_darkness(&[0, 255, 0, 255], 25), 0.5);
        // threshold is inclusive (<=).
        assert_eq!(grayscale_darkness(&[25, 26], 25), 0.5);
    }
}
