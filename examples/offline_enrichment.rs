//! Enrich synthetic RLBot packets without an RLBot server.
//!
//! Run from the repository root so RocketSim can find `collision_meshes/`:
//!
//! ```sh
//! cargo run --example offline_enrichment
//! ```
//!
//! This mirrors what a bot does each tick: build a [`MatchContext`] once,
//! feed every [`GamePacket`] to [`GameStateEnricher::update`], then read the
//! RocketSim-derived contacts through the stable `player_id` lookup. The
//! second frame probes RocketSim once, so wheel contacts appear only after two
//! packets at driving height.

use rlbot_rocketsim::rlbot::flat::{
    BallInfo, BoostPadState, BoxShape, CollisionShape, CustomBot, FieldInfo, GameMode as RlbotMode,
    GamePacket, MatchConfiguration, MatchPhase, Physics, PlayerClass, PlayerConfiguration,
    PlayerInfo, PlayerLoadout, SphereShape, Vector3,
};
use rlbot_rocketsim::rocketsim::{CarBodyConfig, init_from_default};
use rlbot_rocketsim::to_rlbot::car_to_player_info_with_history;
use rlbot_rocketsim::{GameStateEnricher, MatchContext};

fn octane_config(player_id: i32) -> PlayerConfiguration {
    PlayerConfiguration {
        variety: PlayerClass::CustomBot(Box::new(CustomBot {
            name: "Offline Example".into(),
            loadout: Some(Box::new(PlayerLoadout {
                car_id: 23, // Octane
                ..PlayerLoadout::default()
            })),
            ..CustomBot::default()
        })),
        team: 0,
        player_id,
    }
}

fn octane_player(player_id: i32) -> PlayerInfo {
    let body = CarBodyConfig::OCTANE;
    PlayerInfo {
        player_id,
        team: 0,
        physics: Physics {
            location: Vector3 {
                x: 0.0,
                y: -2_000.0,
                z: 17.0, // driving height: the probe finds ground here
            },
            ..Physics::default()
        },
        hitbox: Box::new(BoxShape {
            length: body.hitbox_size.x,
            width: body.hitbox_size.y,
            height: body.hitbox_size.z,
        }),
        hitbox_offset: Vector3 {
            x: body.hitbox_pos_offset.x,
            y: body.hitbox_pos_offset.y,
            z: body.hitbox_pos_offset.z,
        },
        boost: 50.0,
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    }
}

fn packet_for_frame(frame: u32, player: PlayerInfo, num_pads: usize) -> GamePacket {
    let mut packet = GamePacket::default();
    packet.match_info.frame_num = frame;
    packet.match_info.match_phase = MatchPhase::Active;
    packet.match_info.world_gravity_z = -650.0;
    packet.players.push(player);
    packet.balls.push(BallInfo {
        physics: Physics::default(),
        shape: CollisionShape::SphereShape(Box::new(SphereShape { diameter: 182.0 })),
        charge_level: -1,
        target_speed: 0.0,
    });
    packet.boost_pads = (0..num_pads)
        .map(|_| BoostPadState {
            is_active: true,
            timer: 0.0,
        })
        .collect();
    packet
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_from_default(true)?;

    // 1. Static setup once per match.
    let match_config = MatchConfiguration {
        game_mode: RlbotMode::Soccar,
        player_configurations: vec![octane_config(7)],
        ..MatchConfiguration::default()
    };
    let context = MatchContext::new(&match_config, &FieldInfo::default())?;
    let mut enricher = GameStateEnricher::from_match_context(context);
    let num_pads = enricher.arena().num_boost_pads();

    // 2. One `update` per packet. The first packet restores authoritative
    //    state; the second also probes RocketSim once for contacts.
    for frame in 1..=2 {
        let packet = packet_for_frame(frame, octane_player(7), num_pads);
        let mappings = enricher.update(&packet)?;
        println!("frame {frame}: {mappings:?}");
    }

    // 3. Prefer the `player_id` lookup: packet slots may reorder, IDs do not.
    let car = enricher
        .car_state_by_player_id(7)
        .expect("player 7 is present");
    let contacts = car
        .wheels_with_contact
        .iter()
        .filter(|w| w.is_some())
        .count();
    println!(
        "wheels in contact: {contacts}/4 (is_on_ground={})",
        car.is_on_ground
    );
    println!("position kept from packet: {:?}", car.phys.pos);

    // 4. Keep the history when converting back so jump timing survives.
    let history = enricher
        .car_conversion_history_by_player_id(7)
        .expect("history");
    let (info, state) = enricher.arena().get_car_info_and_state(0);
    let back = car_to_player_info_with_history(info, state, &octane_config(7), history)?;
    println!("round-tripped air state: {:?}", back.air_state);
    println!("round-tripped dodge timeout: {}", back.dodge_timeout);

    Ok(())
}
