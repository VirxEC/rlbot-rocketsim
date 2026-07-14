use glam::{EulerRot, Mat3A, Vec3A};
use rlbot_rocketsim::GameStateEnricher;
use rlbot_rocketsim::rlbot::flat::{
    AirState, ControllerState, CustomBot, GamePacket, Human, Physics, PlayerClass,
    PlayerConfiguration, PlayerInfo, PlayerLoadout, Rotator, Vector2, Vector3,
};
use rlbot_rocketsim::rocketsim::{
    Arena, CarBodyConfig, CarControls, CarInfo, CarState, GameMode, PhysState, Team,
    init_from_default,
};
use rlbot_rocketsim::to_rlbot::car_to_player_info;

const EPSILON: f32 = 1e-4;

fn assert_float(actual: f32, expected: f32, field: &str) {
    assert!(
        (actual - expected).abs() <= EPSILON,
        "{field}: {actual} != {expected}"
    );
}

fn assert_vec(actual: Vec3A, expected: Vec3A, field: &str) {
    for (index, (actual, expected)) in actual
        .to_array()
        .into_iter()
        .zip(expected.to_array())
        .enumerate()
    {
        assert_float(actual, expected, &format!("{field}[{index}]"));
    }
}

fn assert_rotation(actual: Mat3A, expected: Mat3A) {
    for (index, (actual, expected)) in actual
        .to_cols_array()
        .into_iter()
        .zip(expected.to_cols_array())
        .enumerate()
    {
        assert_float(actual, expected, &format!("rotation[{index}]"));
    }
}

fn bot_config(player_id: i32, name: &str, team: Team, car_id: u32) -> PlayerConfiguration {
    PlayerConfiguration {
        variety: PlayerClass::CustomBot(Box::new(CustomBot {
            name: name.into(),
            loadout: Some(Box::new(PlayerLoadout {
                car_id,
                ..PlayerLoadout::default()
            })),
            ..CustomBot::default()
        })),
        team: team as u32,
        player_id,
    }
}

fn round_trip(info: &CarInfo, state: CarState, config: &PlayerConfiguration) -> CarState {
    let player = car_to_player_info(info, &state, config).unwrap();
    let mut packet = GamePacket::default();
    packet.match_info.frame_num = 1;
    packet.players.push(player);

    let arena = Arena::new(GameMode::Soccar);
    let mut enricher = GameStateEnricher::new(arena, info.config);
    enricher.update(&packet).unwrap();
    *enricher.car_state(0).unwrap()
}

