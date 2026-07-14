# Agent guidance

## RLBot semantics

Do not infer RLBot field meanings from their names or from RocketSim's similarly
named fields. Read the documentation on the pinned `rlbot::flat` types or the
corresponding FlatBuffer schema before changing conversions.

Important distinctions:

- `PlayerInfo.air_state` describes active ground, jump, double-jump, dodge, or
  free-fall forces. It is not wheel-contact data.
- When producing an RLBot `AirState`, active jump/dodge states take precedence
  over `OnGround`.
- RocketSim `CarState::is_on_ground` means at least three wheels have contact.
  Calculate it from RocketSim wheel contacts; do not copy it from `AirState`.
- RLBot `dodge_timeout` includes the variable initial-jump hold extension.
  RocketSim `air_time_since_jump` starts after the initial jump ends. They are
  not directly invertible.
- RLBot `demolished_timeout == -1` means not demolished. Do not replace this
  sentinel check with a positive-value check.
- RLBot boost-pad `timer` is elapsed time since pickup. RocketSim `cooldown` is
  remaining time until activation.
- `PlayerInfo.player_id` identifies a participant across packets. A player's
  index in `GamePacket.players` is the current packet/control slot.

## Enrichment model

RLBot packet values are authoritative for fields RLBot exposes, including
physics, controls, boost, jump/dodge flags, and demolition state. RocketSim is
responsible for contact and history-dependent fields unavailable from RLBot,
including wheel contacts, `is_on_ground`, contact normals, handbrake
interpolation, and collision history.

Do not make conversions artificially round-trip fields that one side cannot
represent. Tests should verify documented semantics rather than manufactured
reversibility.

## Scope and lifecycle

Unless explicitly requested otherwise, changes should target standard Soccar
without mutators. Do not expand behavior for Dropshot, Rumble, or other modes as
part of unrelated conversion work.

`MatchPhase::Kickoff` and `MatchPhase::Active` both have active physics.
Countdown, replay, pause, and other phases must not advance RocketSim.

When `frame_num` skips values, do not simulate the newest packet once for every
missing frame. Intermediate authoritative states are unavailable. Probe the
current packet at most once for RocketSim-derived enrichment.

Player additions should preserve existing participant history. RocketSim does
not expose car removal, so departures or immutable car changes currently require
an arena rebuild.

## Conversion history

A bare RocketSim `CarState` cannot exactly produce every RLBot field:

- RLBot `dodge_timeout` requires the retained initial-jump duration.
- RLBot `AirState::DoubleJumping` requires transient timing history.

Use `CarConversionHistory` and `car_to_player_info_with_history` when exact
history-assisted conversion is required. Stateless conversion must remain
conservative rather than inventing unavailable timing.

## Validation

After Rust changes, run:

```sh
cargo +nightly fmt
cargo test
cargo clippy --all-targets -- -D warnings
```
