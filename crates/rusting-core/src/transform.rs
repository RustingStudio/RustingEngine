use nalgebra::{Matrix3, Matrix4, Rotation3, Vector3};

/// Describes where an object is, how it is rotated,
/// and how large it is.
///
/// All arrays use this order:
/// [x, y, z]
#[derive(bevy_ecs::component::Component, Copy, Clone, Debug, PartialEq)]
pub struct Transform {
    /// Object position in the world:
    /// [left/right, down/up, backward/forward]
    pub position: [f32; 3],

    /// Rotation around the X, Y, and Z axes.
    ///
    /// IMPORTANT: These values are in radians, not degrees.
    ///
    /// `rotation[0]` = rotation around X
    /// `rotation[1]` = rotation around Y
    /// `rotation[2]` = rotation around Z
    pub rotation: [f32; 3],

    /// Object size along each local axis:
    ///
    /// `scale[0]` = size along X
    /// `scale[1]` = size along Y
    /// `scale[2]` = size along Z
    ///
    /// [1.0, 1.0, 1.0] means normal size.
    /// [2.0, 2.0, 2.0] means twice as large.
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            // Start at the center of the world.
            position: [0.0, 0.0, 0.0],

            // No rotation.
            rotation: [0.0, 0.0, 0.0],

            // Normal size.
            scale: [1.0, 1.0, 1.0],
        }
    }
}

impl Transform {
    /// Creates a transform at the given position.
    ///
    /// Rotation defaults to zero.
    /// Scale defaults to one.
    pub fn new(position: [f32; 3]) -> Self {
        Self {
            position,
            ..Default::default()
        }
    }

    pub fn with_position(mut self, x: f32, y: f32, z: f32) -> Self {
        self.position = [x, y, z];
        self
    }

    /// Sets rotation in radians.
    pub fn with_rotation(mut self, x: f32, y: f32, z: f32) -> Self {
        self.rotation = [x, y, z];
        self
    }

    pub fn with_scale(mut self, x: f32, y: f32, z: f32) -> Self {
        self.scale = [x, y, z];
        self
    }

    /// Combines position, rotation, and scale into one 4×4 matrix.
    pub fn to_matrix(&self) -> [[f32; 4]; 4] {
        // Creates this translation matrix:
        //
        // ┌                     ┐
        // │ 1  0  0  position.x │
        // │ 0  1  0  position.y │
        // │ 0  0  1  position.z │
        // │ 0  0  0      1      │
        // └                     ┘
        //
        // It moves the object to its world position.
        let translation =
            Matrix4::new_translation(&Vector3::from(self.position));

        // Creates a 3D rotation from:
        //
        // rotation[0] = angle around X
        // rotation[1] = angle around Y
        // rotation[2] = angle around Z
        //
        // to_homogeneous() turns the 3×3 rotation matrix
        // into a 4×4 matrix.
        let rotation = Rotation3::from_euler_angles(
            self.rotation[0],
            self.rotation[1],
            self.rotation[2],
        )
        .to_homogeneous();

        // Creates this scale matrix:
        //
        // ┌                            ┐
        // │ scale.x    0        0    0 │
        // │    0    scale.y     0    0 │
        // │    0       0     scale.z 0 │
        // │    0       0        0    1 │
        // └                            ┘
        let scale = Matrix4::new_nonuniform_scaling(&Vector3::from(self.scale));

        // Transformations are applied from right to left:
        //
        // 1. Scale the object
        // 2. Rotate the object
        // 3. Move the object
        let model_matrix = translation * rotation * scale;

        // nalgebra converts the matrix into a column-major array:
        //
        // [
        //     [Xx, Xy, Xz, 0.0], // transformed local X axis
        //     [Yx, Yy, Yz, 0.0], // transformed local Y axis
        //     [Zx, Zy, Zz, 0.0], // transformed local Z axis
        //     [px, py, pz, 1.0], // world position
        // ]
        //
        // Therefore:
        //
        // result[0] = object's local X axis
        // result[1] = object's local Y axis
        // result[2] = object's local Z axis
        // result[3] = object's position
        model_matrix.into()
    }

