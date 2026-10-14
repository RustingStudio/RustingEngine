//! Bit-exact Rust reference of `src/shaders/sim_math.glsl`, the shared
//! simulation math described in `docs/determinism.md`. Every routine uses
//! only correctly rounded `+`, `-`, `*` and integer bit operations in a
//! fixed order, and Rust never fuses them, so each returns the same bits on
//! every CPU and matches the GLSL version on every GPU that honours
//! `precise`. Change both files together.
//!
//! The transcendental functions ([`sin_cos`], [`atan2`], [`asin`]) and the
//! rotation helpers built on them exist only here, since no simulation
//! shader needs them yet. They also use Rust's `/` and `sqrt`, which are
//! correctly rounded on every supported CPU.

use nalgebra::{Matrix3, Quaternion, Rotation3, UnitQuaternion, Vector3};

/// Of `y - 1 ULP`, `y`, and `y + 1 ULP`, the first with the smallest
/// `residual`. Newton steps approach from below and stop an ULP short of
/// round values such as `1 / 1`.
fn closest(y: f32, residual: impl Fn(f32) -> f32) -> f32 {
    let mut best = y;
    let mut best_residual = residual(y);
    for candidate in [
        f32::from_bits(y.to_bits().wrapping_sub(1)),
        f32::from_bits(y.to_bits().wrapping_add(1)),
    ] {
        let candidate_residual = residual(candidate);
        if candidate_residual < best_residual {
            best = candidate;
            best_residual = candidate_residual;
        }
    }
    best
}

/// `1 / x` for finite, normal, non-zero `x`, within 1 ULP.
#[must_use]
pub fn recip(x: f32) -> f32 {
    let magnitude = x.abs();
    let mut y =
        f32::from_bits(0x7EF3_11C3_u32.wrapping_sub(magnitude.to_bits()));
    for _ in 0..3 {
        y *= 2.0 - magnitude * y;
    }
    let y = closest(y, |candidate| (1.0 - magnitude * candidate).abs());
    if x < 0.0 {
        -y
    } else {
        y
    }
}

#[must_use]
pub fn div(a: f32, b: f32) -> f32 {
    a * recip(b)
}

/// `1 / sqrt(x)` for finite, normal, positive `x`, within 2 ULP.
#[must_use]
pub fn rsqrt(x: f32) -> f32 {
    let half_x = 0.5 * x;
    let mut y = f32::from_bits(0x5F37_5A86_u32.wrapping_sub(x.to_bits() >> 1));
    for _ in 0..3 {
        y *= 1.5 - half_x * y * y;
    }
    closest(y, |candidate| (1.0 - x * candidate * candidate).abs())
}

/// `sqrt(x)`; 0 for `x <= 0`.
#[must_use]
pub fn sqrt(x: f32) -> f32 {
    if x <= 0.0 {
        0.0
    } else {
        let root = x * rsqrt(x);
        closest(root, |candidate| (candidate * candidate - x).abs())
    }
}

/// `x` truncated toward zero, saturated to the `i32` range, NaN to 0.
/// This is Rust's `as i32`; `sim_to_int` in GLSL does the same, where a
/// plain `int()` is undefined out of range.
#[must_use]
pub fn to_int(x: f32) -> i32 {
    x as i32
}

#[must_use]
pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[must_use]
pub fn length(v: [f32; 3]) -> f32 {
    sqrt(dot(v, v))
}

/// `v` scaled to length 1; the zero vector stays zero.
#[must_use]
pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let square = dot(v, v);
    if square <= 0.0 {
        return [0.0; 3];
    }
    let scale = rsqrt(square);
    v.map(|component| component * scale)
}

