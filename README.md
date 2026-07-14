# rlbot-rocketsim

Conversions and stateful enrichment between RLBot v5 packets and RocketSim.

## Usage

Initialize RocketSim, create a match context from RLBot's static match data, and
update the enricher with each game packet:

```rust
use rlbot_rocketsim::{GameStateEnricher, MatchContext};
use rlbot_rocketsim::rocketsim::init_from_default;

init_from_default(true)?;

let context = MatchContext::new(&match_configuration, &field_info)?;
let mut enricher = GameStateEnricher::from_match_context(context);

let players = enricher.update(&game_packet)?;
let car = enricher.car_state(players[0].player_index).unwrap();
let ball = enricher.ball_state();
```

`GameStateEnricher` keeps a persistent RocketSim arena. RLBot remains authoritative
for packet data such as physics, controls, boost, and jump state, while RocketSim
supplies state RLBot does not expose, including wheel contacts, contact normals,
and other simulation history.

For the opposite direction, use `CarInfoExt` or `ArenaExt` to convert RocketSim
cars into RLBot `PlayerInfo` values.

## Examples

```sh
cargo run --example rlbot_to_rocketsim
cargo run --example rocketsim_to_rlbot
```

Run examples from the repository root so RocketSim can find `collision_meshes/`.

## Important limitations

- RocketSim supports one ball. Context-backed enrichment rejects packets with
  zero or multiple balls.
- RLBot `AirState` is not wheel-contact data. Ground and wheel contacts are
  calculated by RocketSim from the car's physics.
- RLBot's `dodge_timeout` cannot be converted exactly into RocketSim's
  `air_time_since_jump`; the two timers measure different intervals.
- Some state exists in only one API and therefore cannot be round-tripped.
