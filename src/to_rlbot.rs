use rlbot::flat::{
    AirState, BoxShape, MatchConfiguration, PlayerClass, PlayerConfiguration, PlayerInfo,
};
use rocketsim::{Arena, CarInfo, CarState};
use thiserror::Error;

use crate::body::car_body_config_for_product_id;
use crate::common::{controls_to_rlbot, physics_to_rlbot, vector2_to_rlbot, vector3_to_rlbot};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ToRlbotError {
    #[error("RocketSim car {car_index} has no corresponding MatchConfiguration player")]
    MissingPlayerConfiguration { car_index: usize },
    #[error(
        "RocketSim car {car_index} is on team {rocketsim_team}, but MatchConfiguration uses team {configured_team}"
    )]
    TeamMismatch {
        car_index: usize,
        rocketsim_team: u32,
        configured_team: u32,
    },
    #[error(
        "RocketSim car {car_index} body config disagrees with configured product ID {product_id}"
    )]
    BodyConfigMismatch { car_index: usize, product_id: u32 },
    #[error("player {player_id} uses unknown car product ID {product_id}")]
    UnknownCarProductId { player_id: i32, product_id: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CarConversionHistory {
    /// Duration of the initial jump force, clamped to Rocket League's 0.2-second maximum.
    pub initial_jump_duration: f32,
    /// Seconds remaining in RLBot's active 13-tick double-jump state.
    pub double_jump_active_time: f32,
}

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

pub trait ArenaExt {
    /// Converts a car using `MatchConfiguration.player_configurations[CarInfo::idx]`.
    fn car_to_rlbot_player_info(
        &self,
        car_index: usize,
        match_config: &MatchConfiguration,
    ) -> Result<PlayerInfo, ToRlbotError>;

    /// Converts all cars in RocketSim index order. The resulting vector is
    /// suitable for `GamePacket.players` with `players[N]` corresponding to car N.
    fn to_rlbot_players(
        &self,
        match_config: &MatchConfiguration,
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
}

pub fn car_to_player_info(
    info: &CarInfo,
    state: &CarState,
    player_config: &PlayerConfiguration,
) -> Result<PlayerInfo, ToRlbotError> {
    car_to_player_info_with_history(info, state, player_config, CarConversionHistory::default())
}

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
    } else if history.double_jump_active_time > 0.0 && state.has_double_jumped {
        AirState::DoubleJumping
    } else if state.is_on_ground {
        AirState::OnGround
    } else {
        AirState::InAir
    };

    let dodge_timeout = if air_state == AirState::OnGround
        || !state.has_jumped
        || state.has_double_jumped
        || state.has_flipped
        || state.air_time_since_jump >= rocketsim::consts::car::jump::DOUBLEJUMP_MAX_DELAY
    {
        -1.0
    } else {
        rocketsim::consts::car::jump::DOUBLEJUMP_MAX_DELAY
            + history
                .initial_jump_duration
                .clamp(0.0, rocketsim::consts::car::jump::MAX_TIME)
            - state.air_time_since_jump
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
    use rlbot::flat::{CustomBot, PlayerLoadout};
    use rocketsim::{CarBodyConfig, Team};

    use super::*;

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
            wheels_with_contact: [true; 4],
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
            wheels_with_contact: [true, true, true, false],
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
            wheels_with_contact: [false; 4],
            has_jumped: true,
            jump_time: 0.65,
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
                double_jump_active_time: 0.0,
            },
        )
        .unwrap();
        assert!((converted.dodge_timeout - 1.0).abs() < 1e-5);
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
                double_jump_active_time: 0.1,
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
                double_jump_active_time: 0.01,
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
