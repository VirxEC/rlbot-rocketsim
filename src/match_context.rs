use rlbot::flat::{
    FieldInfo, GameMode as RlbotGameMode, MatchConfiguration, PlayerClass, PlayerConfiguration,
    PlayerLoadout,
};
use rocketsim::{Arena, ArenaConfig, BoostPadConfig, CarBodyConfig, GameMode};
use thiserror::Error;

use crate::body::car_body_config_for_product_id;

/// Static match setup shared by every packet in a Soccar game.
///
/// Built once from [`MatchConfiguration`] (game mode + players) and
/// [`FieldInfo`] (boost-pad layout). The enricher clones it into an
/// [`Arena`] and then checks each packet against it: same
/// participants, same teams, matching hitboxes, and matching pad counts.
///
/// Currently only Soccar is supported; anything else returns
/// [`MatchContextError::UnsupportedGameMode`].
///
/// # Example
///
/// ```rust
/// use rlbot_rocketsim::rlbot::flat::{FieldInfo, GameMode, MatchConfiguration};
/// use rlbot_rocketsim::rocketsim::init_from_default;
/// use rlbot_rocketsim::{GameStateEnricher, MatchContext};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// init_from_default(true)?;
/// let match_config = MatchConfiguration {
///     game_mode: GameMode::Soccar,
///     ..Default::default()
/// };
/// let context = MatchContext::new(&match_config, &FieldInfo::default()).unwrap();
/// let enricher = GameStateEnricher::from_match_context(context);
/// # Ok(())
/// # }
/// ```
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

/// Reasons static match data (or a packet checked against it) is unusable.
///
/// surfacing which `player_id` / `player_index` failed keeps bot logs
/// actionable when a lobby changes cars mid-session.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MatchContextError {
    /// Only `Soccar` is supported. Fails fast in [`MatchContext::new`] so a
    /// Dropshot/Rumble lobby never silently enriches with the wrong physics.
    #[error("unsupported RLBot game mode {0:?}")]
    UnsupportedGameMode(RlbotGameMode),
    /// The loadout names a car ID with no known hitbox family. See
    /// [`body`](crate::body) for the mapped IDs.
    #[error("player {player_id} uses unknown car product ID {product_id}")]
    UnknownCarProductId { player_id: i32, product_id: u32 },
    /// Two match-config entries share a `player_id`. IDs must be unique to
    /// track participants across packets.
    #[error("MatchConfiguration contains duplicate participant ID {player_id}")]
    DuplicatePlayerId { player_id: i32 },
    /// A human (no loadout) sent a hitbox that matches no known
    /// [`CarBodyConfig`] within tolerance.
    #[error("packet player {player_index} with participant ID {player_id} has an unknown hitbox")]
    UnknownPacketHitbox { player_index: usize, player_id: i32 },
    /// The packet contains a `player_id` absent from the match configuration.
    /// Rebuild the [`MatchContext`] when the lobby changes.
    #[error(
        "packet player {player_index} with participant ID {player_id} is absent from MatchConfiguration"
    )]
    PlayerNotConfigured { player_index: usize, player_id: i32 },
    /// The packet team differs from the configured team for this `player_id`.
    #[error(
        "packet player {player_index} with participant ID {player_id} has a different team than MatchConfiguration"
    )]
    ConfiguredTeamMismatch { player_index: usize, player_id: i32 },
    /// The packet hitbox disagrees with the configured product ID (tolerance
    /// 0.25 uu per dimension). Usually a mid-session car change.
    #[error(
        "packet player {player_index} with participant ID {player_id} has a hitbox that disagrees with its configured car product ID"
    )]
    HitboxMismatch { player_index: usize, player_id: i32 },
    /// `GamePacket.boost_pads.len()` must equal the arena pad count, or pads
    /// would silently shift indices.
    #[error("packet has {packet} boost pads but the RocketSim arena has {arena}")]
    BoostPadCountMismatch { packet: usize, arena: usize },
}

impl MatchContext {
    /// Snapshots the arena config and per-player bodies for a match.
    ///
    /// Empty `FieldInfo.boost_pads` keeps the default Soccar pads; otherwise
    /// each pad's position and `is_full_boost` flag becomes a RocketSim
    /// [`BoostPadConfig`]. Duplicate
    /// `player_id`s and unknown car product IDs fail here instead of at the
    /// first packet.
    ///
    /// Humans (no loadout) resolve their hitbox from the first packet they
    /// appear in; bots resolve it now from their product ID.
    pub fn new(
        match_config: &MatchConfiguration,
        field_info: &FieldInfo,
    ) -> Result<Self, MatchContextError> {
        let game_mode = game_mode_from_rlbot(match_config.game_mode)?;
        let mut arena_config = ArenaConfig::new(game_mode);
        if !field_info.boost_pads.is_empty() {
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

        let mut players = Vec::with_capacity(match_config.player_configurations.len());
        for player in &match_config.player_configurations {
            if players
                .iter()
                .any(|configured: &ConfiguredPlayer| configured.player_id == player.player_id)
            {
                return Err(MatchContextError::DuplicatePlayerId {
                    player_id: player.player_id,
                });
            }
            players.push(configured_player(player)?);
        }

        Ok(Self {
            arena_config,
            players,
        })
    }

    /// Builds a fresh RocketSim arena with this match's mode and pad layout.
    ///
    /// The [`GameStateEnricher`](crate::GameStateEnricher) calls this on
    /// construction and on every rebuild (departures, team/body changes,
    /// gravity changes, frame rollbacks).
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
        RlbotGameMode::Soccar => Ok(GameMode::Soccar),
        _ => Err(MatchContextError::UnsupportedGameMode(mode)),
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
