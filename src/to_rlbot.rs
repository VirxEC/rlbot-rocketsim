//! Convert RocketSim cars back into RLBot [`PlayerInfo`] values.
//!
//! A bare [`CarState`] cannot remember everything RLBot
//! needs: the variable initial-jump hold inside `dodge_timeout` and a
//! transient `DoubleJumping` force both require packet history. That is why
//! this module offers two levels:
//!
//! - **Stateless:** [`CarInfoExt`], [`ArenaExt::car_to_rlbot_player_info`], and
//!   [`ArenaExt::to_rlbot_players`] never invent timing. A finite stateless
//!   `dodge_timeout` is a lower bound (zero hold assumed), and unknown double
//!   jumps conservatively read as `InAir`.
//! - **History-aware:** [`car_to_player_info_with_history`] and
//!   [`ArenaExt::to_rlbot_players_with_history`] take the
//!   [`CarConversionHistory`] retained by [`GameStateEnricher`](crate::GameStateEnricher).
//!
//! # Example
//!
//! ```rust
//! use rlbot_rocketsim::rlbot::flat::{CustomBot, PlayerClass, PlayerConfiguration, PlayerLoadout};
//! use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
//! use rlbot_rocketsim::to_rlbot::{ArenaExt, car_to_player_info_with_history};
//! use rlbot_rocketsim::CarConversionHistory;
//!
//! let mut arena = Arena::new(GameMode::TheVoid);
//! let car_index = arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
//! let config = PlayerConfiguration {
//!     variety: PlayerClass::CustomBot(Box::new(CustomBot {
//!         name: "Example".into(),
//!         loadout: Some(Box::new(PlayerLoadout { car_id: 23, ..Default::default() })),
//!         ..Default::default()
//!     })),
//!     team: Team::Blue as u32,
//!     player_id: 7,
//! };
//!
//! // Stateless: fine for physics, boost, and inputs.
//! let stateless = arena.car_to_rlbot_player_info(car_index, &rlbot_rocketsim::rlbot::flat::MatchConfiguration {
//!     player_configurations: vec![config.clone()],
//!     ..Default::default()
//! });
//! assert!(stateless.is_ok());
//!
//! // History-aware: exact jump timing when you kept the enricher history.
//! let (info, state) = arena.get_car_info_and_state(car_index);
//! let history = CarConversionHistory::default();
//! let player = car_to_player_info_with_history(info, state, &config, history).unwrap();
//! assert_eq!(player.player_id, 7);
//! ```

use rlbot::flat::{
    AirState, BoxShape, MatchConfiguration, PlayerClass, PlayerConfiguration, PlayerInfo,
};
use rocketsim::{Arena, CarInfo, CarState};
use thiserror::Error;

use crate::body::car_body_config_for_product_id;
use crate::common::{
    MAX_JUMP_HOLD_TIME, controls_to_rlbot, physics_to_rlbot, vector2_to_rlbot, vector3_to_rlbot,
};

/// Reasons a RocketSim car cannot become an RLBot [`PlayerInfo`].
///
/// These are configuration mismatches, not simulation failures: the car index
/// has no match-config entry, the teams disagree, or the hitbox disagrees
/// with the configured car product ID.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ToRlbotError {
    /// `player_configurations` is shorter than the arena: car `N` needs
    /// `player_configurations[N]`. Add the missing entry.
    #[error("RocketSim car {car_index} has no corresponding MatchConfiguration player")]
    MissingPlayerConfiguration { car_index: usize },
    /// The arena team and the match-config team disagree for this car. Fix
    /// whichever side is stale.
    #[error(
        "RocketSim car {car_index} is on team {rocketsim_team}, but MatchConfiguration uses team {configured_team}"
    )]
    TeamMismatch {
        car_index: usize,
        rocketsim_team: u32,
        configured_team: u32,
    },
    /// The arena hitbox is not the family mapped from `product_id` (see
    /// [`car_body_config_for_product_id`]).
    /// Usually a wrong `car_id` in the loadout.
    #[error(
        "RocketSim car {car_index} body config disagrees with configured product ID {product_id}"
    )]
    BodyConfigMismatch { car_index: usize, product_id: u32 },
    /// The loadout names a car ID this crate does not map yet. Report it so
    /// the table in [`body`](crate::body) can grow.
    #[error("player {player_id} uses unknown car product ID {product_id}")]
    UnknownCarProductId { player_id: i32, product_id: u32 },
}

