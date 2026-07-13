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
    let (yaw, pitch, roll) = state.rot_mat.to_euler(EulerRot::ZYX);

    Physics {
        location: vector3_to_rlbot(state.pos),
        rotation: Rotator { pitch, yaw, roll },
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
            physics.rotation.pitch,
            physics.rotation.roll,
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
        for (actual, expected) in converted
            .rot_mat
            .to_cols_array()
            .into_iter()
            .zip(original.rot_mat.to_cols_array())
        {
            assert_close(actual, expected);
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
