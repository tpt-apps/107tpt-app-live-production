# TPT Live Production — Engineering Todo

Source of truth: `spec.txt`. This file tracks engineering/implementation
tasks only. Commercial/business tasks (pricing, packaging tiers, beta
recruitment, licensing decisions) live in `commercial.md`.

Foundation crates (`tpt-kinetix`, `tpt-cadence`, `tpt-audio`, `tpt-dsp`,
`tpt-av-control`, `tpt-av-sync`, `tpt-av-asset`, `tpt-av-test`) already exist
elsewhere — tasks below integrate them, they do not build them from scratch.

---

## Phase 0 — Repository & Workspace Setup

- [ ] Initialize git repository
- [ ] Create Cargo workspace (`Cargo.toml`) per proposed layout (spec 4):
  - [ ] `crates/tpt-app-live-production-core`
  - [ ] `crates/tpt-app-live-production-model`
  - [ ] `crates/tpt-app-live-production-switcher`
  - [ ] `crates/tpt-app-live-production-mixer`
  - [ ] `crates/tpt-app-live-production-lighting`
  - [ ] `crates/tpt-app-live-production-cues`
  - [ ] `crates/tpt-app-live-production-surfaces`
  - [ ] `crates/tpt-app-live-production-cli`
  - [ ] `crates/tpt-app-live-production-tauri`
  - [ ] `crates/tpt-app-live-production-test`
- [ ] Create `shows/examples/` and `shows/templates/` directories
- [ ] Create `tests/{fixtures,latency,integration,chaos}/` directories
- [ ] Add dual-license `LICENSE` files (MIT OR Apache-2.0), copyright TPT
      Solutions (license text/holder decision tracked in `commercial.md`)
- [ ] Scaffold `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`
- [ ] Scaffold `docs/` stubs: `architecture.md`, `show-model.md`,
      `cue-model.md`, `switcher.md`, `mixer.md`, `lighting.md`,
      `reliability.md`, `integrations.md`
- [ ] Confirm/add workspace dependencies on `tpt-kinetix`, `tpt-cadence`,
      `tpt-audio`, `tpt-dsp`, `tpt-av-control`, `tpt-av-sync`,
      `tpt-av-asset`, `tpt-av-test`
- [ ] Implement core domain model structs/enums (spec 6): `Show`,
      `OperationMode`, `Source`/`SourceKind`, `Output`/`OutputKind`,
      `AudioBus`, `Fixture`, `LightingScene`, `Cue`/`CueStack`,
      `AdvanceMode`, `Transition`

## Phase 1 — Video Switching Core (spec 7)

- [ ] Integrate `tpt-kinetix` for decode + GPU compositing
- [ ] Implement PVW/PGM bus model
- [ ] Enforce: selecting a new PVW source never affects PGM output
- [ ] Implement Cut transition
- [ ] Implement Fade transition (duration-based)
- [ ] Implement basic single-overlay-layer compositing via `tpt-kinetix`
- [ ] Ensure switch timing is frame-accurate against program output frame rate
- [ ] Unit tests: valid/invalid/boundary/malformed input + expected result
      for switcher transitions

## Phase 2 — Audio Mixing Core (spec 8)

- [ ] Integrate `tpt-cadence`/`tpt-audio`/`tpt-dsp`
- [ ] Implement channel strips: per-channel gain, mute, pan
- [ ] Implement bus routing: program bus, one aux/monitor bus
- [ ] Implement bus-level metering
- [ ] Implement cue-triggered gain/mute changes
- [ ] Ensure gain/mute changes are ramped (no click/pop) unless operator
      explicitly requests instant cut
- [ ] Verify no unbounded allocation / no blocking I/O on audio render path
- [ ] Unit tests: valid/invalid/boundary/malformed input + expected result
      for mixer routing

## Phase 3 — Lighting Cue Engine (spec 9)

- [ ] Integrate `tpt-av-control` for DMX/Art-Net/sACN output
- [ ] Implement scene recall with configurable fade time
- [ ] Allow a lighting scene to attach to a cue alongside video/audio changes
- [ ] Unit tests: valid/invalid/boundary/malformed input + expected result
      for lighting cue recall
- [ ] Confirm complex lighting effects/chases are explicitly out of scope
      (deferred — Phase 13)

## Phase 4 — Cue Stack & Timeline (spec 10)

- [ ] Implement `Cue` model: video transition + audio changes + lighting
      scene executed together
- [ ] Implement manual "GO" advance
- [ ] Implement timed advance (`AdvanceMode::Timed`)
- [ ] Implement follow advance (`AdvanceMode::Follow`)
- [ ] Support reordering/inserting/editing a cue without disrupting a
      currently-live cue
- [ ] Unit tests: valid/invalid/boundary/malformed input + expected result
      for cue-stack advance logic

## Phase 5 — Rehearsal vs Live Isolation (spec 3.3, 6.1)

- [ ] Implement `OperationMode::Rehearsal` / `OperationMode::Live`
- [ ] Enforce: Rehearsal mode sends zero signal to program output, lighting
      fixtures, or any audience-facing output
- [ ] Verify a cue stack can be built/rehearsed in Rehearsal mode with zero
      effect on live outputs
- [ ] Design note carried into Phase 9: mode indicator must use colour +
      text together (not colour alone)

## Phase 6 — Control Surfaces (spec 11)

- [ ] Integrate `tpt-av-control` for OSC/MIDI control-surface input
- [ ] Implement configurable mapping: physical/virtual input → application
      function (switcher/mixer/cue-stack)
- [ ] Implement visual feedback to surface (e.g. LED reflecting current
      PGM source) where supported
