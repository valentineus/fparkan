use image::{ImageBuffer, Rgba, RgbaImage};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ManifestMeta {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub lod: Option<usize>,
    pub group: Option<usize>,
    pub angle: Option<f32>,
    pub diff_threshold: Option<u8>,
    pub max_mean_abs: Option<f32>,
    pub max_changed_ratio: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaseSpec {
    pub id: String,
    pub archive: String,
    pub model: Option<String>,
    pub reference: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub lod: Option<usize>,
    pub group: Option<usize>,
    pub angle: Option<f32>,
    pub diff_threshold: Option<u8>,
    pub max_mean_abs: Option<f32>,
    pub max_changed_ratio: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParityManifest {
    #[serde(default)]
    pub meta: ManifestMeta,
    #[serde(rename = "case", default)]
    pub cases: Vec<CaseSpec>,
}

#[derive(Debug, Clone)]
pub struct DiffMetrics {
    pub width: u32,
    pub height: u32,
    pub mean_abs: f32,
    pub max_abs: u8,
    pub changed_pixels: u64,
    pub changed_ratio: f32,
}

pub fn compare_images(
    reference: &RgbaImage,
    actual: &RgbaImage,
    diff_threshold: u8,
) -> Result<DiffMetrics, String> {
    let (rw, rh) = reference.dimensions();
    let (aw, ah) = actual.dimensions();
    if rw != aw || rh != ah {
        return Err(format!(
            "image size mismatch: reference={}x{}, actual={}x{}",
            rw, rh, aw, ah
        ));
    }

    let mut diff_sum = 0u64;
    let mut max_abs = 0u8;
    let mut changed_pixels = 0u64;
    let pixel_count = u64::from(rw).saturating_mul(u64::from(rh));

    for (ref_px, act_px) in reference.pixels().zip(actual.pixels()) {
        let mut pixel_changed = false;
        for chan in 0..3 {
            let a = i16::from(ref_px[chan]);
            let b = i16::from(act_px[chan]);
            let diff = (a - b).unsigned_abs() as u8;
            diff_sum = diff_sum.saturating_add(u64::from(diff));
            if diff > max_abs {
                max_abs = diff;
            }
            if diff > diff_threshold {
                pixel_changed = true;
            }
        }
        if pixel_changed {
            changed_pixels = changed_pixels.saturating_add(1);
        }
    }

    let channels = pixel_count.saturating_mul(3);
    let mean_abs = if channels == 0 {
        0.0
    } else {
        diff_sum as f32 / channels as f32
    };
    let changed_ratio = if pixel_count == 0 {
        0.0
    } else {
        changed_pixels as f32 / pixel_count as f32
    };

    Ok(DiffMetrics {
        width: rw,
        height: rh,
        mean_abs,
        max_abs,
        changed_pixels,
        changed_ratio,
    })
}

pub fn build_diff_image(reference: &RgbaImage, actual: &RgbaImage) -> Result<RgbaImage, String> {
    let (rw, rh) = reference.dimensions();
    let (aw, ah) = actual.dimensions();
    if rw != aw || rh != ah {
        return Err(format!(
            "image size mismatch: reference={}x{}, actual={}x{}",
            rw, rh, aw, ah
        ));
    }

    let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(rw, rh);
    for (dst, (ref_px, act_px)) in out
        .pixels_mut()
        .zip(reference.pixels().zip(actual.pixels()))
    {
        let dr = (i16::from(ref_px[0]) - i16::from(act_px[0])).unsigned_abs() as u8;
        let dg = (i16::from(ref_px[1]) - i16::from(act_px[1])).unsigned_abs() as u8;
        let db = (i16::from(ref_px[2]) - i16::from(act_px[2])).unsigned_abs() as u8;
        *dst = Rgba([dr, dg, db, 255]);
    }
    Ok(out)
}

pub fn evaluate_metrics(
    metrics: &DiffMetrics,
    max_mean_abs: f32,
    max_changed_ratio: f32,
) -> Vec<String> {
    let mut violations = Vec::new();
    if metrics.mean_abs > max_mean_abs {
        violations.push(format!(
            "mean_abs {:.4} > allowed {:.4}",
            metrics.mean_abs, max_mean_abs
        ));
    }
    if metrics.changed_ratio > max_changed_ratio {
        violations.push(format!(
            "changed_ratio {:.4}% > allowed {:.4}%",
            metrics.changed_ratio * 100.0,
            max_changed_ratio * 100.0
        ));
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, r: u8, g: u8, b: u8) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = Rgba([r, g, b, 255]);
        }
        img
    }

    #[test]
    fn compare_identical_images() {
        let ref_img = solid(4, 3, 10, 20, 30);
        let act_img = solid(4, 3, 10, 20, 30);
        let metrics = compare_images(&ref_img, &act_img, 2).expect("comparison must succeed");
        assert_eq!(metrics.width, 4);
        assert_eq!(metrics.height, 3);
        assert_eq!(metrics.max_abs, 0);
        assert_eq!(metrics.changed_pixels, 0);
        assert_eq!(metrics.mean_abs, 0.0);
        assert_eq!(metrics.changed_ratio, 0.0);
    }

    #[test]
    fn compare_detects_changes_and_thresholds() {
        let mut ref_img = solid(2, 2, 100, 100, 100);
        let mut act_img = solid(2, 2, 100, 100, 100);
        ref_img.put_pixel(1, 1, Rgba([120, 100, 100, 255]));
        act_img.put_pixel(1, 1, Rgba([100, 100, 100, 255]));

        let metrics = compare_images(&ref_img, &act_img, 5).expect("comparison must succeed");
        assert_eq!(metrics.max_abs, 20);
        assert_eq!(metrics.changed_pixels, 1);
        assert!((metrics.changed_ratio - 0.25).abs() < 1e-6);
        assert!(metrics.mean_abs > 0.0);

        let violations = evaluate_metrics(&metrics, 2.0, 0.20);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("changed_ratio"));
    }

    #[test]
    fn build_diff_image_returns_per_channel_abs_diff() {
        let mut ref_img = solid(1, 1, 100, 150, 200);
        let mut act_img = solid(1, 1, 90, 180, 170);
        ref_img.put_pixel(0, 0, Rgba([100, 150, 200, 255]));
        act_img.put_pixel(0, 0, Rgba([90, 180, 170, 255]));

        let diff = build_diff_image(&ref_img, &act_img).expect("diff image must build");
        let px = diff.get_pixel(0, 0);
        assert_eq!(px[0], 10);
        assert_eq!(px[1], 30);
        assert_eq!(px[2], 30);
        assert_eq!(px[3], 255);
    }
}
