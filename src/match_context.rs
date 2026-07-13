use rlbot::flat::{
    FieldInfo, GameMode as RlbotGameMode, MatchConfiguration, PlayerClass, PlayerConfiguration,
    PlayerLoadout,
};
use rocketsim::{Arena, ArenaConfig, BoostPadConfig, CarBodyConfig, GameMode};
use thiserror::Error;

use crate::body::car_body_config_for_product_id;

#[derive(Clone, Debug)]
pub struct MatchContext {
    arena_config: ArenaConfig,
    players: Vec<ConfiguredPlayer>,
}

#[derive(Clone, Copy, Debug)]
struct ConfiguredPlayer {
    player_id: i32,
    team: u32,
    body_config: Option<CarBodyConfig>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MatchContextError {
    #[error("unsupported RLBot game mode {0:?}")]
    UnsupportedGameMode(RlbotGameMode),
    #[error("player {player_id} uses unknown car product ID {product_id}")]
    UnknownCarProductId { player_id: i32, product_id: u32 },
    #[error("packet player {player_index} with participant ID {player_id} has an unknown hitbox")]
    UnknownPacketHitbox { player_index: usize, player_id: i32 },
    #[error(
        "packet player {player_index} with participant ID {player_id} is absent from MatchConfiguration"
    )]
    PlayerNotConfigured { player_index: usize, player_id: i32 },
    #[error(
        "packet player {player_index} with participant ID {player_id} has a different team than MatchConfiguration"
    )]
    ConfiguredTeamMismatch { player_index: usize, player_id: i32 },
    #[error(
        "packet player {player_index} with participant ID {player_id} has a hitbox that disagrees with its configured car product ID"
    )]
    HitboxMismatch { player_index: usize, player_id: i32 },
    #[error("packet has {packet} boost pads but the RocketSim arena has {arena}")]
    BoostPadCountMismatch { packet: usize, arena: usize },
}

impl MatchContext {
    pub fn new(
        match_config: &MatchConfiguration,
        field_info: &FieldInfo,
    ) -> Result<Self, MatchContextError> {
        let game_mode = game_mode_from_rlbot(match_config.game_mode)?;
        let mut arena_config = ArenaConfig::new(game_mode);
        if !field_info.boost_pads.is_empty() && game_mode != GameMode::Dropshot {
            arena_config.custom_boost_pads = Some(
                field_info
                    .boost_pads
                    .iter()
                    .map(|pad| BoostPadConfig {
                        pos: glam::Vec3A::new(pad.location.x, pad.location.y, pad.location.z),
                        is_big: pad.is_full_boost,
                    })
                    .collect(),
            );
        }

        let players = match_config
            .player_configurations
            .iter()
            .map(configured_player)
            .collect::<Result<_, _>>()?;

        Ok(Self {
            arena_config,
            players,
        })
    }

    #[must_use]
    pub fn create_arena(&self) -> Arena {
        Arena::new_with_config(self.arena_config.clone())
    }

    #[must_use]
    pub(crate) fn arena_config(&self) -> &ArenaConfig {
        &self.arena_config
    }

    pub(crate) fn body_config_for_packet_player(
        &self,
        player_index: usize,
        player_id: i32,
        team: u32,
        hitbox: &rlbot::flat::BoxShape,
        hitbox_offset: rlbot::flat::Vector3,
    ) -> Result<CarBodyConfig, MatchContextError> {
        let configured = self
            .players
            .iter()
            .find(|player| player.player_id == player_id)
            .ok_or(MatchContextError::PlayerNotConfigured {
                player_index,
                player_id,
            })?;
        if configured.team != team {
            return Err(MatchContextError::ConfiguredTeamMismatch {
                player_index,
                player_id,
            });
        }
        if let Some(body_config) = configured.body_config {
            if !hitbox_matches(body_config, hitbox, hitbox_offset) {
                return Err(MatchContextError::HitboxMismatch {
                    player_index,
                    player_id,
                });
            }
            return Ok(body_config);
        }

        body_config_for_hitbox(hitbox, hitbox_offset).ok_or(
            MatchContextError::UnknownPacketHitbox {
                player_index,
                player_id,
            },
        )
    }
}

fn configured_player(player: &PlayerConfiguration) -> Result<ConfiguredPlayer, MatchContextError> {
    let body_config = player_loadout(&player.variety)
        .map(|loadout| {
            car_body_config_for_product_id(loadout.car_id).ok_or(
                MatchContextError::UnknownCarProductId {
                    player_id: player.player_id,
                    product_id: loadout.car_id,
                },
            )
        })
        .transpose()?;
    Ok(ConfiguredPlayer {
        player_id: player.player_id,
        team: player.team,
        body_config,
    })
}

fn player_loadout(player_class: &PlayerClass) -> Option<&PlayerLoadout> {
    match player_class {
        PlayerClass::CustomBot(bot) => bot.loadout.as_deref(),
        PlayerClass::PsyonixBot(bot) => bot.loadout.as_deref(),
        PlayerClass::Human(_) => None,
    }
}

fn game_mode_from_rlbot(mode: RlbotGameMode) -> Result<GameMode, MatchContextError> {
    match mode {
        RlbotGameMode::Soccar | RlbotGameMode::Rumble => Ok(GameMode::Soccar),
        RlbotGameMode::Hoops => Ok(GameMode::Hoops),
        RlbotGameMode::Dropshot => Ok(GameMode::Dropshot),
        RlbotGameMode::Snowday => Ok(GameMode::Snowday),
        RlbotGameMode::Heatseeker => Ok(GameMode::Heatseeker),
        RlbotGameMode::Gridiron | RlbotGameMode::Knockout => {
            Err(MatchContextError::UnsupportedGameMode(mode))
        }
    }
}

fn body_config_for_hitbox(
    hitbox: &rlbot::flat::BoxShape,
    offset: rlbot::flat::Vector3,
) -> Option<CarBodyConfig> {
    [
        CarBodyConfig::OCTANE,
        CarBodyConfig::DOMINUS,
        CarBodyConfig::BREAKOUT,
        CarBodyConfig::MERC,
        CarBodyConfig::PLANK,
        CarBodyConfig::HYBRID,
        CarBodyConfig::PSYCLOPS,
    ]
    .into_iter()
    .find(|config| hitbox_matches(*config, hitbox, offset))
}

fn hitbox_matches(
    config: CarBodyConfig,
    hitbox: &rlbot::flat::BoxShape,
    offset: rlbot::flat::Vector3,
) -> bool {
    const TOLERANCE: f32 = 0.25;
    let close = |left: f32, right: f32| (left - right).abs() <= TOLERANCE;
    close(config.hitbox_size.x, hitbox.length)
        && close(config.hitbox_size.y, hitbox.width)
        && close(config.hitbox_size.z, hitbox.height)
        && close(config.hitbox_pos_offset.x, offset.x)
        && close(config.hitbox_pos_offset.y, offset.y)
        && close(config.hitbox_pos_offset.z, offset.z)
}
