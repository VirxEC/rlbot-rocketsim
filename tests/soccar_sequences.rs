use rlbot_rocketsim::GameStateEnricher;
use rlbot_rocketsim::rlbot::flat::{
    AirState, GamePacket, MatchPhase, Physics, PlayerInfo, Vector3,
};
use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};

const TICK_TIME: f32 = 1.0 / 120.0;

fn player(player_id: i32) -> PlayerInfo {
    PlayerInfo {
        player_id,
        team: 0,
        physics: Physics {
            location: Vector3 {
                x: 0.0,
                y: 0.0,
                z: 1_000.0,
            },
            ..Physics::default()
        },
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    }
}

fn packet(frame: u32, phase: MatchPhase, players: Vec<PlayerInfo>) -> GamePacket {
    GamePacket {
        players,
        match_info: Box::new(rlbot_rocketsim::rlbot::flat::MatchInfo {
            frame_num: frame,
            match_phase: phase,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn jump_hold_release_and_double_jump_sequence() {
    let arena = Arena::new(GameMode::TheVoid);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
    let mut jumping = player(7);
    jumping.air_state = AirState::Jumping;
    jumping.has_jumped = true;
    jumping.last_input.jump = true;

    for frame in 1..=12 {
        enricher
            .update(&packet(frame, MatchPhase::Active, vec![jumping.clone()]))
            .unwrap();
    }

    let held_duration = enricher.initial_jump_duration(0).unwrap();
    assert!(held_duration >= 11.0 * TICK_TIME);
    assert!(held_duration <= 0.2);

    let mut airborne = jumping;
    airborne.air_state = AirState::InAir;
    airborne.last_input.jump = false;
    airborne.dodge_timeout = 1.25 + held_duration - 0.05;
    enricher
        .update(&packet(13, MatchPhase::Active, vec![airborne.clone()]))
        .unwrap();

    let state = enricher.car_state(0).unwrap();
    assert!(!state.is_jumping);
    assert_eq!(state.jump_time, 0.0);
    assert!((state.air_time_since_jump - 0.05).abs() < 1e-5);

    airborne.air_state = AirState::DoubleJumping;
    airborne.has_double_jumped = true;
    airborne.dodge_timeout = -1.0;
    airborne.last_input.jump = true;
    enricher
        .update(&packet(14, MatchPhase::Active, vec![airborne]))
        .unwrap();

    let state = enricher.car_state(0).unwrap();
    assert!(state.has_double_jumped);
    assert!(!state.is_jumping);
    assert!(!state.is_flipping);
}

#[test]
fn packet_gap_and_phase_sequence_does_not_replay_current_state() {
    let arena = Arena::new(GameMode::TheVoid);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);

    enricher
        .update(&packet(1, MatchPhase::Active, vec![player(7)]))
        .unwrap();
    let initial_tick = enricher.arena().tick_count();

    enricher
        .update(&packet(20, MatchPhase::Active, vec![player(7)]))
        .unwrap();
    assert_eq!(enricher.arena().tick_count(), initial_tick + 1);

    enricher
        .update(&packet(21, MatchPhase::Paused, vec![player(7)]))
        .unwrap();
    assert_eq!(enricher.arena().tick_count(), initial_tick + 1);

    enricher
        .update(&packet(22, MatchPhase::Kickoff, vec![player(7)]))
        .unwrap();
    assert_eq!(enricher.arena().tick_count(), initial_tick + 2);
}

#[test]
fn demolition_and_join_sequence_preserves_only_valid_history() {
    let arena = Arena::new(GameMode::TheVoid);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);

    enricher
        .update(&packet(1, MatchPhase::Active, vec![player(7)]))
        .unwrap();
    let mut state = *enricher.car_state(0).unwrap();
    state.is_on_ground = true;
    state.wheels_with_contact = [true; 4];
    state.world_contact_normal = Some(glam::Vec3A::Z);
    state.handbrake_val = 0.4;
    enricher.arena_mut().set_car_state(0, state);

    let mut joined = packet(2, MatchPhase::Paused, vec![player(7), player(9)]);
    enricher.update(&joined).unwrap();
    assert_eq!(enricher.arena().num_cars(), 2);
    assert_eq!(
        enricher.car_state_by_player_id(7).unwrap().handbrake_val,
        0.4
    );

    joined.match_info.frame_num = 3;
    joined.match_info.match_phase = MatchPhase::Active;
    joined.players[0].demolished_timeout = 2.0;
    enricher.update(&joined).unwrap();

    let demoed = enricher.car_state_by_player_id(7).unwrap();
    assert!(demoed.is_demoed);
    assert!(!demoed.is_on_ground);
    assert_eq!(demoed.wheels_with_contact, [false; 4]);
    assert_eq!(demoed.world_contact_normal, None);
}
