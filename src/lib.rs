//! Conversions and stateful enrichment between RLBot packets and RocketSim.
//!
//! RLBot describes a match from the outside: car and ball physics, inputs,
//! boost, and jump/dodge flags. RocketSim describes it from the inside:
//! wheel contacts, contact normals, and other simulation history that RLBot
//! never sends. This crate bridges the two without pretending either side can
//! fully represent the other.
//!
//! # Which direction do you need?
//!
//! - **RLBot → RocketSim:** feed live [`GameStateEnricher`] with each
//!   [`rlbot::flat::GamePacket`] to get enriched [`rocketsim::CarState`] values
//!   that keep packet physics but add RocketSim contacts. Start with
//!   [`MatchContext`] when you have static match data, or with
//!   [`GameStateEnricher::new`] for quick experiments.
//! - **RocketSim → RLBot:** convert simulated cars back into
//!   [`rlbot::flat::PlayerInfo`] with [`to_rlbot::ArenaExt`],
//!   [`to_rlbot::CarInfoExt`], or [`to_rlbot::car_to_player_info_with_history`].
//!   Stateless conversion is conservative on purpose; pass a retained
//!   [`CarConversionHistory`] when jump timing must round-trip exactly.
//! - **Car bodies:** [`body::car_body_config_for_product_id`] maps an RLBot car
//!   product ID to the RocketSim hitbox family used for that car.
//!
//! # Quick start: enrich packets
//!
//! ```rust
//! use rlbot_rocketsim::rlbot::flat::{GamePacket, MatchPhase, PlayerInfo};
//! use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
//! use rlbot_rocketsim::GameStateEnricher;
//!
//! // `TheVoid` needs no collision meshes, so it is ideal for a first experiment.
//! // Use `Soccar` plus `rocketsim::init_from_default(true)` for real ground contacts.
//! let arena = Arena::new(GameMode::TheVoid);
//! let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::OCTANE);
//!
//! let mut packet = GamePacket::default();
//! packet.match_info.frame_num = 1;
//! packet.match_info.match_phase = MatchPhase::Active;
//! packet.players.push(PlayerInfo {
//!     player_id: 7,
//!     team: 0,
//!     demolished_timeout: -1.0,
//!     dodge_timeout: -1.0,
//!     ..PlayerInfo::default()
//! });
//!
//! let players = enricher.update(&packet).expect("valid packet");
//! let car = enricher.car_state(players[0].player_index).unwrap();
//! assert_eq!(car.phys.pos.x, 0.0);
//! ```
//!
//! RLBot stays authoritative for physics, boost, and jump/dodge flags, while
//! the returned [`rocketsim::CarState`] carries RocketSim-derived wheel
//! contacts and ground state. Feed every new packet to `update`; the enricher
//! probes RocketSim at most once per new active frame.
//!
//! # Quick start: convert back to RLBot
//!
//! ```rust
//! use rlbot_rocketsim::rlbot::flat::{CustomBot, PlayerClass, PlayerConfiguration, PlayerLoadout};
//! use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
//! use rlbot_rocketsim::to_rlbot::ArenaExt;
//!
//! let mut arena = Arena::new(GameMode::TheVoid);
//! let car_index = arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
//!
//! let match_config = rlbot_rocketsim::rlbot::flat::MatchConfiguration {
//!     player_configurations: vec![PlayerConfiguration {
//!         variety: PlayerClass::CustomBot(Box::new(CustomBot {
//!             name: "Example".into(),
//!             loadout: Some(Box::new(PlayerLoadout {
//!                 car_id: 23, // Octane
//!                 ..PlayerLoadout::default()
//!             })),
//!             ..CustomBot::default()
//!         })),
//!         team: Team::Blue as u32,
//!         player_id: 7,
//!     }],
//!     ..Default::default()
//! };
//!
//! let players = arena.to_rlbot_players(&match_config).unwrap();
//! assert_eq!(players[car_index].player_id, 7);
//! ```
//!
//! # Concepts worth knowing up front
//!
//! - **Packet slot vs. participant:** `GamePacket.players[N]` is a control slot
//!   that can reorder. `PlayerInfo.player_id` is the stable participant ID.
//!   Prefer `car_state_by_player_id` and `car_conversion_history_by_player_id`
//!   when tracking someone across packets.
//! - **Phases and frames:** only `Kickoff` and `Active` advance RocketSim.
//!   Repeated, paused, or out-of-order frames never step more than once, and
//!   gaps do not replay missing packets.
//! - **History:** a bare `CarState` cannot recover the initial-jump hold
//!   extension inside `dodge_timeout` or a transient `DoubleJumping` force.
//!   Keep the [`CarConversionHistory`] returned by the enricher for exact
//!   conversion back to RLBot.
//! - **Scope:** standard Soccar without mutators is the supported path. See
//!   `README.md` for the full list of limitations and runnable `examples/`.
//!
//! [`rlbot::flat::GamePacket`]: rlbot::flat::GamePacket

pub mod from_rlbot;
pub mod match_context;
pub mod to_rlbot;

pub mod body;
mod common;

pub use from_rlbot::{EnrichedPlayer, EnrichmentError, GameStateEnricher};
pub use match_context::{MatchContext, MatchContextError};
pub use rlbot;
pub use rocketsim;
pub use to_rlbot::CarConversionHistory;
