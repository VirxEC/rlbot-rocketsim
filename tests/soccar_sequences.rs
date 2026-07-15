use rlbot_rocketsim::GameStateEnricher;
use rlbot_rocketsim::rlbot::flat::{
    AirState, CustomBot, GamePacket, MatchPhase, Physics, PlayerClass, PlayerConfiguration,
    PlayerInfo, PlayerLoadout, Vector3,
};
use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
use rlbot_rocketsim::to_rlbot::car_to_player_info_with_history;

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

fn player_config(player_id: i32) -> PlayerConfiguration {
    PlayerConfiguration {
        variety: PlayerClass::CustomBot(Box::new(CustomBot {
            loadout: Some(Box::new(PlayerLoadout {
                car_id: 23,
                ..PlayerLoadout::default()
            })),
            ..CustomBot::default()
        })),
        team: Team::Blue as u32,
        player_id,
    }
}

fn converted_air_state(enricher: &GameStateEnricher, player_id: i32) -> AirState {
    let (info, state) = enricher.arena().get_car_info_and_state(0);
    car_to_player_info_with_history(
        info,
        state,
        &player_config(player_id),
        enricher
            .car_conversion_history_by_player_id(player_id)
            .unwrap(),
    )
    .unwrap()
    .air_state
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

    assert!(
        enricher
            .car_conversion_history(0)
            .unwrap()
            .double_jump_active
    );
    assert_eq!(converted_air_state(&enricher, 7), AirState::DoubleJumping);
}

#[test]
fn authoritative_double_jump_transition_clears_active_history() {
    let arena = Arena::new(GameMode::TheVoid);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
    let mut double_jumping = player(7);
    double_jumping.air_state = AirState::DoubleJumping;
    double_jumping.has_jumped = true;
    double_jumping.has_double_jumped = true;

    enricher
        .update(&packet(1, MatchPhase::Active, vec![double_jumping.clone()]))
        .unwrap();
    assert!(
        enricher
            .car_conversion_history(0)
            .unwrap()
            .double_jump_active
    );
    assert_eq!(converted_air_state(&enricher, 7), AirState::DoubleJumping);

    double_jumping.air_state = AirState::InAir;
    enricher
        .update(&packet(2, MatchPhase::Active, vec![double_jumping]))
        .unwrap();
    assert!(
        !enricher
            .car_conversion_history(0)
            .unwrap()
            .double_jump_active
    );
    assert_eq!(converted_air_state(&enricher, 7), AirState::InAir);
}

#[test]
fn authoritative_double_jump_state_ignores_packet_gap_and_pause() {
    let arena = Arena::new(GameMode::TheVoid);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
    let mut double_jumping = player(7);
    double_jumping.air_state = AirState::DoubleJumping;
    double_jumping.has_jumped = true;
    double_jumping.has_double_jumped = true;

    enricher
        .update(&packet(1, MatchPhase::Active, vec![double_jumping.clone()]))
        .unwrap();
    enricher
        .update(&packet(
            1_000,
            MatchPhase::Paused,
            vec![double_jumping.clone()],
        ))
        .unwrap();
    assert!(
        enricher
            .car_conversion_history_by_player_id(7)
            .unwrap()
            .double_jump_active
    );
    assert_eq!(converted_air_state(&enricher, 7), AirState::DoubleJumping);

    double_jumping.air_state = AirState::InAir;
    enricher
        .update(&packet(2_000, MatchPhase::Active, vec![double_jumping]))
        .unwrap();
    assert!(
        !enricher
            .car_conversion_history(0)
            .unwrap()
            .double_jump_active
    );
    assert_eq!(converted_air_state(&enricher, 7), AirState::InAir);
}

#[test]
fn airborne_reset_with_default_timeout_retains_untimed_history() {
    let arena = Arena::new(GameMode::TheVoid);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
    let mut reset = player(7);
    reset.dodge_timeout = 0.0;
    reset.air_state = AirState::InAir;

    enricher
        .update(&packet(1, MatchPhase::Active, vec![reset]))
        .unwrap();

    let (info, state) = enricher.arena().get_car_info_and_state(0);
    let converted = car_to_player_info_with_history(
        info,
        state,
        &player_config(7),
        enricher.car_conversion_history_by_player_id(7).unwrap(),
    )
    .unwrap();
    assert_eq!(converted.air_state, AirState::InAir);
    assert_eq!(converted.dodge_timeout, -1.0);
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
