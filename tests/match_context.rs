use rlbot_rocketsim::rlbot::flat::{
    BoostPad, BoostPadState, BoxShape, CollisionShape, CustomBot, FieldInfo, GameMode, GamePacket,
    MatchConfiguration, Physics, PlayerClass, PlayerConfiguration, PlayerInfo, PlayerLoadout,
    SphereShape, Vector3,
};
use rlbot_rocketsim::rocketsim::{CarBodyConfig, init_from_default};
use rlbot_rocketsim::{EnrichmentError, GameStateEnricher, MatchContext, MatchContextError};

fn match_config(car_id: u32) -> MatchConfiguration {
    MatchConfiguration {
        game_mode: GameMode::Soccar,
        player_configurations: vec![PlayerConfiguration {
            variety: PlayerClass::CustomBot(Box::new(CustomBot {
                loadout: Some(Box::new(PlayerLoadout {
                    car_id,
                    ..PlayerLoadout::default()
                })),
                ..CustomBot::default()
            })),
            team: 0,
            player_id: 7,
        }],
        ..MatchConfiguration::default()
    }
}

fn field_info() -> FieldInfo {
    FieldInfo {
        boost_pads: vec![BoostPad {
            location: Vector3 {
                x: 100.0,
                y: 200.0,
                z: 70.0,
            },
            is_full_boost: true,
        }],
        ..FieldInfo::default()
    }
}

fn standard_ball() -> rlbot_rocketsim::rlbot::flat::BallInfo {
    rlbot_rocketsim::rlbot::flat::BallInfo {
        physics: Physics::default(),
        shape: CollisionShape::SphereShape(Box::new(SphereShape { diameter: 182.0 })),
        charge_level: -1,
        target_speed: 0.0,
    }
}

fn dominus_player() -> PlayerInfo {
    let config = CarBodyConfig::DOMINUS;
    PlayerInfo {
        physics: Physics {
            location: Vector3 {
                x: 10.0,
                y: 20.0,
                z: 30.0,
            },
            ..Physics::default()
        },
        hitbox: Box::new(BoxShape {
            length: config.hitbox_size.x,
            width: config.hitbox_size.y,
            height: config.hitbox_size.z,
        }),
        hitbox_offset: Vector3 {
            x: config.hitbox_pos_offset.x,
            y: config.hitbox_pos_offset.y,
            z: config.hitbox_pos_offset.z,
        },
        player_id: 7,
        team: 0,
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    }
}

#[test]
fn builds_match_from_config_field_and_packet_data() {
    init_from_default(true).unwrap();

    let context = MatchContext::new(&match_config(29), &field_info()).unwrap();
    let mut enricher = GameStateEnricher::from_match_context(context);
    let mut packet = GamePacket::default();
    packet.match_info.frame_num = 1;
    packet.players.push(dominus_player());
    packet.boost_pads.push(BoostPadState {
        is_active: false,
        timer: 4.25,
    });
    let mut ball = standard_ball();
    ball.physics.location = Vector3 {
        x: 500.0,
        y: -600.0,
        z: 700.0,
    };
    packet.balls.push(ball);

    enricher.update(&packet).unwrap();

    assert_eq!(
        enricher.arena().get_car_info(0).config,
        CarBodyConfig::DOMINUS
    );
    assert_eq!(enricher.arena().get_car_info(0).idx, 0);
    assert_eq!(enricher.ball_state().phys.pos.x, 500.0);
    // RLBot's timer is elapsed since pickup; RocketSim's cooldown is time remaining.
    assert!((enricher.arena().get_boost_pad_state(0).cooldown - 5.75).abs() < 1e-5);
    assert_eq!(enricher.arena().get_boost_pad_config(0).pos.x, 100.0);
}

