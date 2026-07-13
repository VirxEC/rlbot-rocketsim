use std::sync::Arc;

use rlbot_rocketsim::rlbot::RLBotConnection;
use rlbot_rocketsim::rlbot::agents::{BotAgent, run_bot_agents};
use rlbot_rocketsim::rlbot::flat::{
    ControllableInfo, ControllerState, FieldInfo, GamePacket, MatchConfiguration, PlayerInput,
};
use rlbot_rocketsim::rlbot::util::{AgentEnvironment, PacketQueue};
use rlbot_rocketsim::rocketsim::init_from_default;
use rlbot_rocketsim::{GameStateEnricher, MatchContext};

struct RocketSimAgent {
    index: u32,
    enricher: GameStateEnricher,
}

impl BotAgent for RocketSimAgent {
    fn new(
        _team: u32,
        controllable_info: ControllableInfo,
        match_config: Arc<MatchConfiguration>,
        field_info: Arc<FieldInfo>,
        _packet_queue: &mut PacketQueue,
    ) -> Self {
        init_from_default(true).expect("initialize RocketSim collision meshes");
        let context = MatchContext::new(&match_config, &field_info)
            .expect("build RocketSim match context from RLBot configuration");

        Self {
            index: controllable_info.index,
            enricher: GameStateEnricher::from_match_context(context),
        }
    }

    fn tick(&mut self, game_packet: &GamePacket, packet_queue: &mut PacketQueue) {
        let Ok(mappings) = self.enricher.update(game_packet) else {
            return;
        };
        let player_index = self.index as usize;
        let Some(state) = self.enricher.car_state(player_index) else {
            return;
        };
        let Some(mapping) = mappings.get(player_index) else {
            return;
        };
        let ball = self.enricher.ball_state();

        println!(
            "RLBot player {} -> RocketSim car {}, position {:?}, wheels {:?}, contact {:?}",
            mapping.player_index,
            mapping.car_index,
            state.phys.pos,
            state.wheels_with_contact,
            state.world_contact_normal,
        );
        println!("RocketSim ball position {:?}", ball.phys.pos);

        packet_queue.push(PlayerInput {
            player_index: self.index,
            controller_state: ControllerState::default(),
        });
    }
}

fn main() {
    let AgentEnvironment {
        server_addr,
        agent_id,
    } = AgentEnvironment::from_env();
    let agent_id = agent_id.unwrap_or_else(|| "rlbot-rocketsim/example".into());
    let connection = RLBotConnection::new(&server_addr).expect("connect to RLBot");

    run_bot_agents::<RocketSimAgent>(agent_id, true, true, connection)
        .expect("run RLBot RocketSim example");
}