/// Packet-derived timing that a bare [`CarState`] forgets.
///
/// [`GameStateEnricher`](crate::GameStateEnricher) maintains this for you;
/// read it back with `car_conversion_history(_by_player_id)` and hand it to
/// [`car_to_player_info_with_history`] for an exact trip back to RLBot.
/// `Default` (all zeros/`false`) means "no history", which is exactly what the
/// stateless converters use.
///
/// # Example
///
/// ```rust
/// # use rlbot_rocketsim::CarConversionHistory;
/// let history = CarConversionHistory {
///     initial_jump_duration: 0.12,
///     double_jump_active: true,
///     flip_reset_available: false,
/// };
/// assert!(history.initial_jump_duration <= 0.2);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CarConversionHistory {
    /// How long jump was held, in seconds (clamped to 0.0–0.2 on use).
    /// Rebuilds the hold extension inside RLBot `dodge_timeout`.
    pub initial_jump_duration: f32,
    /// Whether the latest packet reported active double-jump forces. Needed
    /// because `CarState.has_double_jumped` alone cannot distinguish a fresh
    /// double jump from an old one.
    pub double_jump_active: bool,
    /// Whether an airborne car has an untimed flip reset. While set,
    /// `dodge_timeout` stays `-1` (no dodge window is running).
    pub flip_reset_available: bool,
}

/// Converts one (`CarInfo`, `CarState`) pair without retained history.
///
/// Shorthand for [`car_to_player_info`]. See the module docs for when to
/// prefer [`car_to_player_info_with_history`].
///
/// # Example
///
/// ```rust
/// # use rlbot_rocketsim::rlbot::flat::{CustomBot, PlayerClass, PlayerConfiguration, PlayerLoadout};
/// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
/// # use rlbot_rocketsim::to_rlbot::CarInfoExt;
/// # let arena = Arena::new(GameMode::TheVoid);
/// # let mut arena = arena; let car = arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
/// # let (info, state) = arena.get_car_info_and_state(car);
/// # let config = PlayerConfiguration { variety: PlayerClass::CustomBot(Box::new(CustomBot { name: "E".into(), loadout: Some(Box::new(PlayerLoadout { car_id: 23, ..Default::default() })), ..Default::default() })), team: 0, player_id: 7 };
/// let player = info.to_rlbot_player_info(state, &config).unwrap();
/// assert_eq!(player.team, 0);
/// ```
pub trait CarInfoExt {
    fn to_rlbot_player_info(
        &self,
        state: &CarState,
        player_config: &PlayerConfiguration,
    ) -> Result<PlayerInfo, ToRlbotError>;
}

impl CarInfoExt for CarInfo {
    fn to_rlbot_player_info(
        &self,
        state: &CarState,
        player_config: &PlayerConfiguration,
    ) -> Result<PlayerInfo, ToRlbotError> {
        car_to_player_info(self, state, player_config)
    }
}

/// Converts whole arenas to RLBot players, with or without history.
///
/// The stateless methods index `MatchConfiguration.player_configurations` by
/// RocketSim car index (`players[N]` ↔ car `N`). The history-aware method
/// takes one [`CarConversionHistory`] per car in the same order; short slices
/// fall back to stateless conversion per car.
///
/// # Example
///
/// ```rust
/// # use rlbot_rocketsim::rlbot::flat::{CustomBot, MatchConfiguration, PlayerClass, PlayerConfiguration, PlayerLoadout};
/// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
/// # use rlbot_rocketsim::to_rlbot::ArenaExt;
/// # let mut arena = Arena::new(GameMode::TheVoid);
/// # arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
/// # let match_config = MatchConfiguration { player_configurations: vec![PlayerConfiguration { variety: PlayerClass::CustomBot(Box::new(CustomBot { name: "E".into(), loadout: Some(Box::new(PlayerLoadout { car_id: 23, ..Default::default() })), ..Default::default() })), team: 0, player_id: 7 }], ..Default::default() };
/// let players = arena.to_rlbot_players(&match_config).unwrap();
/// assert_eq!(players.len(), 1);
/// ```
pub trait ArenaExt {
    /// Converts one car, looking up its config at
    /// `MatchConfiguration.player_configurations[CarInfo::idx]`.
    ///
    /// Stateless: exact for physics/boost/inputs, conservative for
    /// `AirState::DoubleJumping` and the jump-hold part of `dodge_timeout`.
    fn car_to_rlbot_player_info(
        &self,
        car_index: usize,
        match_config: &MatchConfiguration,
    ) -> Result<PlayerInfo, ToRlbotError>;

