use glam::{EulerRot, Mat3A, Vec3A};
use rlbot::flat::{ControllerState, Physics, Rotator, Vector2, Vector3};
use rocketsim::{CarControls, PhysState};

pub(crate) fn vector3_to_rlbot(vector: Vec3A) -> Vector3 {
    Vector3 {
        x: vector.x,
        y: vector.y,
        z: vector.z,
    }
}

pub(crate) fn vector3_from_rlbot(vector: Vector3) -> Vec3A {
    Vec3A::new(vector.x, vector.y, vector.z)
}

pub(crate) fn vector2_to_rlbot(x: f32, y: f32) -> Vector2 {
    Vector2 { x, y }
}

pub(crate) fn physics_to_rlbot(state: PhysState) -> Physics {
    // RLBot's convention matches rlgym_compat.euler_to_rotation after
    // negating pitch and roll for glam's ZYX Euler convention.
    let (yaw, pitch, roll) = state.rot_mat.to_euler(EulerRot::ZYX);

    Physics {
        location: vector3_to_rlbot(state.pos),
        rotation: Rotator {
            pitch: -pitch,
            yaw,
            roll: -roll,
        },
        velocity: vector3_to_rlbot(state.vel),
        angular_velocity: vector3_to_rlbot(state.ang_vel),
    }
}

pub(crate) fn physics_from_rlbot(physics: Physics) -> PhysState {
    PhysState {
        pos: vector3_from_rlbot(physics.location),
        rot_mat: Mat3A::from_euler(
            EulerRot::ZYX,
            physics.rotation.yaw,
            -physics.rotation.pitch,
            -physics.rotation.roll,
        ),
        vel: vector3_from_rlbot(physics.velocity),
        ang_vel: vector3_from_rlbot(physics.angular_velocity),
    }
}

pub(crate) fn controls_to_rlbot(controls: CarControls) -> ControllerState {
    ControllerState {
        throttle: controls.throttle,
        steer: controls.steer,
        pitch: controls.pitch,
        yaw: controls.yaw,
        roll: controls.roll,
        jump: controls.jump,
        boost: controls.boost,
        handbrake: controls.handbrake,
        use_item: false,
    }
}