#[test]
fn rejects_non_soccar_modes() {
    for mode in [
        GameMode::Rumble,
        GameMode::Hoops,
        GameMode::Dropshot,
        GameMode::Snowday,
        GameMode::Heatseeker,
        GameMode::Gridiron,
        GameMode::Knockout,
    ] {
        let mut config = match_config(29);
        config.game_mode = mode;
        assert_eq!(
            MatchContext::new(&config, &field_info()).unwrap_err(),
            MatchContextError::UnsupportedGameMode(mode)
        );
    }
}

#[test]
fn rejects_duplicate_configured_player_ids() {
    let mut config = match_config(29);
    config
        .player_configurations
        .push(config.player_configurations[0].clone());

    assert_eq!(
        MatchContext::new(&config, &field_info()).unwrap_err(),
        MatchContextError::DuplicatePlayerId { player_id: 7 }
    );
}

#[test]
fn requires_all_boost_pad_states() {
    init_from_default(true).unwrap();

    let context = MatchContext::new(&match_config(29), &field_info()).unwrap();
    let mut enricher = GameStateEnricher::from_match_context(context);
    let mut packet = GamePacket::default();
    packet.players.push(dominus_player());
    packet.balls.push(standard_ball());

    assert_eq!(
        enricher.update(&packet),
        Err(EnrichmentError::MatchContext(
            MatchContextError::BoostPadCountMismatch {
                packet: 0,
                arena: 1,
            }
        ))
    );
}

#[test]
fn requires_exactly_one_ball() {
    init_from_default(true).unwrap();

    let context = MatchContext::new(&match_config(29), &field_info()).unwrap();
    let mut enricher = GameStateEnricher::from_match_context(context);
    let mut packet = GamePacket::default();
    packet.players.push(dominus_player());

    assert_eq!(
        enricher.update(&packet),
        Err(EnrichmentError::BallCount { count: 0 })
    );

    packet.balls.push(standard_ball());
    packet.balls.push(packet.balls[0].clone());

    assert_eq!(
        enricher.update(&packet),
        Err(EnrichmentError::BallCount { count: 2 })
    );
}

#[test]
fn rejects_incompatible_ball_shape() {
    init_from_default(true).unwrap();

    let context = MatchContext::new(&match_config(29), &field_info()).unwrap();
    let mut enricher = GameStateEnricher::from_match_context(context);
    let mut packet = GamePacket::default();
    packet.players.push(dominus_player());
    packet.balls.push(rlbot_rocketsim::rlbot::flat::BallInfo {
        physics: Physics::default(),
        shape: CollisionShape::BoxShape(Box::new(BoxShape {
            length: 100.0,
            width: 100.0,
            height: 100.0,
        })),
        charge_level: -1,
        target_speed: 0.0,
    });

    assert!(matches!(
        enricher.update(&packet),
        Err(EnrichmentError::BallShape {
            shape: "box",
            mode: rlbot_rocketsim::rocketsim::GameMode::Soccar,
        })
    ));
    assert_eq!(enricher.arena().num_cars(), 0);
}

#[test]
fn rejects_hitbox_that_disagrees_with_loadout() {
    init_from_default(true).unwrap();

    let context = MatchContext::new(&match_config(29), &field_info()).unwrap();
    let mut enricher = GameStateEnricher::from_match_context(context);
    let mut player = dominus_player();
    player.hitbox.length = CarBodyConfig::OCTANE.hitbox_size.x;
    let mut packet = GamePacket::default();
    packet.players.push(player);
    packet.balls.push(standard_ball());
    packet.boost_pads.push(BoostPadState {
        is_active: true,
        timer: 0.0,
    });

    assert_eq!(
        enricher.update(&packet),
        Err(EnrichmentError::MatchContext(
            MatchContextError::HitboxMismatch {
                player_index: 0,
                player_id: 7,
            }
        ))
    );
}

#[test]
fn rejects_unknown_car_product_id() {
    assert!(matches!(
        MatchContext::new(&match_config(u32::MAX), &field_info()),
        Err(MatchContextError::UnknownCarProductId {
            player_id: 7,
            product_id: u32::MAX,
        })
    ));
}
