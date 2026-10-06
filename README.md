# TPT Live Production

One application to orchestrate video, audio, and lighting for live
productions — **switch / mix / cue / run the show live.**

TPT Live Production is a native, real-time operator console: a single
cue stack drives video transitions, audio changes, and lighting scenes
together, frame-accurately, with explicit rehearsal/live isolation and
built-in failsafe behaviour. Offline-first; no cloud on the signal path.

Part of the [TPT Apps](https://opensource.tptsolutions.co.nz/) family.

## Status

Work in progress toward the MVP defined in `spec.txt`. The engine core,
show-file format, CLI, and test gates are implemented and gated by 150+
tests; the desktop console is scaffolded. See `todo.md` for the running
task list.

## Workspace

| Crate | Purpose |
|---|---|
| `tpt-app-live-production-model` | Domain model, `.tptshow` file format, validation |
| `tpt-app-live-production-switcher` | PVW/PGM switching, frame-accurate cut/fade/wipe |
| `tpt-app-live-production-mixer` | Real-time channel strips, buses, ramped changes, metering |
| `tpt-app-live-production-lighting` | DMX scene recall with fade timing (sACN/Art-Net) |
| `tpt-app-live-production-cues` | Cue stack: manual / timed / follow advance |
| `tpt-app-live-production-surfaces` | OSC/MIDI/virtual control surfaces, mapping, feedback |
| `tpt-app-live-production-core` | Unified engine: mode gate, failsafe, watchdog, API |
| `tpt-app-live-production-cli` | `tpt-live-production` binary |
| `tpt-app-live-production-tauri` | Desktop console (Tauri 2) — excluded from default build |
| `tpt-app-live-production-test` | Golden / chaos / latency / fuzz / RT-safety suites |

## CLI

```console
$ tpt-live-production validate --show shows/examples/golden-demo.tptshow
$ tpt-live-production rehearse --show shows/examples/golden-demo.tptshow --headless --json
```

Machine-readable result (`--json`):

```json
{ "show": "sunday-service", "cues": 24, "issues": 0 }
```

**Exit codes (stable contract):** `0` success · `1` warnings ·
`2` validation failed · `3` configuration error · `4` input error ·
`5` internal error.

## Building

Rust 1.82+ (Windows first, Linux supported). The engine crates depend on
sibling TPT AV foundation checkouts (`tpt-av-control`, `tpt-av-test`) —
see `docs/integrations.md` and `[workspace.dependencies]`.

```console
cargo test                          # engine + CLI test suite
cargo test -p tpt-app-live-production-test   # golden/chaos/latency/fuzz/RT gates
cargo test --release                # strict latency gates
cargo build -p tpt-app-live-production-tauri # desktop shell (needs the tauri CLI)
```

## Documentation

- [`docs/architecture.md`](docs/architecture.md) — crates, decisions
- [`docs/show-model.md`](docs/show-model.md) ·
  [`docs/cue-model.md`](docs/cue-model.md) — the domain
- [`docs/switcher.md`](docs/switcher.md) ·
  [`docs/mixer.md`](docs/mixer.md) ·
  [`docs/lighting.md`](docs/lighting.md) — the engines
- [`docs/reliability.md`](docs/reliability.md) — failsafe, watchdog, degradation
- [`docs/integrations.md`](docs/integrations.md) — TPT AV foundation

## License

Dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. Copyright TPT Solutions.