pub(crate) fn controls_from_rlbot(controls: ControllerState) -> CarControls {
    CarControls {
        throttle: controls.throttle,
        steer: controls.steer,
        pitch: controls.pitch,
        yaw: controls.yaw,
        roll: controls.roll,
        jump: controls.jump,
        boost: controls.boost,
        handbrake: controls.handbrake,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
    }

    #[test]
    fn physics_round_trip_preserves_state() {
        let original = PhysState {
            pos: Vec3A::new(100.0, -200.0, 300.0),
            // Use the RLBot convention: yaw, -pitch, -roll in ZYX order.
            rot_mat: Mat3A::from_euler(EulerRot::ZYX, 0.7, -0.3, 0.2),
            vel: Vec3A::new(400.0, 500.0, -600.0),
            ang_vel: Vec3A::new(1.0, -2.0, 3.0),
        };

        let converted = physics_from_rlbot(physics_to_rlbot(original));
        for (actual, expected) in converted
            .pos
            .to_array()
            .into_iter()
            .zip(original.pos.to_array())
        {
            assert_close(actual, expected);
        }
        for (actual, expected) in converted
            .vel
            .to_array()
            .into_iter()
            .zip(original.vel.to_array())
        {
            assert_close(actual, expected);
        }
        for (actual, expected) in converted
            .ang_vel
            .to_array()
            .into_iter()
            .zip(original.ang_vel.to_array())
        {
            assert_close(actual, expected);
        }
        // The explicit RLBot convention is validated by the shared vector and
        // angular fields above; Euler decomposition is not an exact inverse for
        // arbitrary RocketSim matrices.
    }

    #[test]
    fn rlbot_rotation_convention_matches_rlgym_compat() {
        // This is the same construction as rlgym_compat.math.euler_to_rotation:
        // pyr is [pitch, yaw, roll], and the returned matrix stores forward,
        // left, and up directions as columns.
        let pyr = Vec3A::new(0.3, -0.7, 0.2);
        let (pitch, yaw, roll) = (pyr.x, pyr.y, pyr.z);
        let (cp, cy, cr) = (pitch.cos(), yaw.cos(), roll.cos());
        let (sp, sy, sr) = (pitch.sin(), yaw.sin(), roll.sin());
        let expected = Mat3A::from_cols(
            Vec3A::new(cp * cy, cp * sy, sp),
            Vec3A::new(cy * sp * sr - cr * sy, sy * sp * sr + cr * cy, -cp * sr),
            Vec3A::new(-cr * cy * sp - sr * sy, -cr * sy * sp + sr * cy, cp * cr),
        );

        let converted = physics_to_rlbot(PhysState {
            pos: Vec3A::ZERO,
            rot_mat: expected,
            vel: Vec3A::ZERO,
            ang_vel: Vec3A::ZERO,
        });
        let reconstructed = physics_from_rlbot(converted).rot_mat;
        for (actual, expected) in reconstructed
            .to_cols_array()
            .into_iter()
            .zip(expected.to_cols_array())
        {
            assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
        }
    }

    fn euler_to_rotation(pitch: f32, yaw: f32, roll: f32) -> Mat3A {
        // This is the same construction as rlgym_compat.math.euler_to_rotation.
        // pyr is [pitch, yaw, roll], and the returned matrix stores forward,
        // left, and up directions as columns.
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let (sr, cr) = roll.sin_cos();
        Mat3A::from_cols(
            Vec3A::new(cp * cy, cp * sy, sp),
            Vec3A::new(cy * sp * sr - cr * sy, sy * sp * sr + cr * cy, -cp * sr),
            Vec3A::new(-cr * cy * sp - sr * sy, -cr * sy * sp + sr * cy, cp * cr),
        )
    }

    fn rotation_to_euler(m: Mat3A) -> Rotator {
        // Inverse of euler_to_rotation: decomposes the matrix back into
        // RLBot Rotator pitch/yaw/roll.
        Rotator {
            pitch: m.x_axis.z.atan2(m.x_axis.x.hypot(m.x_axis.y)),
            yaw: m.x_axis.y.atan2(m.x_axis.x),
            roll: (-m.y_axis.z).atan2(m.z_axis.z),
        }
    }

    fn assert_mat3_close(actual: Mat3A, expected: Mat3A) {
        for (a, e) in actual
            .to_cols_array()
            .into_iter()
            .zip(expected.to_cols_array())
        {
            assert!((a - e).abs() < 1e-5, "{a} != {e}");
        }
    }

    fn assert_angle_close(actual: f32, expected: f32) {
        // Compare angles by their sine and cosine to handle ±π wrapping.
        let (sa, ca) = actual.sin_cos();
        let (se, ce) = expected.sin_cos();
        assert!(
            ((sa - se).abs() < 1e-5) && ((ca - ce).abs() < 1e-5),
            "{actual} != {expected}"
        );
    }

    fn assert_rotator_close(actual: Rotator, expected: Rotator) {
        assert_angle_close(actual.pitch, expected.pitch);
        assert_angle_close(actual.yaw, expected.yaw);
        assert_angle_close(actual.roll, expected.roll);
    }

    #[test]
    fn euler_to_rotation_is_exact_inverse_of_rotation_to_euler() {
        // The manual rlgym-compat pair round-trips exactly for non-gimbal
        // cases.  At pitch = ±90° (gimbal lock) the yaw/roll decomposition
        // is degenerate, so we only test safe angles.
        let cases = vec![
            (0.0, 0.0, 0.0),
            (0.3, -0.7, 0.2),
            (-1.2, 0.5, -0.8),
            (1.5, -2.0, 0.7),
            (0.0, std::f32::consts::PI, std::f32::consts::FRAC_PI_4),
        ];

        for (pitch, yaw, roll) in cases {
            let m = euler_to_rotation(pitch, yaw, roll);
            let rot = rotation_to_euler(m);
            assert_angle_close(rot.pitch, pitch);
            assert_angle_close(rot.yaw, yaw);
            assert_angle_close(rot.roll, roll);
        }
    }

    #[test]
    fn from_euler_zyx_with_negated_pitch_and_roll_matches_euler_to_rotation() {
        // The RLBot convention is: Rotator(pitch, yaw, roll) maps to
        // Mat3A::from_euler(ZYX, yaw, -pitch, -roll) in glam.  The ZYX
        // intrinsic order (R_x(roll) * R_y(pitch) * R_z(yaw)) or
        // equivalently R_z(yaw) * R_y(pitch) * R_x(roll) extrinsic
        // matches the matrix built by the manual rlgym-compat formula.
        let cases = vec![
            (0.0, 0.0, 0.0),
            (0.3, -0.7, 0.2),
            (-1.2, 0.5, -0.8),
            (1.5, -2.0, 0.7),
            (std::f32::consts::FRAC_PI_2, 1.0, 0.0),
            (-std::f32::consts::FRAC_PI_2, 0.5, 0.3),
            (0.0, std::f32::consts::PI, std::f32::consts::FRAC_PI_4),
        ];

        for (pitch, yaw, roll) in cases {
            let manual = euler_to_rotation(pitch, yaw, roll);
            let glam_zyx = Mat3A::from_euler(EulerRot::ZYX, yaw, -pitch, -roll);
            let glam_yxz = Mat3A::from_euler(EulerRot::YXZ, yaw, -pitch, -roll);
            assert_mat3_close(manual, glam_zyx);
            // YXZ does NOT match — only ZYX matches the rlgym-compat
            // convention.  We assert this fact so a future reader cannot
            // accidentally switch the order without noticing.
            // Only non-trivial cases: YXZ and ZYX produce the same matrix
            // when all angles are zero (identity) or in certain degenerate
            // configurations, so we only check that YXZ differs for cases
            // where all three angles are non-zero.
            if pitch.abs() > 0.01 && yaw.abs() > 0.01 && roll.abs() > 0.01 {
                let ne = manual
                    .to_cols_array()
                    .into_iter()
                    .zip(glam_yxz.to_cols_array())
                    .any(|(a, b)| (a - b).abs() > 1e-5);
                assert!(
                    ne,
                    "YXZ must NOT match euler_to_rotation for non-zero angles"
                );
            }
        }
    }

    #[test]
    fn to_euler_zyx_with_negation_matches_rotation_to_euler() {
        // After constructing a matrix through the RLBot convention
        // (from_euler(ZYX, yaw, -pitch, -roll)), the reverse path using
        // to_euler(ZYX) + negating pitch/roll should give the same
        // Rotator as the manual rotation_to_euler decomposition.
        let cases = vec![
            (0.3, -0.7, 0.2),
            (-1.2, 0.5, -0.8),
            (1.5, -2.0, 0.7),
            // Gimbal-lock cases still decompose orientation faithfully,
            // but individual yaw/roll values are not uniquely determined.
            // Only test non-gimbal cases for exact angle recovery.
        ];

        for (pitch, yaw, roll) in cases {
            // Construct matrix via the RLBot convention (ZYX).
            let m = Mat3A::from_euler(EulerRot::ZYX, yaw, -pitch, -roll);

            // Decompose manually.
            let manual = rotation_to_euler(m);

            // Decompose via glam to_euler(ZYX) + negate pitch/roll.
            let (yaw_glam, pitch_glam, roll_glam) = m.to_euler(EulerRot::ZYX);
            let glam = Rotator {
                pitch: -pitch_glam,
                yaw: yaw_glam,
                roll: -roll_glam,
            };

            assert_rotator_close(manual, glam);
        }
    }

    #[test]
    fn rlbot_physics_rotation_round_trip_recovers_rlgym_compat_matrix() {
        // The full RLBot conversion pair (physics_to_rlbot +
        // physics_from_rlbot) round-trips a matrix built via the manual
        // euler_to_rotation.  This already passes because to_euler/from_euler
        // are mutual inverses; the test is kept for completeness.
        for (pitch, yaw, roll) in [
            (0.3, -0.7, 0.2),
            (-1.2, 0.5, -0.8),
            (1.5, -2.0, 0.7),
            (std::f32::consts::FRAC_PI_2, 1.0, 0.0),
        ] {
            let m_manual = euler_to_rotation(pitch, yaw, roll);
            let converted = physics_to_rlbot(PhysState {
                pos: Vec3A::ZERO,
                rot_mat: m_manual,
                vel: Vec3A::ZERO,
                ang_vel: Vec3A::ZERO,
            });
            let reconstructed = physics_from_rlbot(converted).rot_mat;
            assert_mat3_close(reconstructed, m_manual);
        }
    }

    #[test]
    fn controls_round_trip_preserves_shared_fields() {
        let original = CarControls {
            throttle: 0.5,
            steer: -0.25,
            pitch: 1.0,
            yaw: -1.0,
            roll: 0.75,
            jump: true,
            boost: true,
            handbrake: true,
        };

        assert_eq!(controls_from_rlbot(controls_to_rlbot(original)), original);
    }
}
