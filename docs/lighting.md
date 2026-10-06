# Lighting Cue Engine

Scene recall with fade timing (spec 9) in
`tpt-app-live-production-lighting`. Effects/chases are explicitly
post-MVP (spec 22).

## Model

- **Fixtures** patch DMX channels (absolute addresses 1..=512) in a
  universe; the engine maps the show's `UniverseId` strings to numeric
  DMX universes (via `lighting_universe` outputs, or a numeric universe
  id directly).
- **Scenes** set absolute values per fixture with a fade time. Fixtures a
  scene doesn't mention hold their current values.
- **Fades** interpolate linearly (8-bit, rounded) from the *current
  blended state* to the target — recalling mid-fade starts from wherever
  the lights actually are, never from a stale snapshot.

## Output

`DmxSink` receives full 512-channel universe frames. Implementations:

- `RecordingSink` — tests,
- `NullSink` — default (the engine only calls the sink in Live mode),
- `sacn::SacnSink` (feature `sacn`) — sACN E1.31 via
  `tpt-av-control-dmx`; send failures log and retry next tick rather
  than taking the engine down.

Time is caller-supplied monotonic ms, keeping the engine deterministic
and testable; `core` derives that clock from the program frame clock
(`tpt-av-sync` refines cross-device correlation).

Note (spec 26): this is scene recall in service of the unified cue stack
— not a lighting console replaceme
