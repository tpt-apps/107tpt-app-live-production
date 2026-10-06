# Contributing to TPT Live Production

## Ground rules

1. **Real-time safety is non-negotiable** (spec 3.1): no allocation and
   no blocking I/O on the audio/video render paths during live
   operation. `tests/rt_allocation.rs` enforces this — keep it green.
2. **Rehearsal must never touch live outputs** (spec 3.3). New output
   paths must go through the engine's mode gate, not around it.
3. **Every production bug becomes a permanent regression fixture**
   (spec 21.6): add the failing case to `tests/` (golden, chaos, or a
   focused unit test) before fixing.
4. **The CLI exit-code contract is stable** (spec 16): 0-5 as
   documented in the README. Never repurpose a code.
5. **Respect the product boundary** (spec 26): this is the operator's
   real-time console — not a lighting console, not an NLE, not a
   streaming encoder, not a rule editor.

## Development

```console
cargo test                    # full suite (debug; latency gates are relaxed)
cargo test --release          # strict latency gates (what CI runs)
cargo clippy --workspace --all-targets
cargo fmt --all
```

Engine crates live under `crates/`; foundation crates are sibling
checkouts (see `docs/integrations.md`). The Tauri shell is excluded
from the default build; check it explicitly with
`cargo check -p tpt-app-live-production-tauri`.

## Show files

`.tptshow` is TOML, schema version 1 (`docs/show-model.md`). Example
and template shows live in `shows/`. If you change the schema, bump
`SCHEMA_VERSION` and keep the old reader working.

## Licensing

By contributing you agree your work is dual-licensed MIT OR Apache-2.0.