    /// Converts every car in index order without history.
    ///
    /// Suitable for `GamePacket.players` (`players[N]` ↔ car `N`), with the
    /// same conservative jump-timing caveats as
    /// [`car_to_player_info`].
    fn to_rlbot_players(
        &self,
        match_config: &MatchConfiguration,
    ) -> Result<Vec<PlayerInfo>, ToRlbotError>;

    /// Converts every car with its retained history (`histories[car_index]`).
    ///
    /// Missing entries default to stateless conversion, so you can pass a
    /// shorter slice while migrating. For exact results, pass the histories
    /// from `GameStateEnricher::car_conversion_history` in car order.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rlbot_rocketsim::rlbot::flat::{CustomBot, MatchConfiguration, PlayerClass, PlayerConfiguration, PlayerLoadout};
    /// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
    /// # use rlbot_rocketsim::to_rlbot::ArenaExt;
    /// # use rlbot_rocketsim::CarConversionHistory;
    /// # let mut arena = Arena::new(GameMode::TheVoid);
    /// # arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
    /// # let match_config = MatchConfiguration { player_configurations: vec![PlayerConfiguration { variety: PlayerClass::CustomBot(Box::new(CustomBot { name: "E".into(), loadout: Some(Box::new(PlayerLoadout { car_id: 23, ..Default::default() })), ..Default::default() })), team: 0, player_id: 7 }], ..Default::default() };
    /// let histories = vec![CarConversionHistory::default()];
    /// let players = arena.to_rlbot_players_with_history(&match_config, &histories).unwrap();
    /// assert_eq!(players.len(), 1);
    /// ```
    fn to_rlbot_players_with_history(
        &self,
        match_config: &MatchConfiguration,
        histories: &[CarConversionHistory],
    ) -> Result<Vec<PlayerInfo>, ToRlbotError>;
}

impl ArenaExt for Arena {
    fn car_to_rlbot_player_info(
        &self,
        car_index: usize,
        match_config: &MatchConfiguration,
    ) -> Result<PlayerInfo, ToRlbotError> {
        let (info, state) = self.get_car_info_and_state(car_index);
        debug_assert_eq!(info.idx, car_index);
        let player_config = match_config.player_configurations.get(info.idx).ok_or(
            ToRlbotError::MissingPlayerConfiguration {
                car_index: info.idx,
            },
        )?;
        car_to_player_info(info, state, player_config)
    }

    fn to_rlbot_players(
        &self,
        match_config: &MatchConfiguration,
    ) -> Result<Vec<PlayerInfo>, ToRlbotError> {
        (0..self.num_cars())
            .map(|car_index| self.car_to_rlbot_player_info(car_index, match_config))
            .collect()
    }

    fn to_rlbot_players_with_history(
        &self,
        match_config: &MatchConfiguration,
        histories: &[CarConversionHistory],
    ) -> Result<Vec<PlayerInfo>, ToRlbotError> {
        (0..self.num_cars())
            .map(|car_index| {
                let (info, state) = self.get_car_info_and_state(car_index);
                let player_config = match_config.player_configurations.get(info.idx).ok_or(
                    ToRlbotError::MissingPlayerConfiguration {
                        car_index: info.idx,
                    },
                )?;
                let history = histories.get(car_index).copied().unwrap_or_default();
                car_to_player_info_with_history(info, state, player_config, history)
            })
            .collect()
    }
}

