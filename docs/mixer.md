# Audio Mixer

Real-time mixing engine (spec 8) in `tpt-app-live-production-mixer`.

## Layout

- One **channel strip** per audio-capable source: gain (dB), mute, pan
  (constant-power law), each with an amplitude ramp.
- **Buses** accumulate patched strips into interleaved stereo blocks
  (program first, then aux). Bus membership is setup-time state.
- Per-bus **metering** (peak + RMS per block) runs in both modes — an
  operator rehearsing must see meters.

## Real-time constraints (spec 3.1)

- Every buffer is allocated at setup; `render_block` allocates nothing
  (enforced by `tests/rt_allocation.rs` with the `tpt-av-test-benchmark`
  tracking allocator),
- no blocking I/O — `render_block` is pure computation over
  caller-provided blocks,
- fixed per-block ramp resolution: one amplitude step per block
  (128 frames @ 48 kHz ≈ 2.7 ms), which keeps clicks below audibility
  while making the render path branch-predictable.

## Ramps, not steps (spec 8)

Gain/mute/pan changes interpolate over a ramp duration (show default
`ramp_ms`, or explicit `SmoothOver`/`Instant` in cues). A mid-ramp
change starts from wherever the previous ramp currently is — changes
never jump. `Instant` exists for explicit operator hard-cuts; validation
warns when a cue uses it (it can click).

## Malformed input

Short or missing input blocks render as silence; out-of-range or
non-finite parameters are rejected without mutating state. A bad input
must never panic or glitch program audio (spec 20).
