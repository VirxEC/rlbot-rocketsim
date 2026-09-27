//! Feed RLBot packets into RocketSim and read back enriched car state.
//!
//! [`GameStateEnricher`] owns a RocketSim [`Arena`] that
//! mirrors the match. Each [`GamePacket`] is authoritative for physics,
//! inputs, boost, and jump/dodge flags; RocketSim only supplies what RLBot
//! cannot, such as wheel contacts, contact normals, and
//! [`is_on_ground`](rocketsim::CarState::is_on_ground).
//!
//! # Minimal loop
//!
//! ```rust
//! use rlbot_rocketsim::rlbot::flat::{GamePacket, MatchPhase, PlayerInfo};
//! use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
//! use rlbot_rocketsim::GameStateEnricher;
//!
//! let mut enricher =
//!     GameStateEnricher::new(Arena::new(GameMode::TheVoid), CarBodyConfig::OCTANE);
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
//! for mapping in enricher.update(&packet).unwrap() {
//!     // `mapping.player_index` is the `GamePacket.players` slot,
//!     // `mapping.car_index` is the RocketSim car.
//!     let car = enricher.car_state(mapping.player_index).unwrap();
//!     assert_eq!(car.phys.pos.x, 0.0);
//! }
//! ```
//!
//! For real Soccar matches, build the enricher with
//! [`GameStateEnricher::from_match_context`] so bodies, boost pads, and the
//! arena come from [`MatchContext`]. For bots that must
//! survive reordered packets, look cars up with
//! [`GameStateEnricher::car_state_by_player_id`] instead of the packet slot.

use glam::Vec3A;
use rlbot::flat::{AirState, CollisionShape, GamePacket, MatchPhase, PlayerInfo};
use rocketsim::{
    Arena, ArenaConfig, ArenaState, BallState, BoostPadState, CarBodyConfig, CarControls, CarState,
    GameMode, Team, consts,
};
use thiserror::Error;

use crate::common::{MAX_JUMP_HOLD_TIME, controls_from_rlbot, physics_from_rlbot};
use crate::match_context::{MatchContext, MatchContextError};
use crate::to_rlbot::CarConversionHistory;

/// Maps one `GamePacket.players` slot to its RocketSim car.
///
/// Returned by [`GameStateEnricher::update`] in packet order. `player_index`
/// is the index into the packet you just fed in; `car_index` is the matching
/// RocketSim car for [`Arena::get_car_state`](rocketsim::Arena::get_car_state).
///
/// # Example
///
/// ```rust
/// # use rlbot_rocketsim::rlbot::flat::{GamePacket, MatchPhase, PlayerInfo};
/// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
/// # use rlbot_rocketsim::GameStateEnricher;
/// # let mut enricher =
/// #     GameStateEnricher::new(Arena::new(GameMode::TheVoid), CarBodyConfig::OCTANE);
/// # let mut packet = GamePacket::default();
/// # packet.match_info.frame_num = 1;
/// # packet.match_info.match_phase = MatchPhase::Active;
/// # packet.players.push(PlayerInfo { player_id: 7, demolished_timeout: -1.0, ..PlayerInfo::default() });
/// let mappings = enricher.update(&packet).unwrap();
/// assert_eq!(mappings[0].player_index, 0);
/// let car = enricher.arena().get_car_state(mappings[0].car_index);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnrichedPlayer {
    /// Index into the `GamePacket.players` slice that produced this mapping.
    pub player_index: usize,
    /// Index of the corresponding RocketSim car in [`Arena`].
    pub car_index: usize,
}