#[test]
fn all_shared_car_and_player_data_round_trips() {
    init_from_default(true).unwrap();

    let body_config = CarBodyConfig::DOMINUS;
    let info = CarInfo {
        idx: 0,
        team: Team::Orange,
        config: body_config,
    };
    let config = PlayerConfiguration {
        variety: PlayerClass::Human(Box::new(Human {})),
        team: Team::Orange as u32,
        player_id: 1234,
    };
    let original = CarState {
        phys: PhysState {
            pos: Vec3A::new(250.0, -1_500.0, 17.0),
            rot_mat: Mat3A::from_euler(EulerRot::ZYX, 0.75, -0.2, 0.1),
            vel: Vec3A::new(900.0, -400.0, 100.0),
            ang_vel: Vec3A::new(1.0, -2.0, 3.0),
        },
        controls: CarControls {
            throttle: 0.75,
            steer: -0.5,
            pitch: 0.25,
            yaw: -0.75,
            roll: 1.0,
            jump: false,
            boost: true,
            handbrake: true,
        },
        is_on_ground: false,
        has_jumped: true,
        has_double_jumped: false,
        has_flipped: false,
        flip_rel_torque: Vec3A::new(-0.6, 0.8, 0.0),
        flip_time: 0.35,
        is_flipping: true,
        is_jumping: false,
        air_time_since_jump: 0.45,
        boost: 47.5,
        is_supersonic: true,
        is_demoed: false,
        demo_respawn_timer: 0.0,
        ..CarState::default()
    };

    let player = car_to_player_info(&info, &original, &config).unwrap();
    assert_eq!(player.player_id, config.player_id);
    assert_eq!(player.team, 1);
    assert_eq!(player.name, "Human 0");
    assert!(!player.is_bot);
    assert_float(
        player.hitbox.length,
        body_config.hitbox_size.x,
        "hitbox.length",
    );
    assert_float(
        player.hitbox.width,
        body_config.hitbox_size.y,
        "hitbox.width",
    );
    assert_float(
        player.hitbox.height,
        body_config.hitbox_size.z,
        "hitbox.height",
    );
    assert_float(
        player.hitbox_offset.x,
        body_config.hitbox_pos_offset.x,
        "hitbox_offset.x",
    );
    assert_float(
        player.hitbox_offset.y,
        body_config.hitbox_pos_offset.y,
        "hitbox_offset.y",
    );
    assert_float(
        player.hitbox_offset.z,
        body_config.hitbox_pos_offset.z,
        "hitbox_offset.z",
    );
    assert_eq!(player.air_state, AirState::Dodging);
    assert_float(player.dodge_timeout, 0.8, "dodge_timeout");
    assert_eq!(player.demolished_timeout, -1.0);
    assert_float(player.dodge_dir.x, 0.8, "dodge_dir.x");
    assert_float(player.dodge_dir.y, 0.6, "dodge_dir.y");

    let converted = round_trip(&info, original, &config);
    assert_vec(converted.phys.pos, original.phys.pos, "position");
    assert_rotation(converted.phys.rot_mat, original.phys.rot_mat);
    assert_vec(converted.phys.vel, original.phys.vel, "velocity");
    assert_vec(
        converted.phys.ang_vel,
        original.phys.ang_vel,
        "angular_velocity",
    );
    assert_eq!(converted.controls, original.controls);
    assert_eq!(converted.has_jumped, original.has_jumped);
    assert_eq!(converted.has_double_jumped, original.has_double_jumped);
    assert_eq!(converted.has_flipped, original.has_flipped);
    assert_vec(
        converted.flip_rel_torque,
        original.flip_rel_torque,
        "flip_rel_torque",
    );
    assert_float(converted.flip_time, original.flip_time, "flip_time");
    assert_eq!(converted.is_flipping, original.is_flipping);
    assert_eq!(converted.is_jumping, original.is_jumping);
    assert_float(
        converted.air_time_since_jump,
        original.air_time_since_jump,
        "air_time_since_jump",
    );
    assert_float(converted.boost, original.boost, "boost");
    assert_eq!(converted.is_supersonic, original.is_supersonic);
    assert_eq!(converted.is_demoed, original.is_demoed);
    assert_float(
        converted.demo_respawn_timer,
        original.demo_respawn_timer,
        "demo_respawn_timer",
    );
}

#[test]
fn grounded_state_and_demo_timer_round_trip() {
    init_from_default(true).unwrap();

    let info = CarInfo {
        idx: 0,
        team: Team::Blue,
        config: CarBodyConfig::OCTANE,
    };
    let config = bot_config(9, "Demoed", Team::Blue, 23);
    let original = CarState {
        is_on_ground: true,
        wheels_with_contact: [true; 4],
        is_demoed: true,
        demo_respawn_timer: 1.75,
        boost: 0.0,
        ..CarState::default()
    };

    let player = car_to_player_info(&info, &original, &config).unwrap();
    assert_eq!(player.air_state, AirState::OnGround);
    assert_eq!(player.dodge_timeout, -1.0);
    assert_float(player.demolished_timeout, 1.75, "demolished_timeout");

    let converted = round_trip(&info, original, &config);
    // RLBot AirState is not wheel-contact data, so contact is intentionally not
    // asserted in this conversion-only TheVoid test.
    assert!(converted.is_demoed);
    assert_float(converted.demo_respawn_timer, 1.75, "demo_respawn_timer");
}