/// Converts one car without retained history (conservative jump timing).
///
/// Physics, boost, inputs, demolition, and dodge direction round-trip;
/// `AirState::Jumping`/`Dodging` win over `OnGround`, and ground state comes
/// from RocketSim wheels. `dodge_timeout` assumes a zero jump hold, so a
/// finite result is a lower bound and `-1` may hide an unknown hold extension.
/// Use [`car_to_player_info_with_history`] when you kept the enricher history.
///
/// # Example
///
/// ```rust
/// # use rlbot_rocketsim::rlbot::flat::{CustomBot, PlayerClass, PlayerConfiguration, PlayerLoadout};
/// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
/// # use rlbot_rocketsim::to_rlbot::car_to_player_info;
/// # let mut arena = Arena::new(GameMode::TheVoid);
/// # let car = arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
/// # let (info, state) = arena.get_car_info_and_state(car);
/// # let config = PlayerConfiguration { variety: PlayerClass::CustomBot(Box::new(CustomBot { name: "E".into(), loadout: Some(Box::new(PlayerLoadout { car_id: 23, ..Default::default() })), ..Default::default() })), team: 0, player_id: 7 };
/// let player = car_to_player_info(info, state, &config).unwrap();
/// assert_eq!(player.player_id, 7);
/// ```
pub fn car_to_player_info(
    info: &CarInfo,
    state: &CarState,
    player_config: &PlayerConfiguration,
) -> Result<PlayerInfo, ToRlbotError> {
    car_to_player_info_with_history(info, state, player_config, CarConversionHistory::default())
}

/// Converts one car with retained [`CarConversionHistory`] for exact timing.
///
/// On top of [`car_to_player_info`], this restores the initial-jump hold
/// extension inside `dodge_timeout`, reports a transient `DoubleJumping` air
/// state, and keeps `dodge_timeout` at `-1` while a flip reset is available.
/// Histories are clamped (hold to 0.0–0.2 s), so stale values degrade
/// gracefully instead of panicking.
///
/// # Example
///
/// ```rust
/// # use rlbot_rocketsim::rlbot::flat::{CustomBot, PlayerClass, PlayerConfiguration, PlayerLoadout};
/// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, Team};
/// # use rlbot_rocketsim::to_rlbot::car_to_player_info_with_history;
/// # use rlbot_rocketsim::CarConversionHistory;
/// # let mut arena = Arena::new(GameMode::TheVoid);
/// # let car = arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
/// # let (info, state) = arena.get_car_info_and_state(car);
/// # let config = PlayerConfiguration { variety: PlayerClass::CustomBot(Box::new(CustomBot { name: "E".into(), loadout: Some(Box::new(PlayerLoadout { car_id: 23, ..Default::default() })), ..Default::default() })), team: 0, player_id: 7 };
/// let history = CarConversionHistory { initial_jump_duration: 0.1, ..Default::default() };
/// let player = car_to_player_info_with_history(info, state, &config, history).unwrap();
/// assert_eq!(player.player_id, 7);
/// ```
pub fn car_to_player_info_with_history(
    info: &CarInfo,
    state: &CarState,
    player_config: &PlayerConfiguration,
    history: CarConversionHistory,
) -> Result<PlayerInfo, ToRlbotError> {
    validate_player_config(info, player_config)?;

    let air_state = if state.is_jumping {
        AirState::Jumping
    } else if state.is_flipping {
        AirState::Dodging
    } else if history.double_jump_active && state.has_double_jumped {
        AirState::DoubleJumping
    } else if state.is_on_ground {
        AirState::OnGround
    } else {
        AirState::InAir
    };

    let initial_jump_duration = history.initial_jump_duration.clamp(0.0, MAX_JUMP_HOLD_TIME);
    let dodge_time_remaining = rocketsim::consts::car::jump::DOUBLEJUMP_MAX_DELAY
        + initial_jump_duration
        - state.air_time_since_jump;
    let dodge_timeout = if air_state == AirState::OnGround
        || air_state == AirState::Jumping
        || history.flip_reset_available
        || !state.has_jumped
        || state.has_double_jumped
        || state.has_flipped
        || dodge_time_remaining <= 0.0
    {
        -1.0
    } else {
        dodge_time_remaining
    };

    let (name, is_bot) = player_name_and_bot(&player_config.variety, info.idx);
    let dodge_dir = vector2_to_rlbot(state.flip_rel_torque.y, -state.flip_rel_torque.x);
    let hitbox = info.config.hitbox_size;

    Ok(PlayerInfo {
        physics: physics_to_rlbot(state.phys),
        hitbox: Box::new(BoxShape {
            length: hitbox.x,
            width: hitbox.y,
            height: hitbox.z,
        }),
        hitbox_offset: vector3_to_rlbot(info.config.hitbox_pos_offset),
        air_state,
        dodge_timeout,
        demolished_timeout: if state.is_demoed {
            state.demo_respawn_timer
        } else {
            -1.0
        },
        is_supersonic: state.is_supersonic,
        is_bot,
        name,
        team: info.team as u32,
        boost: state.boost,
        player_id: player_config.player_id,
        last_input: controls_to_rlbot(state.controls),
        has_jumped: state.has_jumped,
        has_double_jumped: state.has_double_jumped,
        has_dodged: state.has_flipped,
        dodge_elapsed: if state.is_on_ground {
            0.0
        } else {
            state.flip_time
        },
        dodge_dir,
        ..PlayerInfo::default()
    })
}