/// Reasons [`GameStateEnricher::update`] can reject a packet.
///
/// The enricher is unchanged when `update` fails: no cars are added, no
/// history is reset, and the tick count does not advance.
#[derive(Debug, Error, PartialEq)]
pub enum EnrichmentError {
    /// Static match data disagrees with the packet (unknown body, team or
    /// hitbox change, boost-pad count mismatch, ...).
    #[error(transparent)]
    MatchContext(#[from] MatchContextError),
    /// `PlayerInfo.team` was not 0 (blue) or 1 (orange).
    #[error("player at packet index {player_index} has unsupported team index {team}")]
    InvalidTeam { player_index: usize, team: u32 },
    /// Two packet players share one `player_id`. IDs must be unique because
    /// they are how the enricher tracks participants across reordered packets.
    #[error("RLBot packet contains duplicate participant ID {player_id}")]
    DuplicatePlayerId { player_id: i32 },
    /// Context-backed enrichment needs exactly one ball; context-free
    /// enrichment tolerates an empty `balls` list and leaves the ball alone.
    #[error("expected exactly one RLBot ball, got {count}")]
    BallCount { count: usize },
    /// Example: a sphere ball in Snowday mode, or a puck outside Snowday.
    #[error("RLBot ball shape {shape} is incompatible with RocketSim mode {mode:?}")]
    BallShape { shape: &'static str, mode: GameMode },
    /// The sphere diameter differs from the arena's ball by more than 1 uu.
    #[error("RLBot ball diameter {actual} does not match RocketSim diameter {expected}")]
    BallSize { actual: f32, expected: f32 },
}

struct TrackedPlayer {
    car_index: usize,
    player_id: i32,
    team: Team,
    body_config: CarBodyConfig,
    previous_controls: CarControls,
    initial_jump_duration: f32,
    double_jump_active: bool,
    flip_reset_available: bool,
}

/// Mirrors RLBot packets in RocketSim and adds contact/history enrichment.
///
/// Call [`update`](Self::update) with every new [`GamePacket`]. Packet values
/// win for physics, boost, and jump/dodge flags; the RocketSim probe only
/// fills in what the packet cannot describe (wheel contacts, contact normals,
/// ground state, preserved controls history).
///
/// New players join without disturbing existing cars. Leaving players, team or
/// body changes, gravity changes, and frame rollbacks rebuild the arena
/// because RocketSim has no car-removal API.
///
/// # Example
///
/// ```rust
/// use rlbot_rocketsim::rlbot::flat::{GamePacket, MatchPhase, PlayerInfo};
/// use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
/// use rlbot_rocketsim::GameStateEnricher;
///
/// let mut enricher =
///     GameStateEnricher::new(Arena::new(GameMode::TheVoid), CarBodyConfig::OCTANE);
/// let mut packet = GamePacket::default();
/// packet.match_info.frame_num = 1;
/// packet.match_info.match_phase = MatchPhase::Active;
/// packet.players.push(PlayerInfo {
///     player_id: 7,
///     team: 0,
///     demolished_timeout: -1.0,
///     dodge_timeout: -1.0,
///     ..PlayerInfo::default()
/// });
///
/// enricher.update(&packet).unwrap();
/// // Stable across packet reorderings:
/// assert!(enricher.car_state_by_player_id(7).is_some());
/// ```
pub struct GameStateEnricher {
    arena: Arena,
    arena_config: ArenaConfig,
    default_body_config: CarBodyConfig,
    match_context: Option<MatchContext>,
    players: Vec<TrackedPlayer>,
    last_frame: Option<u32>,
    last_phase: Option<MatchPhase>,
}

impl GameStateEnricher {
    /// Creates an enricher around your own arena; every packet car uses
    /// `body_config`.
    ///
    /// Prefer [`from_match_context`](Self::from_match_context) for real
    /// matches so each car gets its configured hitbox. Use this constructor
    /// for tests and for arenas (like `TheVoid`) that have no match data.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
    /// # use rlbot_rocketsim::GameStateEnricher;
    /// let mut enricher =
    ///     GameStateEnricher::new(Arena::new(GameMode::TheVoid), CarBodyConfig::OCTANE);
    /// assert_eq!(enricher.arena().num_cars(), 0);
    /// ```
    #[must_use]
    pub fn new(arena: Arena, body_config: CarBodyConfig) -> Self {
        let arena_config = arena.get_config().clone();
        Self {
            arena,
            arena_config,
            default_body_config: body_config,
            match_context: None,
            players: Vec::new(),
            last_frame: None,
            last_phase: None,
        }
    }

    /// Creates an enricher from static match data (game mode, boost pads,
    /// configured bodies).
    ///
    /// This is the recommended path for Soccar bots: the arena, pad layout,
    /// and per-player hitboxes all come from [`MatchContext`], and later
    /// packets are validated against that configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rlbot_rocketsim::rlbot::flat::{FieldInfo, GameMode, MatchConfiguration};
    /// # use rlbot_rocketsim::{GameStateEnricher, MatchContext};
    /// # use rlbot_rocketsim::rocketsim::init_from_default;
    /// # init_from_default(true).unwrap();
    /// # let match_config = MatchConfiguration { game_mode: GameMode::Soccar, ..Default::default() };
    /// # let field_info = FieldInfo::default();
    /// let context = MatchContext::new(&match_config, &field_info).unwrap();
    /// let enricher = GameStateEnricher::from_match_context(context);
    /// assert_eq!(enricher.arena().num_cars(), 0);
    /// ```
    #[must_use]
    pub fn from_match_context(context: MatchContext) -> Self {
        let arena_config = context.arena_config().clone();
        let arena = context.create_arena();
        Self {
            arena,
            arena_config,
            default_body_config: CarBodyConfig::default(),
            match_context: Some(context),
            players: Vec::new(),
            last_frame: None,
            last_phase: None,
        }
    }

    /// Borrows the underlying RocketSim arena (cars, ball, pads, tick count).
    ///
    /// Use this for contact queries, raycasts, or stepping your own copies.
    /// Do not mutate through it when you want the next [`update`](Self::update)
    /// to stay packet-authoritative; use [`arena_mut`](Self::arena_mut) only
    /// for deliberate local experiments.
    #[must_use]
    pub const fn arena(&self) -> &Arena {
        &self.arena
    }

    /// Mutably borrows the underlying arena, e.g. to inspect or tweak a probe.
    ///
    /// The next [`update`](Self::update) overwrites car/ball state with the
    /// packet again, but preserves RocketSim-derived contacts.
    pub const fn arena_mut(&mut self) -> &mut Arena {
        &mut self.arena
    }

    /// Snapshots every car and the ball after the latest enrichment.
    #[must_use]
    pub fn arena_state(&self) -> ArenaState {
        self.arena.get_arena_state()
    }

    /// Borrows the enriched ball. Without a [`MatchContext`], an empty
    /// `GamePacket.balls` list leaves a previous ball untouched.
    #[must_use]
    pub const fn ball_state(&self) -> &BallState {
        self.arena.get_ball_state()
    }

    /// Returns the enriched car for a `GamePacket.players` slot.
    ///
    /// Returns `None` for an out-of-range slot. Slots reorder when players
    /// join or the server reorders them, so for anything stored across
    /// packets use [`car_state_by_player_id`](Self::car_state_by_player_id).
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rlbot_rocketsim::rlbot::flat::{GamePacket, MatchPhase, PlayerInfo};
    /// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
    /// # use rlbot_rocketsim::GameStateEnricher;
    /// # let mut enricher = GameStateEnricher::new(Arena::new(GameMode::TheVoid), CarBodyConfig::OCTANE);
    /// # let mut packet = GamePacket::default();
    /// # packet.match_info.frame_num = 1;
    /// # packet.match_info.match_phase = MatchPhase::Active;
    /// # packet.players.push(PlayerInfo { player_id: 7, demolished_timeout: -1.0, ..PlayerInfo::default() });
    /// # enricher.update(&packet).unwrap();
    /// assert!(enricher.car_state(0).is_some());
    /// assert!(enricher.car_state(99).is_none());
    /// ```
    #[must_use]
    pub fn car_state(&self, packet_player_index: usize) -> Option<&CarState> {
        self.players
            .get(packet_player_index)
            .map(|player| self.arena.get_car_state(player.car_index))
    }

    /// Returns the enriched car for a stable RLBot `player_id`.
    ///
    /// Unlike [`car_state`](Self::car_state), this lookup survives packet
    /// reorderings because the enricher keys participants by ID internally.
    /// Returns `None` when that participant was absent from the latest packet.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rlbot_rocketsim::rlbot::flat::{GamePacket, MatchPhase, PlayerInfo};
    /// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
    /// # use rlbot_rocketsim::GameStateEnricher;
    /// # let mut enricher = GameStateEnricher::new(Arena::new(GameMode::TheVoid), CarBodyConfig::OCTANE);
    /// # let mut packet = GamePacket::default();
    /// # packet.match_info.frame_num = 1;
    /// # packet.match_info.match_phase = MatchPhase::Active;
    /// # packet.players.push(PlayerInfo { player_id: 42, demolished_timeout: -1.0, ..PlayerInfo::default() });
    /// # enricher.update(&packet).unwrap();
    /// assert!(enricher.car_state_by_player_id(42).is_some());
    /// assert!(enricher.car_state_by_player_id(7).is_none());
    /// ```
    #[must_use]
    pub fn car_state_by_player_id(&self, player_id: i32) -> Option<&CarState> {
        self.players
            .iter()
            .find(|player| player.player_id == player_id)
            .map(|player| self.arena.get_car_state(player.car_index))
    }

    /// Returns the retained [`CarConversionHistory`] for a packet slot.
    ///
    /// Feed this into
    /// [`car_to_player_info_with_history`](crate::to_rlbot::car_to_player_info_with_history)
    /// to convert back to RLBot without losing the initial-jump hold time or
    /// a transient `DoubleJumping` state. Slots reorder; prefer
    /// [`car_conversion_history_by_player_id`](Self::car_conversion_history_by_player_id)
    /// for stored lookups.
    #[must_use]
    pub fn car_conversion_history(
        &self,
        packet_player_index: usize,
    ) -> Option<CarConversionHistory> {
        self.players
            .get(packet_player_index)
            .map(car_conversion_history)
    }

    /// Returns the retained [`CarConversionHistory`] for a stable `player_id`.
    ///
    /// This is the history-aware counterpart to
    /// [`car_state_by_player_id`](Self::car_state_by_player_id): it keeps
    /// working when `GamePacket.players` is reordered between frames.
    #[must_use]
    pub fn car_conversion_history_by_player_id(
        &self,
        player_id: i32,
    ) -> Option<CarConversionHistory> {
        self.players
            .iter()
            .find(|player| player.player_id == player_id)
            .map(car_conversion_history)
    }

    /// Returns the retained initial-jump hold time (seconds, max 0.2) for a slot.
    ///
    /// This is a shortcut for `car_conversion_history(slot).initial_jump_duration`.
    /// It feeds the `dodge_timeout` reconstruction on the way back to RLBot.
    /// Slots reorder, so treat the result as valid only for the latest packet.
    #[must_use]
    pub fn initial_jump_duration(&self, packet_player_index: usize) -> Option<f32> {
        self.car_conversion_history(packet_player_index)
            .map(|history| history.initial_jump_duration)
    }

    /// Applies one packet and returns per-slot [`EnrichedPlayer`] mappings.
    ///
    /// What happens per call:
    /// 1. Validates teams, duplicate IDs, ball shape/size, and (with a
    ///    [`MatchContext`]) bodies and boost-pad counts.
    /// 2. On a new `Kickoff`/`Active` frame, probes RocketSim once with the
    ///    packet inputs to refresh contacts. Other phases, repeated frames,
    ///    and frame gaps never step more than once.
    /// 3. Restores packet physics/boost/jump state on top of the probe, keeps
    ///    RocketSim contacts, and records jump history for the trip back.
    ///
    /// New `player_id`s join in place; removals, team/body changes, gravity
    /// changes, and frame rollbacks rebuild the arena. On error the enricher
    /// is left untouched.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rlbot_rocketsim::rlbot::flat::{GamePacket, MatchPhase, PlayerInfo};
    /// # use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode};
    /// # use rlbot_rocketsim::GameStateEnricher;
    /// # let mut enricher = GameStateEnricher::new(Arena::new(GameMode::TheVoid), CarBodyConfig::OCTANE);
    /// # let mut packet = GamePacket::default();
    /// # packet.match_info.frame_num = 1;
    /// # packet.match_info.match_phase = MatchPhase::Active;
    /// # packet.players.push(PlayerInfo { player_id: 7, demolished_timeout: -1.0, ..PlayerInfo::default() });
    /// let mappings = enricher.update(&packet).unwrap();
    /// assert_eq!(mappings.len(), 1);
    /// ```
    pub fn update(&mut self, packet: &GamePacket) -> Result<Vec<EnrichedPlayer>, EnrichmentError> {
        let plan = self.build_update_plan(packet)?;
        Ok(self.apply_update_plan(plan))
    }

    fn build_update_plan<'a>(
        &self,
        packet: &'a GamePacket,
    ) -> Result<UpdatePlan<'a>, EnrichmentError> {
        let ball = match packet.balls.as_slice() {
            [ball] => {
                self.validate_ball_shape(ball)?;
                Some(ball)
            }
            [] if self.match_context.is_none() => None,
            balls => return Err(EnrichmentError::BallCount { count: balls.len() }),
        };
        if self.match_context.is_some() && packet.boost_pads.len() != self.arena.num_boost_pads() {
            return Err(MatchContextError::BoostPadCountMismatch {
                packet: packet.boost_pads.len(),
                arena: self.arena.num_boost_pads(),
            }
            .into());
        }

        let mut players = Vec::with_capacity(packet.players.len());
        for (packet_player_index, player) in packet.players.iter().enumerate() {
            if players
                .iter()
                .any(|planned: &PlannedPlayer<'_>| planned.player_id == player.player_id)
            {
                return Err(EnrichmentError::DuplicatePlayerId {
                    player_id: player.player_id,
                });
            }
            players.push(PlannedPlayer {
                player,
                player_id: player.player_id,
                team: team_from_rlbot(packet_player_index, player)?,
                body_config: self.body_config_for_player(packet_player_index, player)?,
                existing_player_index: self
                    .players
                    .iter()
                    .position(|tracked| tracked.player_id == player.player_id),
                controls: controls_from_rlbot(player.last_input),
            });
        }

        let frame = packet.match_info.frame_num;
        let frame_rollback = self.last_frame.is_some_and(|last| frame < last);
        let layout_changed = self.players.iter().any(|tracked| {
            !players
                .iter()
                .any(|planned| planned.player_id == tracked.player_id)
        }) || players.iter().any(|planned| {
            planned.existing_player_index.is_some_and(|index| {
                let tracked = &self.players[index];
                tracked.team != planned.team || tracked.body_config != planned.body_config
            })
        });
        let gravity_changed =
            self.arena_config.mutators.gravity.z != packet.match_info.world_gravity_z;
        let mut arena_config = self.arena_config.clone();
        if gravity_changed {
            arena_config.mutators.gravity.z = packet.match_info.world_gravity_z;
        }
        let rebuild = frame_rollback || layout_changed || gravity_changed;
        let resumed_after_inactive = phase_advances(packet.match_info.match_phase)
            && self.last_phase.is_some_and(|phase| !phase_advances(phase));
        let reset_history = self.last_frame.is_none()
            || frame_rollback
            || layout_changed
            || gravity_changed
            || resumed_after_inactive;
        let should_step = phase_advances(packet.match_info.match_phase)
            && self.last_frame.is_some_and(|last| frame > last);
        let boost_pads = if packet.boost_pads.is_empty() && self.match_context.is_none() {
            None
        } else {
            if packet.boost_pads.len() != self.arena.num_boost_pads() {
                return Err(MatchContextError::BoostPadCountMismatch {
                    packet: packet.boost_pads.len(),
                    arena: self.arena.num_boost_pads(),
                }
                .into());
            }
            Some(
                packet
                    .boost_pads
                    .iter()
                    .enumerate()
                    .map(|(index, pad)| {
                        let config = self.arena.get_boost_pad_config(index);
                        let max_cooldown = if config.is_big {
                            self.arena.mutator_config().boost_pad_cooldown_big
                        } else {
                            self.arena.mutator_config().boost_pad_cooldown_small
                        };
                        BoostPadState {
                            cooldown: if pad.is_active {
                                0.0
                            } else {
                                (max_cooldown - pad.timer).clamp(0.0, max_cooldown)
                            },
                        }
                    })
                    .collect(),
            )
        };

        Ok(UpdatePlan {
            players,
            arena_config,
            rebuild,
            reset_history,
            should_step,
            ball,
            boost_pads,
            frame,
            phase: packet.match_info.match_phase,
        })
    }

    fn apply_update_plan(&mut self, plan: UpdatePlan<'_>) -> Vec<EnrichedPlayer> {
        self.arena_config = plan.arena_config;
        if plan.rebuild {
            self.rebuild_arena();
        }
        if plan.reset_history {
            for tracked in &mut self.players {
                tracked.initial_jump_duration = 0.0;
                tracked.double_jump_active = false;
                tracked.flip_reset_available = false;
            }
        }
        let resolved = self.resolve_players_from_plan(&plan.players);

        if plan.should_step {
            for (planned, resolved) in plan.players.iter().zip(&resolved) {
                self.arena
                    .set_car_controls(resolved.car_index, planned.controls);
            }
            self.arena.step_tick();
        }

        self.apply_players_from_plan(
            &plan.players,
            &resolved,
            plan.reset_history,
            plan.should_step,
        );
        self.apply_ball_from_plan(plan.ball);
        self.apply_boost_pads_from_plan(plan.boost_pads.as_deref());
        self.last_frame = Some(plan.frame);
        self.last_phase = Some(plan.phase);

        resolved
            .iter()
            .enumerate()
            .map(|(player_index, resolved)| EnrichedPlayer {
                player_index,
                car_index: resolved.car_index,
            })
            .collect()
    }

    fn validate_ball_shape(&self, ball: &rlbot::flat::BallInfo) -> Result<(), EnrichmentError> {
        let mode = self.arena.game_mode();
        match (&ball.shape, mode) {
            (CollisionShape::SphereShape(_), GameMode::Snowday) => {
                Err(EnrichmentError::BallShape {
                    shape: "sphere",
                    mode,
                })
            }
            (CollisionShape::SphereShape(shape), _) => {
                let expected = self.arena.mutator_config().ball_radius * 2.0;
                if (shape.diameter - expected).abs() > 1.0 {
                    Err(EnrichmentError::BallSize {
                        actual: shape.diameter,
                        expected,
                    })
                } else {
                    Ok(())
                }
            }
            (CollisionShape::CylinderShape(_), GameMode::Snowday) => Ok(()),
            (CollisionShape::CylinderShape(_), _) => Err(EnrichmentError::BallShape {
                shape: "cylinder",
                mode,
            }),
            (CollisionShape::BoxShape(_), _) => {
                Err(EnrichmentError::BallShape { shape: "box", mode })
            }
        }
    }

    fn rebuild_arena(&mut self) {
        self.arena = Arena::new_with_config(self.arena_config.clone());
        self.players.clear();
        self.last_frame = None;
        self.last_phase = None;
    }

    fn resolve_players_from_plan(&mut self, players: &[PlannedPlayer<'_>]) -> Vec<ResolvedPlayer> {
        let mut ordered_players = Vec::with_capacity(players.len());
        for planned in players {
            let tracked = if let Some(position) = self
                .players
                .iter()
                .position(|tracked| tracked.player_id == planned.player_id)
            {
                self.players.remove(position)
            } else {
                TrackedPlayer {
                    car_index: self.arena.add_car(planned.team, planned.body_config),
                    player_id: planned.player_id,
                    team: planned.team,
                    body_config: planned.body_config,
                    previous_controls: CarControls::default(),
                    initial_jump_duration: 0.0,
                    double_jump_active: false,
                    flip_reset_available: false,
                }
            };
            ordered_players.push(tracked);
        }
        self.players = ordered_players;
        self.players
            .iter()
            .map(|tracked| ResolvedPlayer {
                car_index: tracked.car_index,
                previous_controls: tracked.previous_controls,
                initial_jump_duration: tracked.initial_jump_duration,
                flip_reset_available: tracked.flip_reset_available,
            })
            .collect()
    }

    fn apply_players_from_plan(
        &mut self,
        players: &[PlannedPlayer<'_>],
        resolved: &[ResolvedPlayer],
        reset_history: bool,
        stepped: bool,
    ) {
        for (index, (planned, resolved)) in players.iter().zip(resolved).enumerate() {
            let player = planned.player;
            let mut simulated = *self.arena.get_car_state(resolved.car_index);
            simulated.is_on_ground = simulated.num_wheels_in_contact() >= 3;
            let previous_controls = if stepped {
                simulated.prev_controls
            } else if reset_history {
                planned.controls
            } else {
                resolved.previous_controls
            };
            let state = merge_authoritative_player(
                player,
                simulated,
                previous_controls,
                resolved.initial_jump_duration,
                resolved.flip_reset_available,
                reset_history,
            );
            let mut state = state;
            preserve_simulated_contacts(simulated, &mut state);
            self.arena.set_car_state(resolved.car_index, state);
            self.arena
                .set_car_controls(resolved.car_index, state.controls);
            self.players[index].previous_controls = planned.controls;
            if player.air_state == AirState::Jumping {
                self.players[index].initial_jump_duration =
                    state.jump_time().clamp(0.0, MAX_JUMP_HOLD_TIME);
            } else if !player.has_jumped {
                self.players[index].initial_jump_duration = 0.0;
            }
            self.players[index].double_jump_active = player.air_state == AirState::DoubleJumping;
            self.players[index].flip_reset_available = player.air_state == AirState::InAir
                && !player.has_jumped
                && !player.has_double_jumped
                && !player.has_dodged;
        }
    }

    fn body_config_for_player(
        &self,
        player_index: usize,
        player: &PlayerInfo,
    ) -> Result<CarBodyConfig, EnrichmentError> {
        self.match_context.as_ref().map_or_else(
            || Ok(self.default_body_config),
            |context| {
                context
                    .body_config_for_packet_player(
                        player_index,
                        player.player_id,
                        player.team,
                        &player.hitbox,
                        player.hitbox_offset,
                    )
                    .map_err(Into::into)
            },
        )
    }

    fn apply_ball_from_plan(&mut self, ball: Option<&rlbot::flat::BallInfo>) {
        let Some(ball) = ball else {
            return;
        };
        let mut state = *self.arena.get_ball_state();
        apply_ball(ball, &mut state);
        self.arena.set_ball_state(state);
    }

    fn apply_boost_pads_from_plan(&mut self, boost_pads: Option<&[BoostPadState]>) {
        for (index, state) in boost_pads.unwrap_or_default().iter().enumerate() {
            self.arena.set_boost_pad_state(index, *state);
        }
    }
}

struct PlannedPlayer<'a> {
    player: &'a PlayerInfo,
    player_id: i32,
    team: Team,
    body_config: CarBodyConfig,
    existing_player_index: Option<usize>,
    controls: CarControls,
}

struct UpdatePlan<'a> {
    players: Vec<PlannedPlayer<'a>>,
    arena_config: ArenaConfig,
    rebuild: bool,
    reset_history: bool,
    should_step: bool,
    ball: Option<&'a rlbot::flat::BallInfo>,
    boost_pads: Option<Vec<BoostPadState>>,
    frame: u32,
    phase: MatchPhase,
}

#[derive(Clone, Copy)]
struct ResolvedPlayer {
    car_index: usize,
    previous_controls: CarControls,
    initial_jump_duration: f32,
    flip_reset_available: bool,
}

fn car_conversion_history(player: &TrackedPlayer) -> CarConversionHistory {
    CarConversionHistory {
        initial_jump_duration: player.initial_jump_duration,
        double_jump_active: player.double_jump_active,
        flip_reset_available: player.flip_reset_available,
    }
}

fn phase_advances(phase: MatchPhase) -> bool {
    matches!(phase, MatchPhase::Kickoff | MatchPhase::Active)
}

fn apply_ball(ball: &rlbot::flat::BallInfo, state: &mut BallState) {
    state.phys = physics_from_rlbot(ball.physics);
    state.hs_info.cur_target_speed = ball.target_speed;
    if ball.charge_level >= 0 {
        state.ds_info.charge_level = (ball.charge_level as u8).saturating_add(1).clamp(1, 3);
    }
}

fn team_from_rlbot(player_index: usize, player: &PlayerInfo) -> Result<Team, EnrichmentError> {
    match player.team {
        0 => Ok(Team::Blue),
        1 => Ok(Team::Orange),
        team => Err(EnrichmentError::InvalidTeam { player_index, team }),
    }
}

fn merge_authoritative_player(
    player: &PlayerInfo,
    mut state: CarState,
    previous_controls: CarControls,
    initial_jump_duration: f32,
    flip_reset_available: bool,
    reset_history: bool,
) -> CarState {
    let controls = controls_from_rlbot(player.last_input);

    restore_authoritative_player(
        player,
        &mut state,
        initial_jump_duration,
        flip_reset_available,
    );
    state.prev_controls = previous_controls;

    if reset_history {
        state.prev_controls = controls;
        // AirState describes jump/dodge forces, not wheel contact. Leave contact
        // fields under RocketSim's control so its collision state can establish them.
        state.air_time = 0.0;
        state.jump_ticks = 0;
        state.is_boosting = false;
        state.boosting_time = 0.0;
        state.time_since_boosted = 0.0;
        state.handbrake_val = 0.0;
    }

    state
}

fn preserve_simulated_contacts(simulated: CarState, state: &mut CarState) {
    if state.is_demoed {
        state.is_on_ground = false;
        state.wheels_with_contact = [None; 4];
        state.world_contact_normal = None;
    } else {
        state.is_on_ground = simulated.is_on_ground;
        state.wheels_with_contact = simulated.wheels_with_contact;
        state.world_contact_normal = simulated.world_contact_normal;
    }
}

fn restore_authoritative_player(
    player: &PlayerInfo,
    state: &mut CarState,
    initial_jump_duration: f32,
    flip_reset_available: bool,
) {
    let controls = controls_from_rlbot(player.last_input);
    state.phys = physics_from_rlbot(player.physics);
    state.controls = controls;
    state.has_jumped = player.has_jumped;
    state.has_double_jumped = player.has_double_jumped;
    state.has_flipped = player.has_dodged;
    state.flip_rel_torque = Vec3A::new(-player.dodge_dir.y, player.dodge_dir.x, 0.0);
    state.is_flipping = player.air_state == AirState::Dodging;
    state.flip_time = if state.is_flipping {
        player.dodge_elapsed.max(0.0)
    } else {
        0.0
    };
    state.is_jumping = player.air_state == AirState::Jumping;
    if player.air_state == AirState::DoubleJumping {
        state.has_double_jumped = true;
        state.is_jumping = false;
        state.is_flipping = false;
    }
    if state.is_jumping {
        // RLBot does not expose elapsed initial-jump hold ticks. Keep RocketSim's
        // estimate while the state is continuous, bounded by the documented maximum.
        state.jump_ticks = state.jump_ticks.min(consts::car::jump::MAX_TICKS);
        state.air_time_since_jump = 0.0;
    } else {
        state.jump_ticks = 0;
        if !player.has_jumped {
            state.air_time_since_jump = 0.0;
        } else if player.dodge_timeout >= 0.0 {
            state.air_time_since_jump = (consts::car::jump::DOUBLEJUMP_MAX_DELAY
                + initial_jump_duration.clamp(0.0, MAX_JUMP_HOLD_TIME)
                - player.dodge_timeout)
                .clamp(0.0, consts::car::jump::DOUBLEJUMP_MAX_DELAY);
        } else {
            state.air_time_since_jump = if flip_reset_available {
                0.0
            } else {
                consts::car::jump::DOUBLEJUMP_MAX_DELAY
            };
        }
    }
    state.boost = player.boost.clamp(0.0, 100.0);
    state.is_supersonic = player.is_supersonic;
    state.is_demoed = player.demolished_timeout != -1.0;
    state.demo_respawn_timer = if state.is_demoed {
        player.demolished_timeout.max(0.0)
    } else {
        0.0
    };
}

#[cfg(test)]
mod tests {
    use rlbot::flat::{
        BoostPadState as RlbotBoostPadState, GamePacket, MatchPhase, Physics, PlayerInfo, Vector3,
    };
    use rocketsim::{
        Arena, CarBodyConfig, GameMode, RaycastHitInfo, UserInfoType, init_from_default,
    };