#[test]
fn rlbot_player_shared_fields_round_trip() {
    init_from_default(true).unwrap();

    let body_config = CarBodyConfig::MERC;
    let original = PlayerInfo {
        physics: Physics {
            location: Vector3 {
                x: -300.0,
                y: 1_700.0,
                z: 450.0,
            },
            rotation: Rotator {
                pitch: 0.25,
                yaw: -1.5,
                roll: 0.4,
            },
            velocity: Vector3 {
                x: -500.0,
                y: 600.0,
                z: 700.0,
            },
            angular_velocity: Vector3 {
                x: -1.0,
                y: 2.0,
                z: -3.0,
            },
        },
        air_state: AirState::Jumping,
        dodge_timeout: 0.9,
        demolished_timeout: -1.0,
        is_supersonic: true,
        is_bot: true,
        name: "Packet Player".into(),
        team: 1,
        boost: 31.25,
        player_id: 77,
        last_input: ControllerState {
            throttle: -0.5,
            steer: 0.25,
            pitch: -0.75,
            yaw: 0.5,
            roll: -0.25,
            jump: true,
            boost: false,
            handbrake: true,
            use_item: false,
        },
        has_jumped: true,
        has_double_jumped: false,
        has_dodged: false,
        dodge_elapsed: 0.0,
        dodge_dir: Vector2 { x: -0.6, y: 0.8 },
        ..PlayerInfo::default()
    };
    let info = CarInfo {
        idx: 0,
        team: Team::Orange,
        config: body_config,
    };
    let config = bot_config(original.player_id, &original.name, Team::Orange, 30);
    let mut packet = GamePacket::default();
    packet.match_info.frame_num = 1;
    packet.players.push(original.clone());
    let arena = Arena::new(GameMode::Soccar);
    let mut enricher = GameStateEnricher::new(arena, body_config);
    enricher.update(&packet).unwrap();
    let state = enricher.car_state(0).unwrap();
    let converted = car_to_player_info(&info, state, &config).unwrap();

    assert_float(
        converted.physics.location.x,
        original.physics.location.x,
        "location.x",
    );
    assert_float(
        converted.physics.location.y,
        original.physics.location.y,
        "location.y",
    );
    assert_float(
        converted.physics.location.z,
        original.physics.location.z,
        "location.z",
    );
    let original_rotation = Mat3A::from_euler(
        EulerRot::ZYX,
        original.physics.rotation.yaw,
        original.physics.rotation.pitch,
        original.physics.rotation.roll,
    );
    let converted_rotation = Mat3A::from_euler(
        EulerRot::ZYX,
        converted.physics.rotation.yaw,
        converted.physics.rotation.pitch,
        converted.physics.rotation.roll,
    );
    assert_rotation(converted_rotation, original_rotation);
    assert_eq!(converted.last_input, original.last_input);
    assert_eq!(converted.air_state, original.air_state);
    // RLBot's dodge_timeout includes the variable initial-jump hold extension, while
    // RocketSim tracks time since that jump ended. The exact timeout is not invertible.
    assert!(converted.dodge_timeout >= original.dodge_timeout);
    assert_eq!(converted.demolished_timeout, original.demolished_timeout);
    assert_eq!(converted.is_supersonic, original.is_supersonic);
    assert_eq!(converted.is_bot, original.is_bot);
    assert_eq!(converted.name, original.name);
    assert_eq!(converted.team, original.team);
    assert_float(converted.boost, original.boost, "boost");
    assert_eq!(converted.player_id, original.player_id);
    assert_eq!(converted.has_jumped, original.has_jumped);
    assert_eq!(converted.has_double_jumped, original.has_double_jumped);
    assert_eq!(converted.has_dodged, original.has_dodged);
    assert_float(
        converted.dodge_elapsed,
        original.dodge_elapsed,
        "dodge_elapsed",
    );
    assert_float(converted.dodge_dir.x, original.dodge_dir.x, "dodge_dir.x");
    assert_float(converted.dodge_dir.y, original.dodge_dir.y, "dodge_dir.y");
}

#[test]
fn flip_reset_uses_untimed_rlbot_sentinel() {
    init_from_default(true).unwrap();

    let info = CarInfo {
        idx: 0,
        team: Team::Blue,
        config: CarBodyConfig::OCTANE,
    };
    let config = bot_config(10, "Reset", Team::Blue, 23);
    let original = CarState {
        is_on_ground: false,
        has_jumped: false,
        has_double_jumped: false,
        has_flipped: false,
        air_time_since_jump: 0.0,
        ..CarState::default()
    };

    let player = car_to_player_info(&info, &original, &config).unwrap();
    assert_eq!(player.dodge_timeout, -1.0);

    let converted = round_trip(&info, original, &config);
    assert!(!converted.has_jumped);
    assert_float(converted.air_time_since_jump, 0.0, "air_time_since_jump");
}