fn validate_player_config(
    info: &CarInfo,
    player_config: &PlayerConfiguration,
) -> Result<(), ToRlbotError> {
    let rocketsim_team = info.team as u32;
    if rocketsim_team != player_config.team {
        return Err(ToRlbotError::TeamMismatch {
            car_index: info.idx,
            rocketsim_team,
            configured_team: player_config.team,
        });
    }

    if let Some(product_id) = player_product_id(&player_config.variety) {
        let expected = car_body_config_for_product_id(product_id).ok_or(
            ToRlbotError::UnknownCarProductId {
                player_id: player_config.player_id,
                product_id,
            },
        )?;
        if info.config != expected {
            return Err(ToRlbotError::BodyConfigMismatch {
                car_index: info.idx,
                product_id,
            });
        }
    }

    Ok(())
}

fn player_product_id(player_class: &PlayerClass) -> Option<u32> {
    match player_class {
        PlayerClass::CustomBot(bot) => bot.loadout.as_deref().map(|loadout| loadout.car_id),
        PlayerClass::PsyonixBot(bot) => bot.loadout.as_deref().map(|loadout| loadout.car_id),
        PlayerClass::Human(_) => None,
    }
}

fn player_name_and_bot(player_class: &PlayerClass, player_index: usize) -> (String, bool) {
    match player_class {
        PlayerClass::CustomBot(bot) => (bot.name.clone(), true),
        PlayerClass::PsyonixBot(bot) => (bot.name.clone(), true),
        PlayerClass::Human(_) => (format!("Human {player_index}"), false),
    }
}

#[cfg(test)]
mod tests {
    use glam::Vec3A;
    use rlbot::flat::{CustomBot, PlayerLoadout};
    use rocketsim::{CarBodyConfig, RaycastHitInfo, Team, UserInfoType, consts};

    use super::*;

    const DEFAULT_RAYCAST_HIT_INFO: RaycastHitInfo = RaycastHitInfo {
        hit_point: Vec3A::ZERO,
        hit_normal: Vec3A::Z,
        hit_fraction: 0.0,
        user_info: UserInfoType::None,
    };

    fn player(team: u32, car_id: u32) -> PlayerConfiguration {
        PlayerConfiguration {
            variety: PlayerClass::CustomBot(Box::new(CustomBot {
                name: "Test".into(),
                loadout: Some(Box::new(PlayerLoadout {
                    car_id,
                    ..PlayerLoadout::default()
                })),
                ..CustomBot::default()
            })),
            team,
            player_id: 7,
        }
    }

    #[test]
    fn active_air_state_takes_precedence_over_wheel_contact() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let mut state = CarState {
            wheels_with_contact: [Some(DEFAULT_RAYCAST_HIT_INFO); 4],
            is_on_ground: true,
            is_jumping: true,
            ..CarState::default()
        };

        let jumping = car_to_player_info(&info, &state, &player(0, 23)).unwrap();
        assert_eq!(jumping.air_state, AirState::Jumping);

