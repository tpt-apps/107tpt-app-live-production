# Changelog

All notable changes to TPT Live Production are documented here.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Cargo workspace with the ten crates from the spec's proposed layout
  (engine core + shells split, spec 4).
- Domain model crate: show/sources/outputs/buses/fixtures/scenes/cue
  stack (spec 6), versioned TOML show-file format `.tptshow` schema 1
  (spec 18), and shared validation with machine-readable issue codes
  (spec 16).
- PVW/PGM switcher: frame-accurate cut/fade/wipe against the program
  frame rate, single overlay layer, last-good-frame tracking for the
  failsafe freeze policy (spec 7).
- Real-time audio mixer: channel strips, program/aux buses, ramped
  (click-free) gain/mute/pan, bus metering, allocation-free render path
  (spec 8, 3.1).
- Lighting cue engine: DMX scene recall with configurable fade,
  mid-fade re-recall from current blended state, sACN output sink via
  `tpt-av-control-dmx` (spec 9).
- Cue stack runner: manual GO, timed and follow advance, live-safe
  editing (insert/remove/replace/move never disrupts the live cue)
  (spec 10).
- Engine core: unified cue executor, structural rehearsal/live output
  gate (spec 3.3), input-loss failsafe policies + audio mute-on-error
  (spec 14.1), watchdog supervision with recovery (spec 14.2),
  graceful degradation ordering (spec 14.3), event bus, JSONL session
  log (spec 18), headless rehearsal driver (spec 16).
- Control surfaces: protocol-agnostic mapping table, feedback
  indicators, graceful mid-show disconnect; OSC transport via
  `tpt-av-control-osc` (spec 11).
- Optional localhost API (feature `api`, disabled by default,
  loopback-only, token auth): `/health`, `/show/state`,
  `/show/cue/next`, `/show/cue/:id/go`, WS `/events` (spec 17).
- CLI `validate` and `rehearse --headless` with the stable exit-code
  contract 0-5 (spec 16).
- Test suites: golden cue-stack fixture, chaos tests (signal loss,
  surface disconnect, watchdog restart, pressure, malformed files),
  latency/frame-timing CI gates, deterministic fuzz corpora
  (show-file/OSC/API), allocation-free render proofs via
  `tpt-av-test-benchmark` (spec 21).
- Tauri 2 desktop shell with the live-operation console UI (spec 15),
  excluded from the default build.
- Example + template shows under `shows/`.
