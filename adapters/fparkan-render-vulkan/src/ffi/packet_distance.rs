#![allow(unsafe_code)]
//! The native CPU instruction used for render-packet distance keys.

/// Reproduces Ngi32's squared length multiplied by `RSQRTSS` on x86-64.
/// Other architectures retain the portable distance and report that boundary.
#[must_use]
pub fn legacy_packet_distance_squared(squared: f32) -> Option<f32> {
    if !squared.is_finite() || squared < 0.0 {
        return None;
    }
    if squared == 0.0 {
        static NOTICE: std::sync::Once = std::sync::Once::new();
        NOTICE.call_once(|| {
            eprintln!("render_coverage packet_zero_distance_fallback=1 (stable zero instead of native 0 * infinity)");
        });
        return Some(0.0);
    }
    #[cfg(target_arch = "x86_64")]
    let distance = {
        use core::arch::x86_64::{_mm_cvtss_f32, _mm_rsqrt_ss, _mm_set_ss};
        // SAFETY: SSE is part of the x86-64 baseline; these intrinsics only
        // operate on register values and do not access memory.
        let inverse = unsafe { _mm_cvtss_f32(_mm_rsqrt_ss(_mm_set_ss(squared))) };
        squared * inverse
    };
    #[cfg(not(target_arch = "x86_64"))]
    let distance = {
        static NOTICE: std::sync::Once = std::sync::Once::new();
        NOTICE.call_once(|| {
            eprintln!("render_coverage packet_distance_native_rsqrt_unsupported=1 (using portable square root)");
        });
        squared.sqrt()
    };
    distance.is_finite().then_some(distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_distance_preserves_the_native_approximation_error_bound() {
        for squared in [0.000_001_f32, 1.0, 9.0, 11_393.854, 1_000_000.0] {
            let distance = legacy_packet_distance_squared(squared).unwrap();
            assert!((distance / squared.sqrt() - 1.0).abs() < 0.000_3);
        }
        assert_eq!(legacy_packet_distance_squared(0.0), Some(0.0));
        for invalid in [-1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(legacy_packet_distance_squared(invalid), None);
        }
    }
}
