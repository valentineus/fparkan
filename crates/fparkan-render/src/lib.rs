#![forbid(unsafe_code)]
//! Camera mathematics and graphics pipeline state.

/// A 64-byte transform block returned by the original Terrain camera ABI.
///
/// The original uses two selector-dependent transform pointers.  Their exact
/// matrix convention is still under recovery, so words are deliberately kept
/// losslessly rather than treated as a renderer-ready matrix.  The three
/// translation words have been confirmed at indices 3, 7, and 11.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawCameraTransform {
    /// The exact 16 little-endian dwords returned by the legacy camera.
    pub words: [u32; 16],
}

impl RawCameraTransform {
    /// Indices of the confirmed X, Y, and Z translation floats.
    pub const TRANSLATION_WORD_INDICES: [usize; 3] = [3, 7, 11];

    /// Returns the confirmed legacy world position (X, Y, Z).
    #[must_use]
    pub fn translation(self) -> [f32; 3] {
        Self::TRANSLATION_WORD_INDICES.map(|index| f32::from_bits(self.words[index]))
    }

    /// Inverts a finite row-major affine transform without assigning it a
    /// camera-space meaning.
    ///
    /// The legacy SIMD dispatch multiplies these blocks as ordinary row-major
    /// matrices and the confirmed camera samples have translation in the last
    /// column. `Some` therefore means only that this block has a non-singular
    /// affine inverse. Callers must still establish whether it is a
    /// camera-to-world transform before using the result as a view matrix.
    #[must_use]
    pub fn try_inverse_affine_row_major(self) -> Option<[f32; 16]> {
        let matrix = self.words.map(f32::from_bits);
        if !matrix.iter().all(|value| value.is_finite())
            || matrix[12].abs() > f32::EPSILON
            || matrix[13].abs() > f32::EPSILON
            || matrix[14].abs() > f32::EPSILON
            || (matrix[15] - 1.0).abs() > f32::EPSILON
        {
            return None;
        }

        let [m00, m01, m02, _, m10, m11, m12, _, m20, m21, m22, _, _, _, _, _] = matrix;
        let cofactor00 = m11.mul_add(m22, -(m12 * m21));
        let cofactor01 = m02.mul_add(m21, -(m01 * m22));
        let cofactor02 = m01.mul_add(m12, -(m02 * m11));
        let determinant = m00.mul_add(cofactor00, m10.mul_add(cofactor01, m20 * cofactor02));
        if !determinant.is_finite() || determinant == 0.0 {
            return None;
        }

        let inverse_determinant = determinant.recip();
        let inverse = [
            cofactor00 * inverse_determinant,
            m02.mul_add(m21, -(m01 * m22)) * inverse_determinant,
            cofactor02 * inverse_determinant,
            0.0,
            m12.mul_add(m20, -(m10 * m22)) * inverse_determinant,
            m00.mul_add(m22, -(m02 * m20)) * inverse_determinant,
            m02.mul_add(m10, -(m00 * m12)) * inverse_determinant,
            0.0,
            m10.mul_add(m21, -(m11 * m20)) * inverse_determinant,
            m01.mul_add(m20, -(m00 * m21)) * inverse_determinant,
            m00.mul_add(m11, -(m01 * m10)) * inverse_determinant,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ];
        let [translation_x, translation_y, translation_z] = self.translation();
        let translation = [
            -(inverse[0].mul_add(
                translation_x,
                inverse[1].mul_add(translation_y, inverse[2] * translation_z),
            )),
            -(inverse[4].mul_add(
                translation_x,
                inverse[5].mul_add(translation_y, inverse[6] * translation_z),
            )),
            -(inverse[8].mul_add(
                translation_x,
                inverse[9].mul_add(translation_y, inverse[10] * translation_z),
            )),
        ];

        Some([
            inverse[0],
            inverse[1],
            inverse[2],
            translation[0],
            inverse[4],
            inverse[5],
            inverse[6],
            translation[1],
            inverse[8],
            inverse[9],
            inverse[10],
            translation[2],
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }

    /// Reproduces Ngi32's `Direct3D7` view-matrix conversion for this transform.
    ///
    /// The legacy renderer copies selector-0 into its camera state, then maps
    /// its axes and translation in this exact order before calling
    /// `IDirect3DDevice7::SetTransform(D3DTRANSFORMSTATE_VIEW, ...)`. This is
    /// deliberately distinct from [`Self::try_inverse_affine_row_major`]: it
    /// includes the original renderer's coordinate-system conversion.
    ///
    /// The returned matrix is row-major D3D7 data, not yet a Vulkan view
    /// matrix. A later adapter must explicitly account for clip-space and
    /// shader-vector conventions.
    #[must_use]
    pub fn try_direct3d7_view_row_major(self) -> Option<[f32; 16]> {
        let matrix = self.words.map(f32::from_bits);
        if !matrix.iter().all(|value| value.is_finite())
            || matrix[12].abs() > f32::EPSILON
            || matrix[13].abs() > f32::EPSILON
            || matrix[14].abs() > f32::EPSILON
            || (matrix[15] - 1.0).abs() > f32::EPSILON
        {
            return None;
        }

        let [m00, m01, m02, m03, m10, m11, m12, m13, m20, m21, m22, m23, _, _, _, _] = matrix;
        Some([
            -m01,
            m02,
            m00,
            0.0,
            -m11,
            m12,
            m10,
            0.0,
            -m21,
            m22,
            m20,
            0.0,
            m23.mul_add(m21, m13.mul_add(m11, m03 * m01)),
            -(m23.mul_add(m22, m13.mul_add(m12, m03 * m02))),
            -(m00.mul_add(m03, m23.mul_add(m20, m13 * m10))),
            1.0,
        ])
    }
}

/// Parameters consumed by Ngi32's `Direct3D7` projection-matrix builder.
///
/// These are a separate legacy-renderer boundary from
/// [`RawCameraProjection`]. The latter preserves Terrain's source ABI, while
/// this type records the already-resolved Ngi32 values submitted to `Direct3D7`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyD3d7Projection {
    /// Viewport rectangle as `(left, top, right, bottom)`.
    pub viewport: [i32; 4],
    /// Positive camera near-plane distance.
    pub near_plane: f32,
    /// Camera far-plane distance, greater than [`Self::near_plane`].
    pub far_plane: f32,
    /// Full field-of-view angle in radians.
    pub field_of_view_radians: f32,
}

impl LegacyD3d7Projection {
    /// Reconstructs the exact row-major matrix passed to D3D7 projection state.
    ///
    /// Ngi32 uses the viewport's `width / height`, writes `cos(fov / 2)` to
    /// the diagonal and `sin(fov / 2)` to both the depth scale and
    /// homogeneous-W term. The ratio after D3D's perspective divide is
    /// therefore the expected cotangent scale. This remains legacy D3D7 data
    /// rather than a Vulkan projection.
    #[must_use]
    pub fn try_direct3d7_projection_row_major(self) -> Option<[f32; 16]> {
        let width = self.viewport[2].checked_sub(self.viewport[0])?;
        let height = self.viewport[3].checked_sub(self.viewport[1])?;
        if width <= 0
            || height <= 0
            || !self.near_plane.is_finite()
            || !self.far_plane.is_finite()
            || !self.field_of_view_radians.is_finite()
            || self.near_plane <= 0.0
            || self.far_plane <= self.near_plane
            || self.field_of_view_radians <= 0.0
            || self.field_of_view_radians >= std::f32::consts::PI
        {
            return None;
        }

        // Ngi32 converts these signed viewport dimensions into single-precision
        // arithmetic before building the legacy matrix.

        let aspect = (width as f32) / (height as f32);
        let half_fov = self.field_of_view_radians * 0.5;
        let cosine = half_fov.cos();
        let sine = half_fov.sin();
        let depth_scale = sine / (1.0 - self.near_plane / self.far_plane);
        [aspect, cosine, sine, depth_scale]
            .iter()
            .all(|value| value.is_finite())
            .then_some([
                cosine,
                0.0,
                0.0,
                0.0,
                0.0,
                aspect * cosine,
                0.0,
                0.0,
                0.0,
                0.0,
                depth_scale,
                sine,
                0.0,
                0.0,
                -(depth_scale * self.near_plane),
                0.0,
            ])
    }
}

/// Affine placement inputs consumed by `Iron3D`'s recovered Euler-matrix builder.
///
/// This records the exact matrix construction at `iron3d.dll` RVA `0x36610`.
/// It is intentionally distinct from a mission placement contract: the static
/// trace has not yet proved that TMA's three raw orientation floats reach this
/// builder unchanged for every object category.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyIron3dEulerTransform {
    /// World-space translation passed to the builder.
    pub translation: [f32; 3],
    /// Three angles in the builder's `(x, y, z)` input order, in radians.
    pub orientation_radians: [f32; 3],
}

