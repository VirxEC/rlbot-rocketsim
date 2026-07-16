use glam::{EulerRot, Mat3A, Vec3A};
use rlbot_rocketsim::rlbot::flat::{
    CustomBot, MatchConfiguration, PlayerClass, PlayerConfiguration, PlayerLoadout,
};
use rlbot_rocketsim::rocketsim::{
    Arena, CarBodyConfig, CarControls, CarState, GameMode, PhysState, Team, init_from_default,
};
use rlbot_rocketsim::to_rlbot::ArenaExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_from_default(true)?;
    let mut arena = Arena::new(GameMode::Soccar);
    let car_index = arena.add_car(Team::Blue, CarBodyConfig::OCTANE);
    arena.set_car_state(
        car_index,
        CarState {
            phys: PhysState {
                pos: Vec3A::new(100.0, -2_000.0, 17.0),
                rot_mat: Mat3A::from_euler(EulerRot::ZYX, 0.5, 0.1, -0.2),
                vel: Vec3A::new(1_200.0, 300.0, 0.0),
                ang_vel: Vec3A::new(0.0, 0.0, 1.5),
            },
            controls: CarControls {
                throttle: 1.0,
                steer: 0.25,
                boost: true,
                ..CarControls::default()
            },
            boost: 62.0,
            is_on_ground: true,
            wheels_with_contact: [true; 4],
            ..CarState::default()
        },
    );

    let match_config = MatchConfiguration {
        player_configurations: vec![PlayerConfiguration {
            variety: PlayerClass::CustomBot(Box::new(CustomBot {
                name: "RocketSim Example".into(),
                loadout: Some(Box::new(PlayerLoadout {
                    car_id: 23,
                    ..PlayerLoadout::default()
                })),
                ..CustomBot::default()
            })),
            team: 0,
            player_id: 7,
        }],
        ..MatchConfiguration::default()
    };
    // `ArenaExt` is stateless. Use `car_to_player_info_with_history` instead
    // when packet-derived jump/double-jump history must be preserved exactly.
    let players = arena.to_rlbot_players(&match_config)?;
    let player = &players[car_index];

    println!("GamePacket.players index: {car_index}");
    println!("RLBot participant ID metadata: {}", player.player_id);
    println!("team: {}", player.team);
    println!("position: {:?}", player.physics.location);
    println!("rotation: {:?}", player.physics.rotation);
    println!("last input: {:?}", player.last_input);
    println!("air state: {:?}", player.air_state);

    Ok(())
}