- [ ] Implement graceful handling of control-surface disconnect mid-show
      (continue via on-screen controls)

## Phase 7 — Reliability & Failsafe (spec 14)

- [ ] Implement input-loss failsafe policy: freeze last good frame
- [ ] Implement input-loss failsafe policy: cut to designated backup source
- [ ] Implement input-loss failsafe policy: "signal lost" slate
- [ ] Implement audio mute-on-error default (no corrupted/glitched audio to
      program output)
- [ ] Implement watchdog supervision of the core engine process
- [ ] Implement crash detection + recovery without manual restart mid-show
- [ ] Implement graceful degradation ordering under CPU/GPU pressure:
      1. degrade preview-monitor quality
      2. degrade non-program compositing effects
      3. program output degrades last

## Phase 8 — Persistence & CLI (spec 16, 18)

- [ ] Design versioned, human-readable show-file format (schema_version,
      sources, outputs, buses, fixtures, cue stack)
- [ ] Implement show-file save
- [ ] Implement show-file load
- [ ] Implement embedded store (SQLite or similar) for session history/logs
      (start/stop times, cue-advance timestamps, failsafe events)
- [ ] Implement CLI `validate --show <file>`
- [ ] Implement CLI `rehearse --show <file> --headless`
- [ ] Implement machine-readable JSON result output (show, cues, issues)
- [ ] Implement stable exit-code contract:
      0 SUCCESS / 1 WARNINGS / 2 VALIDATION_FAILED / 3 CONFIGURATION_ERROR /
      4 INPUT_ERROR / 5 INTERNAL_ERROR

## Phase 9 — Desktop UI (Tauri) (spec 15)

- [ ] Set up native Tauri app shell with a real-time rendering surface for
      PVW/PGM monitors, distinct from general app chrome
- [ ] Build Live Operation Console: PVW/PGM monitors, CUT/FADE/WIPE
      controls, audio channel meters, cue stack view, GO control
- [ ] Implement unmistakable MODE indicator (Rehearsal/Live) using colour +
      text, visible at all times
- [ ] Build Rehearsal/Programming Mode screen (same layout, clearly marked
      Rehearsal, outputs disconnected from live displays/PA/lighting)
- [ ] Build Source/Output/Bus/Fixture configuration screen
- [ ] Implement optional import from a TPT AV Commissioning report
      (device/display/audio inventory) — must degrade gracefully if
      Commissioning is not installed
- [ ] Build Control Surface Mapping UI
- [ ] Build Show Browser: manage saved shows, duplicate as template, view
      session history/logs

## Phase 10 — Local API (spec 17)

- [ ] Implement optional localhost-only API (bind 127.0.0.1 by default,
      never external by default)
- [ ] Implement endpoints: `GET /show/state`, `POST /show/cue/next`,
      `POST /show/cue/:id/go`, `GET /health`, `WS /events`
- [ ] Ensure API is disabled by default unless explicitly enabled
- [ ] Implement token-based auth when API is enabled

## Phase 11 — Testing & CI Gates (spec 21)

- [ ] Ensure every engine component has unit tests covering valid, invalid,
      boundary, and malformed input cases (switcher, mixer, lighting,
      cue-stack advance)
- [ ] Build golden cue-stack test fixtures (example show files + expected
      switcher/mixer/lighting state after each cue)
- [ ] Build latency/frame-timing benchmark suite (switch latency, audio
      latency, frame stability under load)
- [ ] Wire latency/frame-timing benchmarks into CI as pass/fail regression
      gates
- [ ] Build chaos tests: input signal loss mid-cue
- [ ] Build chaos tests: control-surface disconnect mid-show
- [ ] Build chaos tests: engine process killed + restarted via watchdog
- [ ] Build chaos tests: CPU/GPU pressure triggering graceful degradation
- [ ] Build chaos tests: malformed show-file input
- [ ] Fuzz the show-file parser
- [ ] Fuzz inbound control-surface message parsing
- [ ] Fuzz local API request handling
- [ ] Reuse `tpt-av-test` fixtures/harnesses where possible
- [ ] Establish policy: every production bug produces a permanent
      regression fixture

## Phase 12 — Hardening & Release

- [ ] Harden error handling and failure isolation across engine/UI/CLI/API
- [ ] Verify malformed show file or unexpected input signal cannot crash
      the application mid-show
- [ ] Package Windows release
- [ ] Validate installation on representative production hardware (clean
      machine, no dev tooling)
- [ ] Benchmark performance under realistic multi-source, multi-output load
- [ ] Verify no internet connection required for core live operation
- [ ] Run through full Definition of Done checklist (spec 27) end-to-end
      before calling MVP complete
- [ ] Run private beta with a real live-event production team (coordination
      tracked in `commercial.md`; engineering support/bugfix loop tracked
      here)

## Phase 13 — Post-MVP (Phase 2 / Phase 3 — not scheduled yet)

Phase 2:
- [ ] Richer lighting effects/chases beyond scene recall
- [ ] Deeper AV Automation integration for background failover rules
- [ ] Multi-operator/networked control (separate audio/video operators)
- [ ] Additional transition types (wipe patterns, stingers)

Phase 3:
- [ ] Built-in streaming/encoding output management
- [ ] Multi-site/remote production (contribution feeds from remote locations)
- [ ] Plugin SDK for custom source/output device drivers

Explicitly out of scope unless/until revisited: full lighting console
replacement, full NLE, built-in streaming-encoder product as MVP feature,
rule-automation authoring UI (belongs to TPT AV Automation), pre-show
installation testing (belongs to TPT AV Commissioning), Broadcast-tier
redundancy features without customer evidence.