impl LegacyIron3dEulerTransform {
    /// Reconstructs the builder's finite row-major affine matrix.
    ///
    /// The recovered x87 code evaluates `Rz(z) * Ry(y) * Rx(x)` and writes
    /// translation into the last column. This uses portable `f32` trigonometry;
    /// any future x87-compatibility path must be separately capture-validated.
    #[must_use]
    pub fn try_row_major(self) -> Option<[f32; 16]> {
        if !self
            .translation
            .iter()
            .chain(self.orientation_radians.iter())
            .all(|value| value.is_finite())
        {
            return None;
        }
        let [x, y, z] = self.orientation_radians;
        let (sin_x, cos_x) = x.sin_cos();
        let (sin_y, cos_y) = y.sin_cos();
        let (sin_z, cos_z) = z.sin_cos();
        let [translation_x, translation_y, translation_z] = self.translation;
        let matrix = [
            cos_z * cos_y,
            cos_z * sin_y * sin_x - sin_z * cos_x,
            cos_z * sin_y * cos_x + sin_z * sin_x,
            translation_x,
            sin_z * cos_y,
            sin_z * sin_y * sin_x + cos_z * cos_x,
            sin_z * sin_y * cos_x - cos_z * sin_x,
            translation_y,
            -sin_y,
            cos_y * sin_x,
            cos_y * cos_x,
            translation_z,
            0.0,
            0.0,
            0.0,
            1.0,
        ];
        matrix
            .iter()
            .all(|value| value.is_finite())
            .then_some(matrix)
    }

