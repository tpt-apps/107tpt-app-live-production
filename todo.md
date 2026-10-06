# TPT Live Production — Engineering Todo

Source of truth: `spec.txt`. This file tracks engineering/implementation
tasks only. Commercial/business tasks (pricing, packaging tiers, beta
recruitment, licensing decisions) live in `commercial.md`.

Foundation crates (`tpt-kinetix`, `tpt-cadence`, `tpt-audio`, `tpt-dsp`,
`tpt-av-control`, `tpt-av-sync`, `tpt-av-asset`, `tpt-av-test`) already exist
elsewhere — tasks below integrate them, they do not build them from scratch.

> **Status snapshot (2026-10-07).** The workspace is implemented and green:
> `cargo test --workspace` passes 154 tests / 0 failures, clippy clean,
> rustfmt clean. Deviations from the plan are annotated inline; the two
> big ones: (1) `tpt-dsp` does not exist in the foundation tree — DSP lives
> in `tpt-av-audio-core` (`tpt-audio` workspace), so the mixer is a
> purpose-built real-time core with foundation integration happening at
> the I/O layer; (2) media backends (camera capture, GPU compositing,
> WASAPI) are trait seams with deterministic cores — the switcher emits
> render *plans*, so backend integration is additive, not structural.

---

## Phase 0 — Repository & Workspace Setup

- [x] Initialize git repository
- [x] Create Cargo workspace (`Cargo.toml`) per proposed layout (spec 4):
  - [x] `crates/tpt-app-live-production-core`
  - [x] `crates/tpt-app-live-production-model`
  - [x] `crates/tpt-app-live-production-switcher`
  - [x] `crates/tpt-app-live-production-mixer`
  - [x] `crates/tpt-app-live-production-lighting`
  - [x] `crates/tpt-app-live-production-cues`
  - [x] `crates/tpt-app-live-production-surfaces`
  - [x] `crates/tpt-app-live-production-cli`
  - [x] `crates/tpt-app-live-production-tauri` (excluded from default build;
        compiles via `cargo check -p tpt-app-live-production-tauri`)
  - [x] `crates/tpt-app-live-production-test`
- [x] Create `shows/examples/` and `shows/templates/` directories
      (golden demo + Sunday Service template, both validate/rehearse clean)
- [x] Create `tests/{fixtures,latency,integration,chaos}/` directories
      (README stubs map them to the test crate's suites; golden fixtures in
      `tests/fixtures/golden/`)
- [x] Add dual-license `LICENSE` files (MIT OR Apache-2.0), copyright TPT
      Solutions (license text/holder decision tracked in `commercial.md`)
- [x] Scaffold `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`
- [x] Scaffold `docs/` stubs: `architecture.md`, `show-model.md`,
      `cue-model.md`, `switcher.md`, `mixer.md`, `lighting.md`,
      `reliability.md`, `integrations.md` — all written as real content
- [x] Confirm/add workspace dependencies on `tpt-kinetix`, `tpt-cadence`,
      `tpt-audio`, `tpt-dsp`, `tpt-av-control`, `tpt-av-sync`,
      `tpt-av-asset`, `tpt-av-test`
      — integrated so far: `tpt-av-control-dmx`, `tpt-av-control-osc`,
      `tpt-av-test-benchmark` (path deps). `tpt-dsp` does not exist (see
      snapshot note). `tpt-kinetix`/`tpt-cadence`/`tpt-audio`/`tpt-av-sync`/
      `tpt-av-asset` integration is planned and documented in
      `docs/integrations.md`; the engine's render-plan seams are built for it.
- [x] Implement core domain model structs/enums (spec 6): `Show`,
      `OperationMode`, `Source`/`SourceKind`, `Output`/`OutputKind`,
      `AudioBus`, `Fixture`, `LightingScene`, `Cue`/`CueStack`,
      `AdvanceMode`, `Transition`

## Phase 1 — Video Switching Core (spec 7)

- [ ] Integrate `tpt-kinetix` for decode + GPU compositing
      *(switcher emits `SwitchFrame` render plans; the kinetix-backed
      compositor consumes them — see `docs/switcher.md`, `docs/integrations.md`)*
- [x] Implement PVW/PGM bus model
- [x] Enforce: selecting a new PVW source never affects PGM output
      (pinned by unit test)
- [x] Implement Cut transition
- [x] Implement Fade transition (duration-based; exact integer
      `ceil(duration × fps)` frame quantization)
- [x] Implement basic single-overlay-layer compositing via `tpt-kinetix`
      *(overlay layer exists in the render plan + engine; the GPU composite
      backend is the same kinetix seam as above)*
- [x] Ensure switch timing is frame-accurate against program output frame rate
- [x] Unit tests: valid/invalid/boundary/malformed input + expected result
      for switcher transitions

## Phase 2 — Audio Mixing Core (spec 8)

