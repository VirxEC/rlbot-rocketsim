use glam::Vec3A;
use rlbot::flat::{AirState, CollisionShape, GamePacket, MatchPhase, PlayerInfo};
use rocketsim::{
    Arena, ArenaConfig, ArenaState, BallState, BoostPadState, CarBodyConfig, CarControls, CarState,
    GameMode, Team, consts,
};
use thiserror::Error;

use crate::common::{controls_from_rlbot, physics_from_rlbot};
use crate::match_context::{MatchContext, MatchContextError};

const MAX_FRAME_DELTA: u32 = 120;

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

    #[must_use]
    pub fn car_state(&self, player_index: usize) -> Option<&CarState> {
        self.players
            .get(player_index)
            .map(|player| self.arena.get_car_state(player.car_index))
    }

    /// Synchronizes packet-authoritative state, advances RocketSim according to
    /// the RLBot frame delta when gameplay is active, and restores packet values.
    pub fn update(&mut self, packet: &GamePacket) -> Result<Vec<EnrichedPlayer>, EnrichmentError> {
        self.validate_packet(packet)?;

        let frame = packet.match_info.frame_num;
        let frame_rollback = self.last_frame.is_some_and(|last| frame < last);
        let layout_changed = self.layout_changed(packet)?;
        if frame_rollback || layout_changed {
            self.rebuild_arena();
        }

        let reset_history = self.last_frame.is_none() || frame_rollback || layout_changed;
        let ticks_to_step = self.ticks_to_step(packet, reset_history);
        let resolved = self.resolve_players(packet)?;
        self.apply_players(packet, &resolved, reset_history);
        self.sync_ball(packet)?;
        self.sync_boost_pads(packet)?;

        for _ in 0..ticks_to_step {
            self.arena.step_tick();
        }

        self.restore_authoritative_ball(packet);
        self.restore_authoritative_boost_pads(packet)?;
        self.restore_authoritative_players(packet);
        self.last_frame = Some(frame);

        Ok(resolved
            .iter()
            .enumerate()
            .map(|(player_index, resolved)| EnrichedPlayer {
                player_index,
                car_index: resolved.car_index,
            })
            .collect())
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
        if !packet.boost_pads.is_empty() && packet.boost_pads.len() != self.arena.num_boost_pads() {
            return Err(MatchContextError::BoostPadCountMismatch {
                packet: packet.boost_pads.len(),
                arena: self.arena.num_boost_pads(),
            }
            .into());
        }
        for (player_index, player) in packet.players.iter().enumerate() {
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
        if packet.players.len() != self.players.len() && !self.players.is_empty() {
            return Ok(true);
        }
        for (index, player) in packet.players.iter().enumerate() {
            let Some(tracked) = self.players.get(index) else {
                continue;
            };
            let team = team_from_rlbot(index, player)?;
            let body = self.body_config_for_player(index, player)?;
            if tracked.player_id != player.player_id
                || tracked.team != team
                || tracked.body_config != body
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn rebuild_arena(&mut self) {
        self.arena = Arena::new_with_config(self.arena_config.clone());
        self.players.clear();
        self.last_frame = None;
    }

    fn ticks_to_step(&self, packet: &GamePacket, reset_history: bool) -> u32 {
        if !phase_advances(packet.match_info.match_phase) {
            return 0;
        }
        if reset_history {
            return 1;
        }
        let Some(last_frame) = self.last_frame else {
            return 1;
        };
        packet
            .match_info
            .frame_num
            .saturating_sub(last_frame)
            .min(MAX_FRAME_DELTA)
    }

    fn resolve_players(
        &mut self,
        packet: &GamePacket,
    ) -> Result<Vec<ResolvedPlayer>, EnrichmentError> {
        let mut resolved = Vec::with_capacity(packet.players.len());
        for (index, player) in packet.players.iter().enumerate() {
            let team = team_from_rlbot(index, player)?;
            let body_config = self.body_config_for_player(index, player)?;
            let car_index = if let Some(tracked) = self.players.get(index) {
                tracked.car_index
            } else {
                let car_index = self.arena.add_car(team, body_config);
                self.players.push(TrackedPlayer {
                    car_index,
                    player_id: player.player_id,
                    team,
                    body_config,
                    previous_controls: CarControls::default(),
                });
                car_index
            };
            resolved.push(ResolvedPlayer {
                car_index,
                previous_controls: self.players[index].previous_controls,
            });
        }
        Ok(resolved)
    }

    fn apply_players(
        &mut self,
        packet: &GamePacket,
        resolved: &[ResolvedPlayer],
        reset_history: bool,
    ) {
        for (index, (player, resolved)) in packet.players.iter().zip(resolved).enumerate() {
            let previous = *self.arena.get_car_state(resolved.car_index);
            let previous_controls = if reset_history {
                controls_from_rlbot(player.last_input)
            } else {
                resolved.previous_controls
            };
            let state =
                merge_authoritative_player(player, previous, previous_controls, reset_history);
            self.arena.set_car_state(resolved.car_index, state);
            self.arena
                .set_car_controls(resolved.car_index, state.controls);
            self.players[index].previous_controls = controls_from_rlbot(player.last_input);
        }
    }

    fn restore_authoritative_players(&mut self, packet: &GamePacket) {
        for (index, player) in packet.players.iter().enumerate() {
            let car_index = self.players[index].car_index;
            let mut state = *self.arena.get_car_state(car_index);
            restore_authoritative_player(player, &mut state);
            self.arena.set_car_state(car_index, state);
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

    fn restore_authoritative_ball(&mut self, packet: &GamePacket) {
        let [ball] = packet.balls.as_slice() else {
            return;
        };
        let mut state = *self.arena.get_ball_state();
        apply_ball(ball, &mut state);
        self.arena.set_ball_state(state);
    }

    fn sync_boost_pads(&mut self, packet: &GamePacket) -> Result<(), EnrichmentError> {
        apply_boost_pads(&mut self.arena, packet)
    }

    fn restore_authoritative_boost_pads(
        &mut self,
        packet: &GamePacket,
    ) -> Result<(), EnrichmentError> {
        apply_boost_pads(&mut self.arena, packet)
    }
}

#[derive(Clone, Copy)]
struct ResolvedPlayer {
    car_index: usize,
    previous_controls: CarControls,
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
        arena.set_boost_pad_state(
            index,
            BoostPadState {
                cooldown: if pad.is_active { 0.0 } else { pad.timer },
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
    reset_history: bool,
) -> CarState {
    let controls = controls_from_rlbot(player.last_input);
    let is_on_ground = player.air_state == AirState::OnGround;

    restore_authoritative_player(player, &mut state);
    state.prev_controls = previous_controls;

    if reset_history {
        state.prev_controls = controls;
        state.wheels_with_contact = if is_on_ground { [true; 4] } else { [false; 4] };
        state.world_contact_normal = None;
        state.air_time = 0.0;
        state.jump_time = 0.0;
        state.boosting_time = 0.0;
        state.time_since_boosted = 0.0;
        state.handbrake_val = f32::from(controls.handbrake);
    }

    state
}

fn restore_authoritative_player(player: &PlayerInfo, state: &mut CarState) {
    state.phys = physics_from_rlbot(player.physics);
    state.controls = controls_from_rlbot(player.last_input);
    state.is_on_ground = player.air_state == AirState::OnGround;
    state.has_jumped = player.has_jumped;
    state.has_double_jumped = player.has_double_jumped;
    state.has_flipped = player.has_dodged;
    state.flip_rel_torque = Vec3A::new(-player.dodge_dir.y, player.dodge_dir.x, 0.0);
    state.flip_time = player.dodge_elapsed.max(0.0);
    state.is_flipping = player.air_state == AirState::Dodging;
    state.is_jumping = player.air_state == AirState::Jumping;
    state.air_time_since_jump = if player.dodge_timeout >= 0.0 {
        (consts::car::jump::DOUBLEJUMP_MAX_DELAY - player.dodge_timeout)
            .clamp(0.0, consts::car::jump::DOUBLEJUMP_MAX_DELAY)
    } else if player.has_jumped {
        consts::car::jump::DOUBLEJUMP_MAX_DELAY
    } else {
        0.0
    };
    state.boost = player.boost;
    state.is_supersonic = player.is_supersonic;
    state.is_demoed = player.demolished_timeout > 0.0;
    state.demo_respawn_timer = player.demolished_timeout.max(0.0);
}

#[cfg(test)]
mod tests {
    use rlbot::flat::{GamePacket, MatchPhase, Physics, PlayerInfo, Vector3};
    use rocketsim::{Arena, CarBodyConfig, GameMode};

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
    fn packet_index_is_the_canonical_car_identity() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        let mut packet = packet(1, player());
        packet.players.push(player());
        packet.players[1].physics.location.x = 999.0;

        let mappings = enricher.update(&packet).unwrap();

        assert_eq!(mappings[0].car_index, 0);
        assert_eq!(mappings[1].car_index, 1);
        assert_eq!(enricher.car_state(1).unwrap().phys.pos.x, 999.0);
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
    fn frame_gaps_advance_rocketsim_with_a_bound() {
        let arena = Arena::new(GameMode::TheVoid);
        let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
        enricher.update(&packet(1, player())).unwrap();
        let tick = enricher.arena().tick_count();

        enricher.update(&packet(5, player())).unwrap();
        assert_eq!(enricher.arena().tick_count(), tick + 4);

        enricher.update(&packet(1_000, player())).unwrap();
        assert_eq!(
            enricher.arena().tick_count(),
            tick + 4 + MAX_FRAME_DELTA as u64
        );
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
