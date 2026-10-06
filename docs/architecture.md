# Architecture

TPT Live Production is a native, real-time operator console that unifies
video switching, audio mixing, and lighting cueing behind one cue stack
(spec 1, 29). The workspace splits along the one load-bearing boundary the
spec calls out (spec 4): the **real-time engine core** and the
**application shells** that host it.

```
┌──────────────────────────  shells  ──────────────────────────┐
│  tauri (desktop console)          cli (validate / rehearse)  │
│         │                                  │                 │
│         └──────────┬───────────────────────┘                 │
│                    ▼                                         │
│  ┌───────────────────────────────────────────────────────┐   │
│  │                 tpt-app-live-production-core          │   │
│  │   LiveEngine: unified cue executor, mode gate,        │   │
│  │   failsafe, degradation, watchdog, session log,       │   │
│  │   optional localhost API                              │   │
│  └───────┬──────────┬──────────┬──────────┬──────────────┘   │
│          ▼          ▼          ▼          ▼                  │
│      switcher    mixer    lighting     cues                 │
│  ┌───────────────────────────────────────────────────────┐   │
│  │                tpt-app-live-production-model          │   │
│  │   Show, Source, Output, Bus, Fixture, Scene, Cue,     │   │
│  │   show-file (TOML), validation                        │   │
│  └───────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────┘
          │            │             │            │
          ▼            ▼             ▼            ▼
   tpt-kinetix   tpt-cadence/   tpt-av-control  tpt-av-sync
   (media)       tpt-audio      (DMX/OSC/MIDI)  (timing)
```

## Key decisions

**The switcher is a pure frame-indexed state machine.** It decides what
each output shows per frame; a render backend executes the decision. This
makes frame-accurate switch timing (spec 7) testable without a GPU, and it
is what the RT allocation gates verify.

**Rehearsal/live isolation is structural, not advisory** (spec 3.3). In
rehearsal the engine computes internal state but the *program output
fields are never populated* — `FrameOutputs.program_video` is `None`,
program audio is empty, no DMX frames are sent. There is no flag a caller
can forget to check.

**The engine never blocks and never allocates on render paths** (spec
3.1). Events publish through bounded queues with drop-on-full; the mixer
preallocates every buffer at setup; the switcher updates a caller-owned
frame buffer in place. `tests/rt_allocation.rs` enforces all three with a
tracking global allocator.

**Cue ordering logic is engine-independent.** `cues` yields CueExecution
records; `core` applies them. That separation is what lets the golden
fixtures and the chaos suite drive the stack headlessly.

**Foundation crates are integrated, not reimplemented** (spec 5): sACN
output via `tpt-av-control-dmx`, OSC surfaces via `tpt-av-control-osc`,
RT-safety harness via `tpt-av-test-benchmark`. The sibling crates are
path dependencies; see `docs/integrations.md`.