    use super::*;

    fn packet(frame: u32, player: PlayerInfo) -> GamePacket {
        let mut packet = GamePacket::default();
        packet.match_info.frame_num = frame;
        packet.match_info.match_phase = MatchPhase::Active;
        packet.players.push(player);
        packet
    }

    fn player() -> PlayerInfo {
        PlayerInfo {
            player_id: 42,
            team: 0,
            physics: Physics {
                location: Vector3 {
                    x: 123.0,
                    y: -456.0,
                    z: 789.0,
                },
                velocity: Vector3 {
                    x: 10.0,
                    y: 20.0,
                    z: 30.0,
                },
                ..Physics::default()
            },
            boost: 37.0,
            demolished_timeout: -1.0,
            dodge_timeout: -1.0,
            ..PlayerInfo::default()
        }
    }

    #[test]
    fn preserves_player_mapping_and_authoritative_physics() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());

        let first = enricher.update(&packet(1, player())).unwrap();
        let second = enricher.update(&packet(2, player())).unwrap();

        assert_eq!(first[0].car_index, second[0].car_index);
        assert_eq!(enricher.arena().num_cars(), 1);
        assert_eq!(
            enricher.car_state(0).unwrap().phys.pos,
            Vec3A::new(123.0, -456.0, 789.0)
        );
    }

    #[test]
    fn player_id_preserves_identity_when_packet_order_changes() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut first_packet = packet(1, player());
        let mut second_player = player();
        second_player.player_id = 99;
        second_player.physics.location.x = 999.0;
        first_packet.players.push(second_player.clone());

        let first = enricher.update(&first_packet).unwrap();
        let first_car = first[0].car_index;
        let second_car = first[1].car_index;
        let mut first_state = *enricher.arena().get_car_state(first_car);
        first_state.handbrake_val = 0.25;
        enricher.arena_mut().set_car_state(first_car, first_state);
        let mut second_state = *enricher.arena().get_car_state(second_car);
        second_state.handbrake_val = 0.75;
        enricher.arena_mut().set_car_state(second_car, second_state);

        let mut reordered = GamePacket::default();
        reordered.match_info.frame_num = 2;
        reordered.match_info.match_phase = MatchPhase::Paused;
        reordered.players.push(second_player);
        reordered.players.push(player());
        let mappings = enricher.update(&reordered).unwrap();

        assert_eq!(mappings[0].car_index, second_car);
        assert_eq!(mappings[1].car_index, first_car);
        assert_eq!(enricher.car_state(0).unwrap().handbrake_val, 0.75);
        assert_eq!(enricher.car_state(1).unwrap().handbrake_val, 0.25);
        assert_eq!(
            enricher.car_state_by_player_id(42).unwrap().phys.pos.x,
            123.0
        );
        assert_eq!(enricher.arena().num_cars(), 2);
    }

    #[test]
    fn joining_player_preserves_existing_player_history() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        enricher.update(&packet(1, player())).unwrap();
        let mut state = *enricher.car_state(0).unwrap();
        state.handbrake_val = 0.5;
        enricher.arena_mut().set_car_state(0, state);

        let mut joined = packet(2, player());
        joined.match_info.match_phase = MatchPhase::Paused;
        let mut newcomer = player();
        newcomer.player_id = 99;
        joined.players.push(newcomer);
        enricher.update(&joined).unwrap();

        assert_eq!(enricher.arena().num_cars(), 2);
        assert_eq!(
            enricher.car_state_by_player_id(42).unwrap().handbrake_val,
            0.5
        );
    }

    #[test]
    fn rejects_duplicate_player_ids() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut duplicate = packet(1, player());
        duplicate.players.push(player());

        assert_eq!(
            enricher.update(&duplicate),
            Err(EnrichmentError::DuplicatePlayerId { player_id: 42 })
        );
        assert_eq!(enricher.arena().num_cars(), 0);
    }

    #[test]
    fn rebuilds_when_player_layout_changes() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        enricher.update(&packet(1, player())).unwrap();

        let mut replacement = player();
        replacement.player_id = 99;
        enricher.update(&packet(2, replacement)).unwrap();

        assert_eq!(enricher.arena().num_cars(), 1);
    }

    #[test]
    fn probe_retains_the_immediately_previous_controls() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut held = player();
        held.last_input.jump = true;

        enricher.update(&packet(1, held.clone())).unwrap();
        enricher.update(&packet(2, held.clone())).unwrap();
        assert!(enricher.car_state(0).unwrap().prev_controls.jump);

        let mut released = held;
        released.last_input.jump = false;
        enricher.update(&packet(3, released)).unwrap();
        assert!(!enricher.car_state(0).unwrap().prev_controls.jump);
    }

    #[test]
    fn packet_gravity_reconfigures_the_probe_arena() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut packet = packet(1, player());
        packet.match_info.world_gravity_z = 325.0;

        enricher.update(&packet).unwrap();

        assert_eq!(enricher.arena().mutator_config().gravity.z, 325.0);
        assert_eq!(enricher.arena_config.mutators.gravity.z, 325.0);
    }

    #[test]
    fn gravity_changes_rebuild_the_arena_and_reset_history() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut first = packet(1, player());
        first.match_info.world_gravity_z = -650.0;
        enricher.update(&first).unwrap();
        let mut state = *enricher.car_state(0).unwrap();
        state.handbrake_val = 0.5;
        enricher.arena_mut().set_car_state(0, state);

        let mut changed = packet(2, player());
        changed.match_info.world_gravity_z = 325.0;
        enricher.update(&changed).unwrap();

        assert_eq!(enricher.arena().mutator_config().gravity.z, 325.0);
        assert_eq!(enricher.arena().num_cars(), 1);
        assert_eq!(enricher.car_state(0).unwrap().handbrake_val, 0.0);
    }

    #[test]
    fn repeated_and_paused_frames_do_not_advance() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        enricher.update(&packet(1, player())).unwrap();
        let tick = enricher.arena().tick_count();
        enricher.update(&packet(1, player())).unwrap();
        assert_eq!(enricher.arena().tick_count(), tick);

        let mut paused = packet(2, player());
        paused.match_info.match_phase = MatchPhase::Paused;
        enricher.update(&paused).unwrap();
        assert_eq!(enricher.arena().tick_count(), tick);
    }

    #[test]
    fn frame_gaps_probe_only_the_current_packet_once() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        enricher.update(&packet(1, player())).unwrap();
        let tick = enricher.arena().tick_count();

        enricher.update(&packet(5, player())).unwrap();
        assert_eq!(enricher.arena().tick_count(), tick + 1);

        enricher.update(&packet(1_000, player())).unwrap();
        assert_eq!(enricher.arena().tick_count(), tick + 2);
    }

    #[test]
    fn resuming_after_pause_resets_history() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut active = player();
        active.last_input.handbrake = true;
        enricher.update(&packet(1, active)).unwrap();

        let mut paused = packet(2, player());
        paused.match_info.match_phase = MatchPhase::Paused;
        enricher.update(&paused).unwrap();

        let resumed = packet(3, player());
        enricher.update(&resumed).unwrap();
        assert_eq!(enricher.car_state(0).unwrap().handbrake_val, 0.0);
    }

    #[test]
    fn resume_discards_stale_initial_jump_duration() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut jumping = player();
        jumping.air_state = AirState::Jumping;
        jumping.has_jumped = true;
        jumping.last_input.jump = true;
        for frame in 1..=12 {
            enricher.update(&packet(frame, jumping.clone())).unwrap();
        }
        assert!(enricher.initial_jump_duration(0).unwrap() > 0.0);

        let mut paused = packet(13, jumping.clone());
        paused.match_info.match_phase = MatchPhase::Paused;
        enricher.update(&paused).unwrap();

        let mut resumed = jumping;
        resumed.air_state = AirState::InAir;
        resumed.last_input.jump = false;
        resumed.dodge_timeout = 1.0;
        enricher.update(&packet(14, resumed)).unwrap();

        assert_eq!(enricher.initial_jump_duration(0), Some(0.0));
        assert!((enricher.car_state(0).unwrap().air_time_since_jump - 0.25).abs() < 1e-5);
    }

    #[test]
    fn reconstructs_post_jump_time_from_retained_initial_jump_duration() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut jumping = player();
        jumping.air_state = AirState::Jumping;
        jumping.has_jumped = true;
        jumping.last_input.jump = true;

        for frame in 1..=12 {
            enricher.update(&packet(frame, jumping.clone())).unwrap();
        }

        let initial_jump_duration = enricher.players[0].initial_jump_duration;
        assert!(initial_jump_duration > 0.0);

        let mut airborne = jumping;
        airborne.air_state = AirState::InAir;
        airborne.last_input.jump = false;
        airborne.dodge_timeout =
            consts::car::jump::DOUBLEJUMP_MAX_DELAY + initial_jump_duration - 0.05;
        enricher.update(&packet(13, airborne)).unwrap();

        let state = enricher.car_state(0).unwrap();
        assert_eq!(state.jump_ticks, 0);
        assert!((state.air_time_since_jump - 0.05).abs() < 1e-5);
    }

    #[test]
    fn double_jumping_consumes_the_rocketsim_double_jump() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut double_jumping = player();
        double_jumping.air_state = AirState::DoubleJumping;
        double_jumping.has_jumped = true;
        double_jumping.has_double_jumped = true;

        enricher.update(&packet(1, double_jumping)).unwrap();
        let state = enricher.car_state(0).unwrap();
        assert!(state.has_double_jumped);
        assert!(!state.is_jumping);
        assert!(!state.is_flipping);
    }

    #[test]
    fn arena_state_contains_enriched_snapshot() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        enricher.update(&packet(1, player())).unwrap();

        let snapshot = enricher.arena_state();
        assert_eq!(snapshot.num_cars(), 1);
        assert_eq!(snapshot.cars[0].0.idx, 0);
        assert_eq!(snapshot.ball.phys.pos, enricher.ball_state().phys.pos);
    }

    #[test]
    fn absent_boost_pad_states_leave_non_context_arena_unchanged() {
        init_from_default(true).unwrap();
        let arena = Arena::new(GameMode::Soccar);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let pad_index = 0;
        let mut first = packet(1, player());
        first.boost_pads = (0..enricher.arena().num_boost_pads())
            .map(|_| RlbotBoostPadState {
                is_active: true,
                timer: 0.0,
            })
            .collect();
        first.boost_pads[pad_index] = RlbotBoostPadState {
            is_active: false,
            timer: 2.5,
        };
        enricher.update(&first).unwrap();
        let cooldown = enricher.arena().get_boost_pad_state(pad_index).cooldown;

        let mut second = packet(2, player());
        second.match_info.match_phase = MatchPhase::Paused;
        enricher.update(&second).unwrap();

        assert_eq!(
            enricher.arena().get_boost_pad_state(pad_index).cooldown,
            cooldown
        );
    }

    #[test]
    fn converts_elapsed_rlbot_boost_timer_to_remaining_cooldown() {
        init_from_default(true).unwrap();
        let arena = Arena::new(GameMode::Soccar);
        let max_cooldown = arena.mutator_config().boost_pad_cooldown_big;
        let big_pad = (0..arena.num_boost_pads())
            .find(|&index| arena.get_boost_pad_config(index).is_big)
            .unwrap();
        let mut packet = GamePacket {
            boost_pads: (0..arena.num_boost_pads())
                .map(|_| RlbotBoostPadState {
                    is_active: true,
                    timer: 0.0,
                })
                .collect(),
            ..GamePacket::default()
        };
        packet.boost_pads[big_pad] = RlbotBoostPadState {
            is_active: false,
            timer: 2.5,
        };
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());

        enricher.update(&packet).unwrap();

        assert_eq!(
            enricher.arena().get_boost_pad_state(big_pad).cooldown,
            max_cooldown - 2.5
        );
    }

    #[test]
    fn demolished_players_do_not_retain_contacts() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        enricher.update(&packet(1, player())).unwrap();
        let mut prior = *enricher.car_state(0).unwrap();
        prior.is_on_ground = true;
        prior.wheels_with_contact = [Some(RaycastHitInfo {
            hit_point: Vec3A::ZERO,
            hit_normal: Vec3A::Z,
            hit_fraction: 0.0,
            user_info: UserInfoType::None,
        }); 4];
        prior.world_contact_normal = Some(Vec3A::Z);
        enricher.arena_mut().set_car_state(0, prior);

        let mut demoed = player();
        demoed.demolished_timeout = 2.0;
        enricher.update(&packet(2, demoed)).unwrap();
        let state = enricher.car_state(0).unwrap();
        assert!(state.is_demoed);
        assert!(!state.is_on_ground);
        assert_eq!(state.wheels_with_contact, [None; 4]);
        assert_eq!(state.world_contact_normal, None);
    }

    #[test]
    fn rlbot_air_and_demo_states_follow_their_documented_meaning() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut jumping = player();
        jumping.air_state = AirState::Jumping;
        jumping.has_jumped = true;
        jumping.dodge_elapsed = 4.0;
        jumping.demolished_timeout = 0.0;

        enricher.update(&packet(1, jumping)).unwrap();
        let state = enricher.car_state(0).unwrap();

        assert!(!state.is_on_ground);
        assert!(state.is_jumping);
        assert_eq!(state.air_time_since_jump, 0.0);
        assert_eq!(state.flip_time, 0.0);
        assert!(state.is_demoed);
        assert_eq!(state.demo_respawn_timer, 0.0);
    }

    #[test]
    fn rejects_unsupported_teams_without_mutating_layout() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut invalid = player();
        invalid.team = 2;

        assert_eq!(
            enricher.update(&packet(1, invalid)),
            Err(EnrichmentError::InvalidTeam {
                player_index: 0,
                team: 2,
            })
        );
        assert_eq!(enricher.arena().num_cars(), 0);
    }
}
