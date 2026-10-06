# tests/latency

Fixture/harness area required by the spec's repository layout (spec 4).

The latency/frame-timing gates themselves live in
`crates/tpt-app-live-production-test/tests/latency.rs` and run as
pass/fail CI regression tests (strict thresholds in release; see
`.github/workflows/ci.yml`).