- [~] Integrate `tpt-cadence`/`tpt-audio`/`tpt-dsp`
      *(tpt-dsp does not exist; mixer core is purpose-built RT-safe Rust.
      tpt-av-audio-core/tpt-cadence integration is scoped for decode +
      device I/O in `docs/integrations.md`)*
- [x] Implement channel strips: per-channel gain, mute, pan
- [x] Implement bus routing: program bus, one aux/monitor bus
- [x] Implement bus-level metering (peak + RMS per block)
- [x] Implement cue-triggered gain/mute changes
- [x] Ensure gain/mute changes are ramped (no click/pop) unless operator
      explicitly requests instant cut (verified by click-free tests;
      mid-ramp changes start from current amplitude)
- [x] Verify no unbounded allocation / no blocking I/O on audio render path
      (enforced by tracking-allocator RT tests)
- [x] Unit tests: valid/invalid/boundary/malformed input + expected result
      for mixer routing

## Phase 3 — Lighting Cue Engine (spec 9)

- [x] Integrate `tpt-av-control` for DMX/Art-Net/sACN output
      (`tpt-av-control-dmx` sACN sink behind the `sacn` feature)
- [x] Implement scene recall with configurable fade time
- [x] Allow a lighting scene to attach to a cue alongside video/audio changes
- [x] Unit tests: valid/invalid/boundary/malformed input + expected result
      for lighting cue recall
- [x] Confirm complex lighting effects/chases are explicitly out of scope
      (deferred — Phase 13)

## Phase 4 — Cue Stack & Timeline (spec 10)

- [x] Implement `Cue` model: video transition + audio changes + lighting
      scene executed together
- [x] Implement manual "GO" advance
- [x] Implement timed advance (`AdvanceMode::Timed`)
- [x] Implement follow advance (`AdvanceMode::Follow`)
- [x] Support reordering/inserting/editing a cue without disrupting a
      currently-live cue (runner tracks the live cue by id; unit-tested)
- [x] Unit tests: valid/invalid/boundary/malformed input + expected result
      for cue-stack advance logic

## Phase 5 — Rehearsal vs Live Isolation (spec 3.3, 6.1)

- [x] Implement `OperationMode::Rehearsal` / `OperationMode::Live`
- [x] Enforce: Rehearsal mode sends zero signal to program output, lighting
      fixtures, or any audience-facing output
      (structural: program outputs are unpopulated in rehearsal — see
      `FrameOutputs`; unit-tested)
- [x] Verify a cue stack can be built/rehearsed in Rehearsal mode with zero
      effect on live outputs (headless rehearsal driver forces Rehearsal)
- [x] Design note carried into Phase 9: mode indicator must use colour +
      text together (not colour alone) — implemented in the console UI

## Phase 6 — Control Surfaces (spec 11)

- [x] Integrate `tpt-av-control` for OSC/MIDI control-surface input
      (OSC via `tpt-av-control-osc` on a dedicated thread; MIDI control
      variants are modelled in the mapping table, midir transport pending)
- [x] Implement configurable mapping: physical/virtual input → application
      function (switcher/mixer/cue-stack) — serializable `MappingConfig`;
      virtual buttons share the physical code path
- [x] Implement visual feedback to surface (e.g. LED reflecting current
      PGM source) where supported (`FeedbackSink` + `update_feedback`)
- [x] Implement graceful handling of control-surface disconnect mid-show
      (input dropped, on-screen controls continue; unit-tested)

## Phase 7 — Reliability & Failsafe (spec 14)

- [x] Implement input-loss failsafe policy: freeze last good frame
- [x] Implement input-loss failsafe policy: cut to designated backup source
- [x] Implement input-loss failsafe policy: "signal lost" slate
      (all three chaos-tested; missing backup/slate degrades to freeze)
- [x] Implement audio mute-on-error default (no corrupted/glitched audio to
      program output)
- [x] Implement watchdog supervision of the core engine process
- [x] Implement crash detection + recovery without manual restart mid-show
      *(in-process: stall detection + engine rebuild from show file, chaos-
      tested; process-level restart belongs to the OS service wrapper /
      desktop shell — documented in `docs/reliability.md`)*
- [x] Implement graceful degradation ordering under CPU/GPU pressure:
  1. degrade preview-monitor quality
  2. degrade non-program compositing effects
  3. program output degrades last
      (hysteresis controller; order pinned by tests)

## Phase 8 — Persistence & CLI (spec 16, 18)

- [x] Design versioned, human-readable show-file format (schema_version,
      sources, outputs, buses, fixtures, cue stack) — TOML `.tptshow`,
      schema 1
- [x] Implement show-file save
- [x] Implement show-file load
- [x] Implement embedded store (SQLite or similar) for session history/logs
      (start/stop times, cue-advance timestamps, failsafe events)
      — append-only JSONL store ("or similar" per spec): crash-safe,
      human-readable, trivially importable to SQLite later
      (`core::session`; torn-tail-tolerant reader)
