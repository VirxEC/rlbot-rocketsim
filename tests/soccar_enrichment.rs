use rlbot_rocketsim::GameStateEnricher;
use rlbot_rocketsim::rlbot::flat::{AirState, GamePacket, Physics, PlayerInfo, Vector3};
use rlbot_rocketsim::rocketsim::{Arena, CarBodyConfig, GameMode, init_from_default};

#[test]
fn enriches_wheel_contacts_in_soccar() {
    init_from_default(true).unwrap();

    let arena = Arena::new(GameMode::Soccar);
    let mut enricher = GameStateEnricher::new(arena, CarBodyConfig::default());
    let player = PlayerInfo {
        player_id: 7,
        team: 0,
        physics: Physics {
            location: Vector3 {
                x: 0.0,
                y: -2_000.0,
                z: 17.0,
            },
            ..Physics::default()
        },
        air_state: AirState::OnGround,
        dodge_timeout: -1.0,
        demolished_timeout: -1.0,
        ..PlayerInfo::default()
    };
    let mut packet = GamePacket::default();
    packet.match_info.frame_num = 1;
    packet.players.push(player);

    enricher.update(&packet).unwrap();

    let state = enricher.car_state(0).unwrap();
    assert_eq!(state.wheels_with_contact, [true; 4]);
    assert_eq!(state.phys.pos.z, 17.0);
}
