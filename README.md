# rlbot-rocketsim

Conversions between RocketSim state types and RLBot v5 owned FlatBuffer types.

The two directions are intentionally separate:

- `to_rlbot` performs deterministic conversion from RocketSim `CarInfo` and
  `CarState`, with extension traits for both `CarInfo` and `Arena`. `CarInfo::idx`
  corresponds to the converted player's position in `GamePacket.players`.
- `from_rlbot` provides a stateful `GameStateEnricher` backed by a persistent
  RocketSim `Arena`. RLBot packet fields remain authoritative while RocketSim
  supplies contact and history-dependent state unavailable from RLBot.

Round trips are useful for validating shared fields, but are not the intended
production data flow.

## Examples

Convert RocketSim arena cars (`CarInfo` + `CarState`) into RLBot `PlayerInfo`
values using `ArenaExt` and a `MatchConfiguration`:

```sh
cargo run --example rocketsim_to_rlbot
```

Statefully enrich RLBot `GamePacket` players into persistent RocketSim cars:

```sh
cargo run --example rlbot_to_rocketsim
```

The RLBot-to-RocketSim example is a real RLBot v5 `BotAgent`. Its constructor
builds a `MatchContext` from `MatchConfiguration` and `FieldInfo`, and each tick
updates RocketSim from the live `GamePacket`. Run it through RLBot from this
repository so RocketSim can find `collision_meshes/`.

`MatchContext` maps RLBot game modes, uses `FieldInfo` boost-pad positions, and
resolves each player's RocketSim hitbox from its configured `PlayerLoadout.car_id`.
The product-ID mapping is adapted from the MIT-licensed
[VirxEC/replay-to-rocketsim](https://github.com/VirxEC/replay-to-rocketsim).
Known loadouts are checked against the authoritative packet hitbox; players
without a loadout are resolved from packet hitbox dimensions and offset.
`GamePacket` updates synchronize players, one ball, and boost-pad timers. Since
RocketSim models exactly one ball, context-backed enrichment requires RLBot to
provide exactly one ball and returns an error for zero or multiple balls. Ball
shape and diameter are validated against the RocketSim game mode. The resulting
`BallState` is available through `GameStateEnricher::ball_state`, and a complete
`ArenaState` snapshot is available through `GameStateEnricher::arena_state`.

The enricher advances RocketSim using bounded `MatchInfo.frame_num` deltas only
during kickoff and active play. Repeated frames, pauses, replays, and inactive
phases do not advance simulation. Packet-authoritative car, ball, and boost-pad
state is restored after simulation. Frame rollback or player layout changes
rebuild the arena, preventing departed or replaced players from becoming ghost
colliders. Packet validation completes before arena mutation.

## Round-trip guarantees

RLBot identifies a controlled car using its index in `GamePacket.players` and
`PlayerInput.player_index`. `PlayerInfo.player_id` is participant metadata, not
that index. For RocketSim-to-RLBot conversion, `CarInfo.idx` indexes
`MatchConfiguration.player_configurations` and determines the resulting
`GamePacket.players` position. Team, participant ID, bot status, name, and
loadout validation come from that player configuration. Human names are emitted
as `Human N` because `PlayerClass::Human` contains no name.

The test suite verifies both conversion directions for every field represented
by both `CarState` and `PlayerInfo`: physics, orientation, controls, boost,
supersonic state, jump/dodge flags, active initial-jump and dodge state, dodge
direction and timing, demolition state and timing, team and player metadata,
and hitbox
size and offset. Rotation equality is checked as an orientation matrix because
Euler-angle representations are not unique.

RocketSim-only state such as per-wheel contact, world-contact normals, boost
history, handbrake interpolation, auto-flip state, and collision timers cannot
be encoded in `PlayerInfo`; these are preserved or produced by stateful
RocketSim enrichment instead. Conversely, RocketSim has no active
`DoubleJumping` discriminator corresponding to RLBot's short-lived
`AirState::DoubleJumping`; only the persistent `has_double_jumped` flag can be
round-tripped. RLBot-only scoreboard, touch, accolade, and
Rumble fields cannot be produced from `CarState`.