    /// Extracts position, rotation, and scale from a 4×4 matrix.
    ///
    /// nalgebra indexes matrices as:
    ///
    /// m[(row, column)]
    ///
    /// The mathematical matrix looks like:
    ///
    /// ┌                           ┐
    /// │ m00  m01  m02  position.x │
    /// │ m10  m11  m12  position.y │
    /// │ m20  m21  m22  position.z │
    /// │  0    0    0       1      │
    /// └                           ┘
    pub fn from_matrix(m: Matrix4<f32>) -> Self {
        // Translation is stored in column 3:
        //
        // m[(0, 3)] = X position
        // m[(1, 3)] = Y position
        // m[(2, 3)] = Z position
        let position = [m[(0, 3)], m[(1, 3)], m[(2, 3)]];

        // The first three columns describe the object's
        // transformed local axes.
        //
        // Column 0 = local X direction multiplied by X scale
        // Column 1 = local Y direction multiplied by Y scale
        // Column 2 = local Z direction multiplied by Z scale
        //
        // The length of each column gives us its scale.
        let mut scale = [
            // Length of column 0: X scale
            Vector3::new(m[(0, 0)], m[(1, 0)], m[(2, 0)]).norm(),
            // Length of column 1: Y scale
            Vector3::new(m[(0, 1)], m[(1, 1)], m[(2, 1)]).norm(),
            // Length of column 2: Z scale
            Vector3::new(m[(0, 2)], m[(1, 2)], m[(2, 2)]).norm(),
        ];

        // A mirrored matrix (negative determinant) cannot be a rotation
        // times positive scales, so the mirror goes into the X scale.
        if m.fixed_view::<3, 3>(0, 0).determinant() < 0.0 {
            scale[0] = -scale[0];
        }

        // A zero scale has no direction to recover; dividing by 1 instead
        // keeps the rotation finite (the zero column stays zero).
        let divisor = scale.map(|s| if s == 0.0 { 1.0 } else { s });

        // Each axis currently contains both rotation and scale.
        //
        // Dividing each column by its scale removes the scale,
        // leaving only the rotation.
        //
        // For example:
        //
        // m[(0, 0)] / scale[0]
        //
        // means:
        // "world-X part of the local X axis, without X scale."
        let rotation_matrix = Matrix3::new(
            // Row 0: how local X, Y, and Z affect world X
            m[(0, 0)] / divisor[0],
            m[(0, 1)] / divisor[1],
            m[(0, 2)] / divisor[2],
            // Row 1: how local X, Y, and Z affect world Y
            m[(1, 0)] / divisor[0],
            m[(1, 1)] / divisor[1],
            m[(1, 2)] / divisor[2],
            // Row 2: how local X, Y, and Z affect world Z
            m[(2, 0)] / divisor[0],
            m[(2, 1)] / divisor[1],
            m[(2, 2)] / divisor[2],
        );

        // Convert the rotation matrix back into:
        //
        // rotation.0 = X angle
        // rotation.1 = Y angle
        // rotation.2 = Z angle
        let rotation =
            Rotation3::from_matrix_unchecked(rotation_matrix).euler_angles();

        Self {
            position,
            rotation: [rotation.0, rotation.1, rotation.2],
            scale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(t: &Transform) -> Matrix4<f32> {
        Matrix4::from(t.to_matrix())
    }

    fn close(a: Matrix4<f32>, b: Matrix4<f32>) -> bool {
        (a - b).abs().max() < 1e-5
    }

    #[test]
    fn transforms_compose_and_decompose_back_to_the_same_matrix() {
        let cases = [
            Transform::default(),
            Transform::new([1.0, -2.0, 3.0]),
            Transform::new([4.0, 5.0, 6.0])
                .with_rotation(0.3, -1.1, 2.0)
                .with_scale(2.0, 0.5, 3.0),
            // Gimbal lock: the angles come back different, the matrix not.
            Transform::default().with_rotation(
                0.4,
                std::f32::consts::FRAC_PI_2,
                0.2,
            ),
            // Mirrored along one axis.
            Transform::default()
                .with_rotation(0.0, 0.7, 0.0)
                .with_scale(-1.0, 2.0, 1.0),
        ];
        for case in cases {
            let back = Transform::from_matrix(matrix(&case));
            assert!(
                close(matrix(&back), matrix(&case)),
                "{case:?} -> {back:?}"
            );
            assert!(back.scale[1] > 0.0 && back.scale[2] > 0.0, "{back:?}");
        }
        let plain =
            Transform::new([1.0, 2.0, 3.0]).with_rotation(0.1, 0.2, 0.3);
        let back = Transform::from_matrix(matrix(&plain));
        for (a, b) in back.rotation.iter().zip(plain.rotation) {
            assert!((a - b).abs() < 1e-5, "{back:?}");
        }

        // A flattened object keeps finite numbers.
        let flat = Transform::default()
            .with_rotation(0.0, 0.5, 0.0)
            .with_scale(1.0, 0.0, 1.0);
        let back = Transform::from_matrix(matrix(&flat));
        assert!(back.rotation.iter().all(|a| a.is_finite()), "{back:?}");
        assert_eq!(back.scale[1], 0.0);

        // Parent times child: scale, then rotate, then move, child first.
        let parent = Transform::new([10.0, 0.0, 0.0])
            .with_rotation(0.0, std::f32::consts::FRAC_PI_2, 0.0)
            .with_scale(2.0, 2.0, 2.0);
        let child = Transform::new([1.0, 0.0, 0.0]);
        let world = matrix(&parent) * matrix(&child);
        let origin = world.transform_point(&nalgebra::Point3::origin());
        // The child's +X offset of 1 is doubled and turned to -Z.
        assert!(
            (origin - nalgebra::Point3::new(10.0, 0.0, -2.0)).norm() < 1e-5,
            "{origin}"
        );
        let composed = Transform::from_matrix(world);
        assert!(close(matrix(&composed), world));
    }
}
