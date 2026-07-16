use glam::Vec3A;
use rlbot::flat::{AirState, CollisionShape, GamePacket, MatchPhase, PlayerInfo};
use rocketsim::{
    Arena, ArenaConfig, ArenaState, BallState, BoostPadState, CarBodyConfig, CarControls, CarState,
    GameMode, Team, consts,
};
use thiserror::Error;

use crate::common::{controls_from_rlbot, physics_from_rlbot};
use crate::match_context::{MatchContext, MatchContextError};
use crate::to_rlbot::CarConversionHistory;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnrichedPlayer {
    pub player_index: usize,
    pub car_index: usize,
}

#[derive(Debug, Error, PartialEq)]
pub enum EnrichmentError {
    #[error(transparent)]
    MatchContext(#[from] MatchContextError),
    #[error("player at packet index {player_index} has unsupported team index {team}")]
    InvalidTeam { player_index: usize, team: u32 },
    #[error("RLBot packet contains duplicate participant ID {player_id}")]
    DuplicatePlayerId { player_id: i32 },
    #[error("expected exactly one RLBot ball, got {count}")]
    BallCount { count: usize },
    #[error("RLBot ball shape {shape} is incompatible with RocketSim mode {mode:?}")]
    BallShape { shape: &'static str, mode: GameMode },
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

/// Maintains a RocketSim arena whose cars follow RLBot packets while RocketSim
/// supplies contact and history-dependent state unavailable from RLBot.
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

    #[must_use]
    pub const fn arena(&self) -> &Arena {
        &self.arena
    }

    pub const fn arena_mut(&mut self) -> &mut Arena {
        &mut self.arena
    }

    #[must_use]
    pub fn arena_state(&self) -> ArenaState {
        self.arena.get_arena_state()
    }

    #[must_use]
    pub const fn ball_state(&self) -> &BallState {
        self.arena.get_ball_state()
    }

    /// Returns the enriched car state for a slot in the latest `GamePacket.players`.
    ///
    /// Packet slots can change when player order changes; use
    /// [`Self::car_state_by_player_id`] for stable participant identity.
    #[must_use]
    pub fn car_state(&self, packet_player_index: usize) -> Option<&CarState> {
        self.players
            .get(packet_player_index)
            .map(|player| self.arena.get_car_state(player.car_index))
    }

    /// Returns the enriched car state for an RLBot participant ID.
    #[must_use]
    pub fn car_state_by_player_id(&self, player_id: i32) -> Option<&CarState> {
        self.players
            .iter()
            .find(|player| player.player_id == player_id)
            .map(|player| self.arena.get_car_state(player.car_index))
    }

    /// Returns conversion history for a slot in the latest `GamePacket.players`.
    ///
    /// Packet slots can change when player order changes; use
    /// [`Self::car_conversion_history_by_player_id`] for stable participant identity.
    #[must_use]
    pub fn car_conversion_history(
        &self,
        packet_player_index: usize,
    ) -> Option<CarConversionHistory> {
        self.players
            .get(packet_player_index)
            .map(car_conversion_history)
    }

    /// Returns the conversion history for an RLBot participant ID.
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

    /// Returns retained initial-jump duration for a slot in the latest packet.
    ///
    /// Packet slots can change when player order changes.
    #[must_use]
    pub fn initial_jump_duration(&self, packet_player_index: usize) -> Option<f32> {
        self.car_conversion_history(packet_player_index)
            .map(|history| history.initial_jump_duration)
    }

    /// Synchronizes packet-authoritative state, advances RocketSim once to derive
    /// enrichment for a new active frame, and restores packet values.
    ///
    /// Player additions preserve existing history. Departures and immutable car
    /// changes rebuild the arena because RocketSim does not expose car removal.
    pub fn update(&mut self, packet: &GamePacket) -> Result<Vec<EnrichedPlayer>, EnrichmentError> {
        self.validate_packet(packet)?;

        let frame = packet.match_info.frame_num;
        let frame_rollback = self.last_frame.is_some_and(|last| frame < last);
        let layout_changed = self.layout_changed(packet)?;
        let gravity_changed =
            self.arena_config.mutators.gravity.z != packet.match_info.world_gravity_z;
        if gravity_changed {
            self.arena_config.mutators.gravity.z = packet.match_info.world_gravity_z;
        }
        if frame_rollback || layout_changed || gravity_changed {
            self.rebuild_arena();
        }

        let resumed_after_inactive = phase_advances(packet.match_info.match_phase)
            && self.last_phase.is_some_and(|phase| !phase_advances(phase));
        let reset_history = self.last_frame.is_none()
            || frame_rollback
            || layout_changed
            || gravity_changed
            || resumed_after_inactive;
        let should_step = self.should_step(packet);
        if reset_history {
            for tracked in &mut self.players {
                tracked.initial_jump_duration = 0.0;
                tracked.double_jump_active = false;
                tracked.flip_reset_available = false;
            }
        }
        let resolved = self.resolve_players(packet)?;

        if should_step {
            for (player, resolved) in packet.players.iter().zip(&resolved) {
                self.arena
                    .set_car_controls(resolved.car_index, controls_from_rlbot(player.last_input));
            }
            self.arena.step_tick();
        }

        self.apply_players(packet, &resolved, reset_history, should_step);
        self.sync_ball(packet)?;
        self.sync_boost_pads(packet)?;
        self.last_frame = Some(frame);
        self.last_phase = Some(packet.match_info.match_phase);

        let mut enriched_players = Vec::with_capacity(resolved.len());
        for (player_index, resolved) in resolved.iter().enumerate() {
            enriched_players.push(EnrichedPlayer {
                player_index,
                car_index: resolved.car_index,
            });
        }
        Ok(enriched_players)
    }

    fn validate_packet(&self, packet: &GamePacket) -> Result<(), EnrichmentError> {
        if self.match_context.is_some() && packet.balls.len() != 1 {
            return Err(EnrichmentError::BallCount {
                count: packet.balls.len(),
            });
        }
        if let [ball] = packet.balls.as_slice() {
            self.validate_ball_shape(ball)?;
        }
        if self.match_context.is_some() && packet.boost_pads.len() != self.arena.num_boost_pads() {
            return Err(MatchContextError::BoostPadCountMismatch {
                packet: packet.boost_pads.len(),
                arena: self.arena.num_boost_pads(),
            }
            .into());
        }
        for (player_index, player) in packet.players.iter().enumerate() {
            if packet.players[..player_index]
                .iter()
                .any(|other| other.player_id == player.player_id)
            {
                return Err(EnrichmentError::DuplicatePlayerId {
                    player_id: player.player_id,
                });
            }
            team_from_rlbot(player_index, player)?;
            self.body_config_for_player(player_index, player)?;
        }
        Ok(())
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

    fn layout_changed(&self, packet: &GamePacket) -> Result<bool, EnrichmentError> {
        if self.players.is_empty() {
            return Ok(false);
        }
        if packet.players.len() < self.players.len()
            || self.players.iter().any(|tracked| {
                !packet
                    .players
                    .iter()
                    .any(|player| player.player_id == tracked.player_id)
            })
        {
            return Ok(true);
        }
        for (index, player) in packet.players.iter().enumerate() {
            let Some(tracked) = self
                .players
                .iter()
                .find(|tracked| tracked.player_id == player.player_id)
            else {
                continue;
            };
            let team = team_from_rlbot(index, player)?;
            let body = self.body_config_for_player(index, player)?;
            if tracked.team != team || tracked.body_config != body {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn rebuild_arena(&mut self) {
        self.arena = Arena::new_with_config(self.arena_config.clone());
        self.players.clear();
        self.last_frame = None;
        self.last_phase = None;
    }

    fn should_step(&self, packet: &GamePacket) -> bool {
        phase_advances(packet.match_info.match_phase)
            && self
                .last_frame
                .is_some_and(|last| packet.match_info.frame_num > last)
    }

    fn resolve_players(
        &mut self,
        packet: &GamePacket,
    ) -> Result<Vec<ResolvedPlayer>, EnrichmentError> {
        let mut ordered_players = Vec::with_capacity(packet.players.len());
        for (index, player) in packet.players.iter().enumerate() {
            let team = team_from_rlbot(index, player)?;
            let body_config = self.body_config_for_player(index, player)?;
            let tracked = if let Some(position) = self
                .players
                .iter()
                .position(|tracked| tracked.player_id == player.player_id)
            {
                self.players.remove(position)
            } else {
                TrackedPlayer {
                    car_index: self.arena.add_car(team, body_config),
                    player_id: player.player_id,
                    team,
                    body_config,
                    previous_controls: CarControls::default(),
                    initial_jump_duration: 0.0,
                    double_jump_active: false,
                    flip_reset_available: false,
                }
            };
            ordered_players.push(tracked);
        }
        self.players = ordered_players;
        Ok(self
            .players
            .iter()
            .map(|tracked| ResolvedPlayer {
                car_index: tracked.car_index,
                previous_controls: tracked.previous_controls,
                initial_jump_duration: tracked.initial_jump_duration,
                flip_reset_available: tracked.flip_reset_available,
            })
            .collect())
    }

    fn apply_players(
        &mut self,
        packet: &GamePacket,
        resolved: &[ResolvedPlayer],
        reset_history: bool,
        stepped: bool,
    ) {
        for (index, (player, resolved)) in packet.players.iter().zip(resolved).enumerate() {
            let mut simulated = *self.arena.get_car_state(resolved.car_index);
            simulated.is_on_ground = simulated.num_wheels_in_contact() >= 3;
            let previous_controls = if stepped {
                simulated.prev_controls
            } else if reset_history {
                controls_from_rlbot(player.last_input)
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
            self.players[index].previous_controls = controls_from_rlbot(player.last_input);
            if player.air_state == AirState::Jumping {
                self.players[index].initial_jump_duration =
                    state.jump_time.clamp(0.0, consts::car::jump::MAX_TIME);
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

    fn sync_ball(&mut self, packet: &GamePacket) -> Result<(), EnrichmentError> {
        let ball = match packet.balls.as_slice() {
            [ball] => ball,
            [] if self.match_context.is_none() => return Ok(()),
            balls => return Err(EnrichmentError::BallCount { count: balls.len() }),
        };
        let mut state = *self.arena.get_ball_state();
        apply_ball(ball, &mut state);
        self.arena.set_ball_state(state);
        Ok(())
    }

    fn sync_boost_pads(&mut self, packet: &GamePacket) -> Result<(), EnrichmentError> {
        apply_boost_pads(&mut self.arena, packet)
    }
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

fn apply_boost_pads(arena: &mut Arena, packet: &GamePacket) -> Result<(), EnrichmentError> {
    if packet.boost_pads.is_empty() {
        return Ok(());
    }
    if packet.boost_pads.len() != arena.num_boost_pads() {
        return Err(MatchContextError::BoostPadCountMismatch {
            packet: packet.boost_pads.len(),
            arena: arena.num_boost_pads(),
        }
        .into());
    }
    for (index, pad) in packet.boost_pads.iter().enumerate() {
        let config = arena.get_boost_pad_config(index);
        let max_cooldown = if config.is_big {
            arena.mutator_config().boost_pad_cooldown_big
        } else {
            arena.mutator_config().boost_pad_cooldown_small
        };
        arena.set_boost_pad_state(
            index,
            BoostPadState {
                // RLBot reports elapsed time since pickup; RocketSim stores time remaining.
                cooldown: if pad.is_active {
                    0.0
                } else {
                    (max_cooldown - pad.timer).clamp(0.0, max_cooldown)
                },
            },
        );
    }
    Ok(())
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
        state.jump_time = 0.0;
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
        state.wheels_with_contact = [false; 4];
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
        // RLBot does not expose elapsed initial-jump time. Keep RocketSim's estimate while
        // the state is continuous, bounded by the documented maximum hold duration.
        state.jump_time = state.jump_time.clamp(0.0, consts::car::jump::MAX_TIME);
        state.air_time_since_jump = 0.0;
    } else {
        state.jump_time = 0.0;
        if !player.has_jumped {
            state.air_time_since_jump = 0.0;
        } else if player.dodge_timeout >= 0.0 {
            state.air_time_since_jump = (consts::car::jump::DOUBLEJUMP_MAX_DELAY
                + initial_jump_duration.clamp(0.0, consts::car::jump::MAX_TIME)
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
    use rocketsim::{Arena, CarBodyConfig, GameMode, init_from_default};

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
        assert_eq!(state.jump_time, 0.0);
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
        prior.wheels_with_contact = [true; 4];
        prior.world_contact_normal = Some(Vec3A::Z);
        enricher.arena_mut().set_car_state(0, prior);

        let mut demoed = player();
        demoed.demolished_timeout = 2.0;
        enricher.update(&packet(2, demoed)).unwrap();
        let state = enricher.car_state(0).unwrap();
        assert!(state.is_demoed);
        assert!(!state.is_on_ground);
        assert_eq!(state.wheels_with_contact, [false; 4]);
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