    /// Applies the recovered rotation after a component-wise local scale.
    ///
    /// This is the affine `R * (scale * point) + translation` convention used
    /// by the static source-world bridge. The original dynamic transform path
    /// needs its own capture evidence before it may replace this static path.
    #[must_use]
    pub fn try_transform_scaled_point(self, point: [f32; 3], scale: [f32; 3]) -> Option<[f32; 3]> {
        if !point
            .iter()
            .chain(scale.iter())
            .all(|value| value.is_finite())
        {
            return None;
        }
        let matrix = self.try_row_major()?;
        let scaled = [
            point[0] * scale[0],
            point[1] * scale[1],
            point[2] * scale[2],
        ];
        let transformed = [
            matrix[0] * scaled[0] + matrix[1] * scaled[1] + matrix[2] * scaled[2] + matrix[3],
            matrix[4] * scaled[0] + matrix[5] * scaled[1] + matrix[6] * scaled[2] + matrix[7],
            matrix[8] * scaled[0] + matrix[9] * scaled[1] + matrix[10] * scaled[2] + matrix[11],
        ];
        transformed
            .iter()
            .all(|value| value.is_finite())
            .then_some(transformed)
    }
}

/// Fixed-function blend behaviour represented without a graphics API type.
///
/// This is a compatibility contract, not yet a decoded MAT0 mapping.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LegacyBlendMode {
    /// Do not blend the fragment with the existing colour.
    #[default]
    Opaque,
    /// Blend using source alpha.
    SourceAlpha,
}

/// Depth-buffer behaviour represented without a graphics API type.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LegacyDepthMode {
    /// No depth attachment is used.
    #[default]
    Disabled,
    /// Test depth and write passing fragments.
    TestWrite,
    /// Test depth without modifying it.
    TestReadOnly,
}

/// Triangle culling behaviour represented without a graphics API type.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LegacyCullMode {
    /// Keep both front- and back-facing triangles.
    #[default]
    Disabled,
    /// Cull back-facing triangles.
    BackFace,
    /// Cull front-facing triangles.
    FrontFace,
}

/// Legacy fixed-function state that changes graphics-pipeline structure.
///
/// Alpha reference is deliberately not present: it is dynamic material data,
/// whereas this state records only whether an alpha-test shader variant is used.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LegacyPipelineState {
    /// Colour blend mode.
    pub blend: LegacyBlendMode,
    /// Depth test/write mode.
    pub depth: LegacyDepthMode,
    /// Face culling mode.
    pub cull: LegacyCullMode,
    /// Whether alpha-test shader logic is enabled.
    pub alpha_test: bool,
}

/// Canonical, backend-neutral key for a graphics-pipeline variant.
///
/// The value is explicitly packed rather than hashed, so captures and caches
/// remain stable across processes and Rust toolchain updates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PipelineKey(u8);

impl PipelineKey {
    /// Returns the canonical packed representation.
    #[must_use]
    pub const fn packed(self) -> u8 {
        self.0
    }
}