/// `(sin(x), cos(x))`. Cephes `sinf`/`cosf`: octant reduction by pi/4 in
/// three parts, then minimax polynomials, within 2 ULP of the true value
/// for `|x| <= 8192`. Larger arguments first reduce by an `f32` 2 pi with
/// the exact `%`, which loses accuracy but still gives the same bits
/// everywhere.
#[must_use]
pub fn sin_cos(x: f32) -> (f32, f32) {
    const FOUR_OVER_PI: f32 = 1.273_239_5;
    const PI_OVER_FOUR: [f32; 3] = [0.785_156_25, 2.418_756_5e-4, 3.774_895e-8];
    let mut magnitude = x.abs();
    if magnitude > 8192.0 {
        magnitude %= std::f32::consts::TAU;
    }
    // Round the octant up to even, so the remainder stays within pi/4.
    let mut octant = (magnitude * FOUR_OVER_PI) as u32;
    octant += octant & 1;
    let octant_float = octant as f32;
    let remainder = ((magnitude - octant_float * PI_OVER_FOUR[0])
        - octant_float * PI_OVER_FOUR[1])
        - octant_float * PI_OVER_FOUR[2];
    let square = remainder * remainder;
    let sine = ((-1.951_529_6e-4 * square + 8.332_161e-3) * square
        - 1.666_665_5e-1)
        * square
        * remainder
        + remainder;
    let cosine = ((2.443_315_7e-5 * square - 1.388_731_6e-3) * square
        + 4.166_664_6e-2)
        * square
        * square
        - 0.5 * square
        + 1.0;
    let (sine, cosine) = match octant & 7 {
        0 => (sine, cosine),
        2 => (cosine, -sine),
        4 => (-sine, -cosine),
        _ => (-cosine, sine),
    };
    (if x < 0.0 { -sine } else { sine }, cosine)
}

/// `atan(x)`. Cephes `atanf`: reduce to `|t| <= tan(pi/8)` around 0,
/// pi/4, or pi/2, then a minimax polynomial.
fn atan(x: f32) -> f32 {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
    let magnitude = x.abs();
    let (base, t) = if magnitude > 2.414_213_6 {
        (FRAC_PI_2, -1.0 / magnitude)
    } else if magnitude > 0.414_213_57 {
        (FRAC_PI_4, (magnitude - 1.0) / (magnitude + 1.0))
    } else {
        (0.0, magnitude)
    };
    let square = t * t;
    let result = base
        + ((((8.053_744_5e-2 * square - 1.387_768_5e-1) * square
            + 1.997_771_1e-1)
            * square
            - 3.333_295e-1)
            * square
            * t
            + t);
    if x < 0.0 {
        -result
    } else {
        result
    }
}

/// `atan2(y, x)` in `[-pi, pi]`; `atan2(0, 0)` is 0.
#[must_use]
pub fn atan2(y: f32, x: f32) -> f32 {
    use std::f32::consts::{FRAC_PI_2, PI};
    if x == 0.0 {
        return if y > 0.0 {
            FRAC_PI_2
        } else if y < 0.0 {
            -FRAC_PI_2
        } else {
            0.0
        };
    }
    let angle = atan(y / x);
    match (x < 0.0, y < 0.0) {
        (false, _) => angle,
        (true, false) => angle + PI,
        (true, true) => angle - PI,
    }
}

/// `asin(x)` with `x` clamped to `[-1, 1]`. Cephes `asinf`.
#[must_use]
pub fn asin(x: f32) -> f32 {
    let magnitude = x.abs().min(1.0);
    let wide = magnitude > 0.5;
    let (square, root) = if wide {
        let half = 0.5 * (1.0 - magnitude);
        (half, half.sqrt())
    } else {
        (magnitude * magnitude, magnitude)
    };
    let mut result = ((((4.216_32e-2 * square + 2.418_131e-2) * square
        + 4.547_002_6e-2)
        * square
        + 7.495_300_3e-2)
        * square
        + 1.666_675_2e-1)
        * square
        * root
        + root;
    if wide {
        result = std::f32::consts::FRAC_PI_2 - (result + result);
    }
    if x < 0.0 {
        -result
    } else {
        result
    }
}

/// `Rotation3::from_euler_angles` with [`sin_cos`].
#[must_use]
pub fn rotation_from_euler(roll: f32, pitch: f32, yaw: f32) -> Rotation3<f32> {
    let (sr, cr) = sin_cos(roll);
    let (sp, cp) = sin_cos(pitch);
    let (sy, cy) = sin_cos(yaw);
    Rotation3::from_matrix_unchecked(Matrix3::new(
        cy * cp,
        cy * sp * sr - sy * cr,
        cy * sp * cr + sy * sr,
        sy * cp,
        sy * sp * sr + cy * cr,
        sy * sp * cr - cy * sr,
        -sp,
        cp * sr,
        cp * cr,
    ))
}

