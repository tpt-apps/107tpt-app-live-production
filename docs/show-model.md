# Show Model

Source of truth: `crates/tpt-app-live-production-model` (spec 6). A show
is the complete, saveable description of an event:

```rust
pub struct Show {
    pub id: ShowId,
    pub name: String,
    pub sources: Vec<Source>,      // cameras, mics, playback assets, graphics
    pub outputs: Vec<Output>,      // program/aux video+audio, lighting universes
    pub buses: Vec<AudioBus>,      // input patches summed to outputs
    pub fixtures: Vec<Fixture>,    // DMX fixtures (universe + channels)
    pub scenes: Vec<LightingScene>,// recallable looks with fade times
    pub cue_stack: CueStack,
    pub mode: OperationMode,       // Rehearsal | Live
    pub settings: ShowSettings,    // fps, sample rate, block size, ramp
}
```

Design notes:

- **IDs are human-chosen strings** (`"cam1"`, `"pgm_video"`). Show files
  are hand-editable, so ids must be readable and stable across edits.
- **`CueStack::current_index` is `Option<usize>`** (spec 6.5 shows a bare
  `usize`): before the first GO there is no live cue. Editing operations
  keep the live cue's identity stable through inserts/removes/reorders —
  `tests::editing_cues_mid_show_does_not_disrupt_live_cue` pins this.
- **Durations** are `std::time::Duration` in the domain and integer
  milliseconds in the file; conversion helpers live in
  `model::showfile::duration_ms`.
- **Validation** (`model::validation`) classifies findings as errors
  (refuse to run) or warnings (operator may proceed). The engine refuses
  to build from a show with error-severity issues, so an invalid show can
  never reach the stage.
