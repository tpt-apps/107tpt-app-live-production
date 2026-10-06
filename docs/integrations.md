# Integrations

## TPT AV foundation crates (spec 5.1)

The foundation crates are sibling checkouts referenced as **path
dependencies** (see `[workspace.dependencies]` in the root `Cargo.toml`).
Currently integrated:

| Foundation | Used by | Feature |
|---|---|---|
| `tpt-av-control-dmx` | `lighting` (sACN E1.31 sink, Art-Net capable) | `sacn` (default) |
| `tpt-av-control-osc` | `surfaces` (OSC control-surface input, sync UDP server on a dedicated thread) | `osc` (default) |
| `tpt-av-test-benchmark` | `test` (tracking global allocator for RT-safety gates) | dev-deps |

Planned, per spec 5.1:

- `tpt-kinetix` — playback-asset decode + GPU compositing backend
  consuming the switcher's `SwitchFrame` plans,
- `tpt-cadence` / `tpt-av-audio-core` — file-based audio decode for
  playback assets and the WASAPI output backend,
- `tpt-av-sync` — cross-output drift measurement/correction once real
  device clocks exist,
- `tpt-av-asset` — playback asset management/proxies.

> Note: the spec's `tpt-dsp` does not exist in the foundation tree; DSP
> lives in `tpt-av-audio-core` (the `tpt-audio` workspace). References
> to `tpt-dsp` in `spec.txt`/`todo.md` map there.

## TPT AV Commissioning (spec 13)

Optional import of a commissioning report's device/display/audio
inventory as the starting source/output/fixture list. Live Production
must be fully configurable without it — the import is a convenience
that degrades gracefully when Commissioning is not installed (UI work,
Phase 9).

## TPT AV Automation (spec 12)

Live Production may invoke an *armed* AV Automation rule pack for
background, condition-driven behaviour (e.g. automatic failover when a
camera drops). No rule-authoring UI lives here — that is AV Automation's
domain. Integration point: the engine's event bus exposes show state
(current cue, active sources) that Automation can treat as trigger
input.