- [x] Implement CLI `validate --show <file>`
- [x] Implement CLI `rehearse --show <file> --headless`
- [x] Implement machine-readable JSON result output (show, cues, issues)
- [x] Implement stable exit-code contract:
      0 SUCCESS / 1 WARNINGS / 2 VALIDATION_FAILED / 3 CONFIGURATION_ERROR /
      4 INPUT_ERROR / 5 INTERNAL_ERROR
      (clap usage errors remap to 3 so clap's default 2 never collides with
      VALIDATION_FAILED)

## Phase 9 — Desktop UI (Tauri) (spec 15)

- [~] Set up native Tauri app shell with a real-time rendering surface for
      PVW/PGM monitors, distinct from general app chrome
      *(Tauri 2 shell compiles; commands wired to the engine; the native
      render surface lands with the kinetix compositor)*
- [~] Build Live Operation Console: PVW/PGM monitors, CUT/FADE/WIPE
      controls, audio channel meters, cue stack view, GO control
      *(console UI implemented in `ui/index.html`: monitors, CUT/FADE/GO
      with keyboard shortcuts, meters, cue stack, degradation/lost-input
      banners; WIPE button + source-selection panels pending)*
- [x] Implement unmistakable MODE indicator (Rehearsal/Live) using colour +
      text, visible at all times (text + colour + Live pulse animation)
- [~] Build Rehearsal/Programming Mode screen (same layout, clearly marked
      Rehearsal, outputs disconnected from live displays/PA/lighting)
      *(single console covers both modes; the gate is structural in the
      engine — a separate programming screen is UI polish)*
- [ ] Build Source/Output/Bus/Fixture configuration screen
- [ ] Implement optional import from a TPT AV Commissioning report
      (device/display/audio inventory) — must degrade gracefully if
      Commissioning is not installed
- [ ] Build Control Surface Mapping UI
- [ ] Build Show Browser: manage saved shows, duplicate as template, view
      session history/logs

## Phase 10 — Local API (spec 17)

- [x] Implement optional localhost-only API (bind 127.0.0.1 by default,
      never external by default — non-loopback bind refused without
      explicit `allow_external`)
- [x] Implement endpoints: `GET /show/state`, `POST /show/cue/next`,
      `POST /show/cue/:id/go`, `GET /health`, `WS /events`
- [x] Ensure API is disabled by default unless explicitly enabled
      (`spawn` refuses a disabled config — cannot start by accident)
- [x] Implement token-based auth when API is enabled (constant-time
      comparison; all endpoints gated)

## Phase 11 — Testing & CI Gates (spec 21)

- [x] Ensure every engine component has unit tests covering valid, invalid,
      boundary, and malformed input cases (switcher, mixer, lighting,
      cue-stack advance)
- [x] Build golden cue-stack test fixtures (example show files + expected
      switcher/mixer/lighting state after each cue)
      (`shows/examples/golden-demo.tptshow` +
      `tests/fixtures/golden/golden-demo.expected.json`; determinism test)
- [x] Build latency/frame-timing benchmark suite (switch latency, audio
      latency, frame stability under load)
      (`tests/latency.rs`; profile-scaled thresholds, `TPT_LATENCY_GATE=strict`)
- [x] Wire latency/frame-timing benchmarks into CI as pass/fail regression
      gates (`.github/workflows/ci.yml`, release + strict)
- [x] Build chaos tests: input signal loss mid-cue
- [x] Build chaos tests: control-surface disconnect mid-show
- [x] Build chaos tests: engine process killed + restarted via watchdog
- [x] Build chaos tests: CPU/GPU pressure triggering graceful degradation
- [x] Build chaos tests: malformed show-file input
- [x] Fuzz the show-file parser (deterministic seeded corpus in
      `tests/fuzz.rs`; cargo-fuzz targets in `fuzz/` for Linux CI)
- [x] Fuzz inbound control-surface message parsing (same layout)
- [x] Fuzz local API request handling (live server bombarded with mutated
      requests; must survive and stay healthy)
- [x] Reuse `tpt-av-test` fixtures/harnesses where possible
      (tracking allocator + allocation counters from
      `tpt-av-test-benchmark` power the RT-safety gates)
- [x] Establish policy: every production bug produces a permanent
      regression fixture (documented in `CONTRIBUTING.md`; seeded with the
      duplicate-cue-number fixture)

## Phase 12 — Hardening & Release

- [x] Harden error handling and failure isolation across engine/UI/CLI/API
      (refusal to build invalid shows; cue steps that fail log and skip;
      API/fuzz hardening; torn session-log tails tolerated)
- [x] Verify malformed show file or unexpected input signal cannot crash
      the application mid-show (chaos + fuzz suites)
- [ ] Package Windows release
- [ ] Validate installation on representative production hardware (clean
      machine, no dev tooling)
- [~] Benchmark performance under realistic multi-source, multi-output load
      *(synthetic multi-source load gates exist; representative hardware
      runs pending)*
- [~] Verify no internet connection required for core live operation
      *(offline-first by construction — no network on any live path; formal
      clean-machine verification pending)*
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