/// `Rotation3::euler_angles` with [`atan2`] and [`asin`]: `(roll, pitch,
/// yaw)`. Away from gimbal lock `cos(pitch) > 0`, so the angles come from
/// the matrix entries without dividing by it.
#[must_use]
pub fn euler_from_rotation(rotation: &Rotation3<f32>) -> (f32, f32, f32) {
    use std::f32::consts::FRAC_PI_2;
    let m = rotation.matrix();
    if m[(2, 0)].abs() < 1.0 {
        (
            atan2(m[(2, 1)], m[(2, 2)]),
            -asin(m[(2, 0)]),
            atan2(m[(1, 0)], m[(0, 0)]),
        )
    } else if m[(2, 0)] <= -1.0 {
        (atan2(m[(0, 1)], m[(0, 2)]), FRAC_PI_2, 0.0)
    } else {
        (-atan2(-m[(0, 1)], -m[(0, 2)]), -FRAC_PI_2, 0.0)
    }
}

/// `Rotation3::from_scaled_axis` with [`sin_cos`]: a turn of `|axis|`
/// radians around `axis`.
#[must_use]
pub fn rotation_from_scaled_axis(axis: Vector3<f32>) -> Rotation3<f32> {
    let angle = axis.norm();
    if angle == 0.0 {
        return Rotation3::identity();
    }
    let (sine, cosine) = sin_cos(0.5 * angle);
    let scaled = axis * (sine / angle);
    UnitQuaternion::new_unchecked(Quaternion::new(
        cosine, scaled.x, scaled.y, scaled.z,
    ))
    .to_rotation_matrix()
}

/// `Transform::to_matrix` with [`rotation_from_euler`]: the column-major
/// model matrix that GPU physics bodies start from.
#[must_use]
pub fn transform_matrix(transform: &crate::Transform) -> [[f32; 4]; 4] {
    let [roll, pitch, yaw] = transform.rotation;
    let rotation = rotation_from_euler(roll, pitch, yaw);
    let column = |index: usize| {
        let axis = rotation.matrix().column(index) * transform.scale[index];
        [axis.x, axis.y, axis.z, 0.0]
    };
    let [x, y, z] = transform.position;
    [column(0), column(1), column(2), [x, y, z, 1.0]]
}

