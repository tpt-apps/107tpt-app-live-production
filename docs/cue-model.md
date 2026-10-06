# Cue Model

The cue is the product's central abstraction (spec 6.5, 10): one operator
action that commits a video transition, changes audio, and recalls a
lighting scene — together.

```rust
pub struct Cue {
    pub id: CueId,
    pub number: CueNumber,          // unique, operator-facing
    pub label: String,
    pub video_transition: Option<Transition>, // Cut | Fade | Wipe
    pub preview_source: Option<SourceId>,     // put on PVW before the take
    pub audio_changes: Vec<AudioChange>,      // gain/mute/pan/bus, ramped
    pub lighting_scene: Option<LightingSceneId>,
    pub advance: AdvanceMode,       // Manual | Timed | Follow
}
```

## Advance semantics

A cue's `advance` field describes **how the cue exits** — i.e. when the
*next* cue fires:

- `Manual` — the next cue waits for GO (button, spacebar, OSC, API).
- `Timed { after_ms }` — the next cue fires this long after this cue
  fired, regardless of transition state.
- `Follow` — the next cue fires as soon as this cue's video transition
  completes.

`tpt-app-live-production-cues::CueStackRunner` owns this state machine;
`core::LiveEngine::poll` feeds it the clock and transition-completion
signal.

## Execution order within one cue

1. `preview_source` → PVW bus (frame N),
2. `video_transition` → TAKE, quantized to frames starting at frame N,
3. audio changes → ramps beginning this audio block,
4. lighting scene → fade starting this tick.

All four land in the same poll, which is what makes a single cue affect
video, audio, and lighting "in sync" (spec 5.1 `tpt-av-sync` refines the
cross-device timing as backends come online).

## Live-safe editing

Insert/remove/replace/move are safe while a cue is live: the runner
tracks the live cue by id, and `CueStack` operations keep indices
consistent. Editing the live cue never restarts it; the edit applies the
next time that cue fires.