impl From<LegacyPipelineState> for PipelineKey {
    fn from(state: LegacyPipelineState) -> Self {
        let blend = match state.blend {
            LegacyBlendMode::Opaque => 0,
            LegacyBlendMode::SourceAlpha => 1,
        };
        let depth = match state.depth {
            LegacyDepthMode::Disabled => 0,
            LegacyDepthMode::TestWrite => 1,
            LegacyDepthMode::TestReadOnly => 2,
        };
        let cull = match state.cull {
            LegacyCullMode::Disabled => 0,
            LegacyCullMode::BackFace => 1,
            LegacyCullMode::FrontFace => 2,
        };
        Self(blend | (depth << 1) | (cull << 3) | (u8::from(state.alpha_test) << 5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multiply_row_major(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
        let mut result = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                result[row * 4 + column] = (0..4)
                    .map(|index| left[row * 4 + index] * right[index * 4 + column])
                    .sum();
            }
        }
        result
    }

    fn assert_matrix_approximately_identity(matrix: [f32; 16]) {
        for (index, value) in matrix.into_iter().enumerate() {
            let expected = if index / 4 == index % 4 { 1.0 } else { 0.0 };
            assert!(
                (value - expected).abs() < 0.000_02,
                "matrix element {index}: expected {expected}, got {value}"
            );
        }
    }

    #[test]
    fn raw_camera_transform_inverts_only_non_singular_affine_blocks() {
        let source = [
            0.0, -1.0, 0.0, 433.544_7, 0.948_985, 0.0, 0.315_322, 652.292_5, -0.315_322, 0.0,
            0.948_985, 10.673_42, 0.0, 0.0, 0.0, 1.0,
        ];
        let transform = RawCameraTransform {
            words: source.map(f32::to_bits),
        };
        let inverse = transform
            .try_inverse_affine_row_major()
            .expect("observed affine transform is invertible");

        assert_matrix_approximately_identity(multiply_row_major(source, inverse));
        assert_matrix_approximately_identity(multiply_row_major(inverse, source));

        let singular = RawCameraTransform { words: [0_u32; 16] };
        assert_eq!(singular.try_inverse_affine_row_major(), None);
    }

    #[test]
    fn raw_camera_transform_reproduces_direct3d7_view_axis_conversion() {
        let transform = RawCameraTransform {
            words: [
                0.0_f32.to_bits(),
                (-1.0_f32).to_bits(),
                0.0_f32.to_bits(),
                10.0_f32.to_bits(),
                1.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                20.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
                30.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
            ],
        };

        assert_eq!(
            transform.try_direct3d7_view_row_major(),
            Some([
                1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, -10.0, -30.0, -20.0,
                1.0,
            ])
        );
        assert_eq!(
            RawCameraTransform { words: [0_u32; 16] }.try_direct3d7_view_row_major(),
            None
        );
    }

    #[test]
    fn legacy_d3d7_projection_matches_recovered_camera_formula() {
        let projection = LegacyD3d7Projection {
            viewport: [0, 0, 1024, 768],
            near_plane: 0.5,
            far_plane: 700.0,
            field_of_view_radians: 1.3,
        };
        let matrix = projection
            .try_direct3d7_projection_row_major()
            .expect("live Ngi32 projection parameters are valid");
        let half_fov = 0.65_f32;
        let depth_scale = half_fov.sin() * 700.0_f32 / 699.5;

        assert_eq!(matrix[0], half_fov.cos());
        assert_eq!(matrix[5], (4.0 / 3.0) * half_fov.cos());
        assert_eq!(matrix[10], depth_scale);
        assert_eq!(matrix[11], half_fov.sin());
        assert_eq!(matrix[14], -(depth_scale * 0.5));
        assert_eq!(
            LegacyD3d7Projection {
                viewport: [0, 0, 0, 768],
                ..projection
            }
            .try_direct3d7_projection_row_major(),
            None
        );
    }

    #[test]
    fn iron3d_euler_builder_uses_rz_ry_rx_and_last_column_translation() {
        let matrix = LegacyIron3dEulerTransform {
            translation: [418.103_18, 717.433, 3.040_938_9],
            orientation_radians: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
        }
        .try_row_major()
        .expect("finite recovered builder inputs");

        assert!((matrix[0]).abs() < 0.000_001);
        assert!((matrix[1] + 1.0).abs() < 0.000_001);
        assert!((matrix[4] - 1.0).abs() < 0.000_001);
        assert!((matrix[5]).abs() < 0.000_001);
        assert_eq!(matrix[3], 418.103_18);
        assert_eq!(matrix[7], 717.433);
        assert_eq!(matrix[11], 3.040_938_9);
        assert_eq!(matrix[15], 1.0);
        assert_eq!(
            LegacyIron3dEulerTransform {
                translation: [10.0, 20.0, 30.0],
                orientation_radians: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
            }
            .try_transform_scaled_point([2.0, 3.0, 4.0], [2.0, 1.0, 0.5]),
            Some([7.0, 24.0, 32.0])
        );
        assert_eq!(
            LegacyIron3dEulerTransform {
                translation: [0.0, 0.0, 0.0],
                orientation_radians: [f32::NAN, 0.0, 0.0],
            }
            .try_row_major(),
            None
        );
    }
}
