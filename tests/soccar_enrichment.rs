use rlbot_rocketsim::GameStateEnricher;
use rlbot_rocketsim::rlbot::flat::{AirState, GamePacket, Physics, PlayerInfo, Vector3};
use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, init_from_default};

#[test]
fn derives_no_wheel_contacts_instead_of_copying_ground_air_state() {
    init_from_default(true).unwrap();

    let arena = Arena::new(GameMode::Soccar);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
    let player = PlayerInfo {
        player_id: 7,
        team: 0,
        physics: Physics {
            location: Vector3 {
                x: 0.0,
                y: -2_000.0,
                z: 1_000.0,
            },
            ..Physics::default()
        },
        air_state: AirState::OnGround,
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    };
    let mut packet = GamePacket::default();
    packet.match_info.frame_num = 1;
    packet.players.push(player);

    enricher.update(&packet).unwrap();
    let state = enricher.car_state(0).unwrap();
    assert_eq!(state.wheels_with_contact, [false; 4]);
    assert!(!state.is_on_ground);
    assert_eq!(state.phys.pos.z, 1_000.0);
}

#[test]
fn first_packet_does_not_probe_forward_from_authoritative_state() {
    init_from_default(true).unwrap();

    let arena = Arena::new(GameMode::Soccar);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
    let player = PlayerInfo {
        player_id: 7,
        team: 0,
        physics: Physics {
            location: Vector3 {
                x: 0.0,
                y: -2_000.0,
                z: 17.0,
            },
            ..Physics::default()
        },
        air_state: AirState::InAir,
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    };
    let mut packet = GamePacket::default();
    packet.match_info.frame_num = 1;
    packet.match_info.match_phase = rlbot_rocketsim::rlbot::flat::MatchPhase::Active;
    packet.match_info.world_gravity_z = -650.0;
    packet.players.push(player);

    enricher.update(&packet).unwrap();

    let state = enricher.car_state(0).unwrap();
    assert_eq!(state.wheels_with_contact, [false; 4]);
    assert!(!state.is_on_ground);
}

#[test]
fn derives_ground_state_from_wheel_contacts_despite_in_air_state() {
    init_from_default(true).unwrap();

    let arena = Arena::new(GameMode::Soccar);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
    let player = PlayerInfo {
        player_id: 7,
        team: 0,
        physics: Physics {
            location: Vector3 {
                x: 0.0,
                y: -2_000.0,
                z: 17.0,
            },
            ..Physics::default()
        },
        air_state: AirState::InAir,
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    };

    for frame in 1..=2 {
        let mut packet = GamePacket::default();
        packet.match_info.frame_num = frame;
        packet.match_info.match_phase = rlbot_rocketsim::rlbot::flat::MatchPhase::Active;
        packet.match_info.world_gravity_z = -650.0;
        packet.players.push(player.clone());
        enricher.update(&packet).unwrap();
    }

    let state = enricher.car_state(0).unwrap();
    assert_eq!(state.wheels_with_contact, [true; 4]);
    assert!(state.is_on_ground);
    assert_eq!(state.phys.pos.z, 17.0);
}

#[test]
fn current_packet_uses_contacts_from_the_prior_interval() {
    init_from_default(true).unwrap();

    let arena = Arena::new(GameMode::Soccar);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
    let mut player = PlayerInfo {
        player_id: 7,
        team: 0,
        physics: Physics {
            location: Vector3 {
                x: 0.0,
                y: -2_000.0,
                z: 1_000.0,
            },
            ..Physics::default()
        },
        air_state: AirState::InAir,
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    };
    let mut first = GamePacket::default();
    first.match_info.frame_num = 1;
    first.match_info.match_phase = rlbot_rocketsim::rlbot::flat::MatchPhase::Active;
    first.match_info.world_gravity_z = -650.0;
    first.players.push(player.clone());
    enricher.update(&first).unwrap();

    player.physics.location.z = 17.0;
    let mut second = GamePacket::default();
    second.match_info.frame_num = 2;
    second.match_info.match_phase = rlbot_rocketsim::rlbot::flat::MatchPhase::Active;
    second.match_info.world_gravity_z = -650.0;
    second.players.push(player);
    enricher.update(&second).unwrap();

    let state = enricher.car_state(0).unwrap();
    assert_eq!(state.phys.pos.z, 17.0);
    assert_eq!(state.wheels_with_contact, [false; 4]);
    assert!(!state.is_on_ground);
}