/// `Transform::from_matrix` with [`euler_from_rotation`]: the transform a
/// GPU body's model matrix reads back as.
#[must_use]
pub fn transform_from_matrix(model: [[f32; 4]; 4]) -> crate::Transform {
    let axis = |index: usize| {
        Vector3::new(model[index][0], model[index][1], model[index][2])
    };
    let scale = [0, 1, 2].map(|index| axis(index).norm());
    let rotation = Rotation3::from_matrix_unchecked(Matrix3::from_columns(&[
        axis(0) / scale[0],
        axis(1) / scale[1],
        axis(2) / scale[2],
    ]));
    let (roll, pitch, yaw) = euler_from_rotation(&rotation);
    crate::Transform {
        position: [model[3][0], model[3][1], model[3][2]],
        rotation: [roll, pitch, yaw],
        scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ulps(a: f32, b: f32) -> u32 {
        a.to_bits().abs_diff(b.to_bits())
    }

    /// Why a compiled simulation shader breaks the rules in
    /// `docs/determinism.md`: a float operation the driver may fuse or
    /// reorder (no `NoContraction`, so the GLSL value was not `precise`), a
    /// relaxed-precision value, or a GLSL.std.450 function whose result
    /// differs between devices.
    fn simulation_violations(words: &[u32]) -> Vec<String> {
        // GLSL.std.450 functions with one exact result: Round is left out
        // because its direction at .5 is up to the device.
        const EXACT_FUNCTIONS: [u32; 16] =
            [2, 3, 4, 5, 6, 7, 8, 9, 37, 38, 39, 40, 41, 42, 43, 44];
        let mut precise = std::collections::HashSet::new();
        let mut operations = Vec::new();
        let mut violations = Vec::new();
        let mut index = 5;
        while index < words.len() {
            let count = (words[index] >> 16) as usize;
            let operands = &words[index + 1..index + count];
            match words[index] & 0xffff {
                // OpDecorate: NoContraction is 42, RelaxedPrecision is 0.
                71 if operands[1] == 42 => {
                    precise.insert(operands[0]);
                }
                71 if operands[1] == 0 => {
                    violations
                        .push(format!("%{} is RelaxedPrecision", operands[0]));
                }
                // OpMemberDecorate names the member before the decoration.
                72 if operands[2] == 0 => violations.push(format!(
                    "%{} member {} is RelaxedPrecision",
                    operands[0], operands[1]
                )),
                // OpExtInst.
                12 if !EXACT_FUNCTIONS.contains(&operands[3]) => violations
                    .push(format!(
                        "%{} uses GLSL.std.450 function {}",
                        operands[1], operands[3]
                    )),
                // Float add, sub, mul, div, rem, mod, and the vector and
                // matrix products up to OpDot.
                opcode @ (129 | 131 | 133 | 136 | 140 | 141 | 142..=148) => {
                    operations.push((operands[1], opcode));
                }
                _ => {}
            }
            index += count;
        }
        violations.extend(
            operations
                .into_iter()
                .filter(|(id, _)| !precise.contains(id))
                .map(|(id, opcode)| {
                    format!("%{id} (opcode {opcode}) is not precise")
                }),
        );
        violations
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests`; needs `glslc` on the PATH"
    )]
    fn simulation_shaders_are_precise_and_never_relaxed() {
        for (path, define) in [
            ("src/shaders/compute/physics.comp", None),
            ("src/shaders/compute/physics_contacts.comp", None),
            (
                "src/shaders/compute/physics_contacts.comp",
                Some("-DGRID_PASS=1"),
            ),
            ("src/shaders/compute/sim_math_test.comp", None),
            ("src/shaders/compute/cloth.comp", None),
        ] {
            let output = std::process::Command::new("glslc")
                .args([
                    "-fshader-stage=compute",
                    "-I",
                    "src/shaders",
                    "-o",
                    "-",
                    path,
                ])
                .args(define)
                .output()
                .expect(
                    "glslc from the Vulkan SDK or shaderc must be on the PATH",
                );
            assert!(
                output.status.success(),
                "{path}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let words: Vec<u32> = output
                .stdout
                .as_chunks::<4>()
                .0
                .iter()
                .map(|word| u32::from_le_bytes(*word))
                .collect();
            let violations = simulation_violations(&words);
            assert!(
                violations.is_empty(),
                "{path} {define:?}: {violations:#?}"
            );
        }
    }

    #[test]
    fn simulation_violations_flag_fusable_relaxed_and_inexact_code() {
        // Header, then: %4 NoContraction, %5 RelaxedPrecision, member 1 of
        // %7 RelaxedPrecision, %3 = OpFAdd (not precise), %4 = OpFMul, and
        // %6 = OpExtInst Sin (13).
        let header = [0x0723_0203, 0x0001_0000, 0, 10, 0];
        #[rustfmt::skip]
        let body = [
            71 | 3 << 16, 4, 42,
            71 | 3 << 16, 5, 0,
            72 | 4 << 16, 7, 1, 0,
            129 | 5 << 16, 2, 3, 1, 1,
            133 | 5 << 16, 2, 4, 1, 1,
            12 | 6 << 16, 2, 6, 7, 13, 1,
        ];
        let words: Vec<u32> = header.into_iter().chain(body).collect();
        assert_eq!(
            simulation_violations(&words),
            [
                "%5 is RelaxedPrecision",
                "%7 member 1 is RelaxedPrecision",
                "%6 uses GLSL.std.450 function 13",
                "%3 (opcode 129) is not precise",
            ]
        );
    }

    #[test]
    fn transcendentals_stay_close_to_f64_and_keep_their_bits() {
        use std::f32::consts::{FRAC_PI_2, PI};
        assert_eq!(sin_cos(0.0), (0.0, 1.0));
        assert_eq!(
            [atan2(0.0, 1.0), atan2(1.0, 0.0), atan2(0.0, 0.0)],
            [0.0, FRAC_PI_2, 0.0]
        );
        assert_eq!(
            [asin(1.0), asin(-2.0), asin(0.0)],
            [FRAC_PI_2, -FRAC_PI_2, 0.0]
        );
        assert_eq!(atan2(0.0, -1.0), PI);
        // Every result feeds a checksum pinned below, so a change in any
        // bit on any platform fails this test.
        let mut checksum = 0u32;
        let mut mix =
            |value: f32| checksum = checksum.rotate_left(5) ^ value.to_bits();
        for step in -4000..=4000 {
            let x = step as f32 * 0.0025;
            let wide = x * 1000.0;
            let (sine, cosine) = sin_cos(wide);
            let error = (f64::from(sine) - f64::from(wide).sin()).abs()
                + (f64::from(cosine) - f64::from(wide).cos()).abs();
            assert!(wide.abs() > 8192.0 || error < 4e-7, "sin_cos({wide})");
            let angle = atan2(x, 1.0 - x.abs());
            let expected = f64::from(x).atan2(1.0 - f64::from(x.abs()));
            assert!((f64::from(angle) - expected).abs() < 5e-7, "atan2({x})");
            let clamped = x.clamp(-1.0, 1.0);
            assert!(
                (f64::from(asin(clamped)) - f64::from(clamped).asin()).abs()
                    < 5e-7,
                "asin({x})"
            );
            [sine, cosine, angle, asin(clamped)]
                .into_iter()
                .for_each(&mut mix);
        }
        assert_eq!(checksum, 1_637_066_667);
    }

    #[test]
    fn rotation_helpers_match_nalgebra() {
        let spin = Vector3::new(0.3, -1.2, 0.7);
        let difference = rotation_from_scaled_axis(spin).matrix()
            - Rotation3::from_scaled_axis(spin).matrix();
        assert!(difference.abs().max() < 1e-6);
        assert_eq!(
            rotation_from_scaled_axis(Vector3::zeros()),
            Rotation3::identity()
        );
        let (roll, pitch, yaw) = (0.4, -0.9, 2.5);
        let rotation = rotation_from_euler(roll, pitch, yaw);
        let difference = rotation.matrix()
            - Rotation3::from_euler_angles(roll, pitch, yaw).matrix();
        assert!(difference.abs().max() < 1e-6);
        let transform = crate::Transform {
            position: [1.0, -2.0, 3.0],
            rotation: [roll, pitch, yaw],
            scale: [2.0, 0.5, 1.5],
        };
        let expected = transform.to_matrix();
        for (column, expected) in
            transform_matrix(&transform).iter().zip(expected)
        {
            for (value, expected) in column.iter().zip(expected) {
                assert!((value - expected).abs() < 1e-5);
            }
        }
        let back = transform_from_matrix(transform_matrix(&transform));
        for (value, expected) in back
            .position
            .iter()
            .chain(&back.rotation)
            .chain(&back.scale)
            .zip(
                transform
                    .position
                    .iter()
                    .chain(&transform.rotation)
                    .chain(&transform.scale),
            )
        {
            assert!((value - expected).abs() < 1e-5);
        }
        let (x, y, z) = euler_from_rotation(&rotation);
        assert!(
            (x - roll).abs() < 1e-5
                && (y - pitch).abs() < 1e-5
                && (z - yaw).abs() < 1e-5
        );
    }

    #[test]
    fn round_values_are_exact_and_the_rest_stay_within_two_ulp() {
        assert_eq!(recip(1.0), 1.0);
        assert_eq!(recip(-4.0), -0.25);
        assert_eq!(div(3.0, 1.0), 3.0);
        assert_eq!(rsqrt(4.0), 0.5);
        assert_eq!([sqrt(9.0), sqrt(100.0), sqrt(-1.0)], [3.0, 10.0, 0.0]);
        assert_eq!(normalize([0.0, 3.0, 4.0]), [0.0, 0.6, 0.8]);
        assert_eq!(normalize([0.0; 3]), [0.0; 3]);
        for step in 0..4000 {
            let x = 2f32.powf(step as f32 / 50.0 - 40.0);
            assert!(ulps(recip(x), 1.0 / x) <= 1, "recip({x})");
            assert!(ulps(rsqrt(x), 1.0 / x.sqrt()) <= 2, "rsqrt({x})");
            assert!(ulps(sqrt(x), x.sqrt()) <= 1, "sqrt({x})");
        }
    }
}