        state.is_jumping = false;
        state.is_flipping = true;
        let dodging = car_to_player_info(&info, &state, &player(0, 23)).unwrap();
        assert_eq!(dodging.air_state, AirState::Dodging);
    }

    #[test]
    fn uses_rocketsim_ground_state_and_resets_dodge_elapsed_on_landing() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let state = CarState {
            is_on_ground: true,
            wheels_with_contact: [
                Some(DEFAULT_RAYCAST_HIT_INFO),
                Some(DEFAULT_RAYCAST_HIT_INFO),
                Some(DEFAULT_RAYCAST_HIT_INFO),
                None,
            ],
            has_flipped: true,
            flip_time: 0.4,
            ..CarState::default()
        };

        let converted = car_to_player_info(&info, &state, &player(0, 23)).unwrap();
        assert_eq!(converted.air_state, AirState::OnGround);
        assert_eq!(converted.dodge_elapsed, 0.0);
    }

    #[test]
    fn conversion_history_supplies_initial_jump_duration() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let state = CarState {
            is_on_ground: false,
            wheels_with_contact: [None; 4],
            has_jumped: true,
            jump_ticks: (0.65 * consts::TICK_RATE) as u32,
            air_time_since_jump: 0.45,
            ..CarState::default()
        };

        let conservative = car_to_player_info(&info, &state, &player(0, 23)).unwrap();
        assert!((conservative.dodge_timeout - 0.8).abs() < 1e-5);

        let converted = car_to_player_info_with_history(
            &info,
            &state,
            &player(0, 23),
            CarConversionHistory {
                initial_jump_duration: 0.2,
                double_jump_active: false,
                flip_reset_available: false,
            },
        )
        .unwrap();
        assert!((converted.dodge_timeout - 1.0).abs() < 1e-5);
    }

    #[test]
    fn conversion_history_includes_hold_extension_during_initial_jump() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let state = CarState {
            is_on_ground: true,
            is_jumping: true,
            has_jumped: true,
            jump_ticks: (0.1 * consts::TICK_RATE) as u32,
            ..CarState::default()
        };

        let converted = car_to_player_info_with_history(
            &info,
            &state,
            &player(0, 23),
            CarConversionHistory {
                initial_jump_duration: 0.1,
                double_jump_active: false,
                flip_reset_available: false,
            },
        )
        .unwrap();
        assert_eq!(converted.dodge_timeout, -1.0);
    }

    #[test]
    fn conversion_history_retains_jump_extension_after_base_window() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let state = CarState {
            is_on_ground: false,
            has_jumped: true,
            air_time_since_jump: 1.25,
            ..CarState::default()
        };

        let converted = car_to_player_info_with_history(
            &info,
            &state,
            &player(0, 23),
            CarConversionHistory {
                initial_jump_duration: 0.2,
                double_jump_active: false,
                flip_reset_available: false,
            },
        )
        .unwrap();
        assert!((converted.dodge_timeout - 0.2).abs() < 1e-5);
    }

    #[test]
    fn conversion_history_supplies_double_jump_transient() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let state = CarState {
            is_on_ground: false,
            has_jumped: true,
            has_double_jumped: true,
            ..CarState::default()
        };

        let converted = car_to_player_info_with_history(
            &info,
            &state,
            &player(0, 23),
            CarConversionHistory {
                initial_jump_duration: 0.0,
                double_jump_active: true,
                flip_reset_available: false,
            },
        )
        .unwrap();
        assert_eq!(converted.air_state, AirState::DoubleJumping);
    }

    #[test]
    fn active_dodge_takes_precedence_over_double_jump_history() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let state = CarState {
            is_on_ground: false,
            has_double_jumped: true,
            has_flipped: true,
            is_flipping: true,
            ..CarState::default()
        };

        let converted = car_to_player_info_with_history(
            &info,
            &state,
            &player(0, 23),
            CarConversionHistory {
                initial_jump_duration: 0.0,
                double_jump_active: true,
                flip_reset_available: false,
            },
        )
        .unwrap();
        assert_eq!(converted.air_state, AirState::Dodging);
    }

    #[test]
    fn landing_resets_dodge_elapsed_even_when_jump_state_takes_precedence() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };
        let state = CarState {
            is_on_ground: true,
            is_jumping: true,
            has_flipped: true,
            flip_time: 0.4,
            ..CarState::default()
        };

        let converted = car_to_player_info(&info, &state, &player(0, 23)).unwrap();
        assert_eq!(converted.air_state, AirState::Jumping);
        assert_eq!(converted.dodge_elapsed, 0.0);
    }

    #[test]
    fn validates_team_and_body_configuration() {
        let info = CarInfo {
            idx: 0,
            team: Team::Blue,
            config: CarBodyConfig::OCTANE,
        };

        assert!(car_to_player_info(&info, &CarState::default(), &player(0, 23)).is_ok());
        assert_eq!(
            car_to_player_info(&info, &CarState::default(), &player(1, 23)),
            Err(ToRlbotError::TeamMismatch {
                car_index: 0,
                rocketsim_team: 0,
                configured_team: 1,
            })
        );
        assert_eq!(
            car_to_player_info(&info, &CarState::default(), &player(0, 29)),
            Err(ToRlbotError::BodyConfigMismatch {
                car_index: 0,
                product_id: 29,
            })
        );
    }
}
