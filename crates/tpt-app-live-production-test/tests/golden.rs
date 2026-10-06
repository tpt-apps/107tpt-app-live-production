//! Golden cue-stack regression tests (spec 21.2).
//!
//! `shows/examples/golden-demo.tptshow` runs headlessly and the full
//! result is compared against `tests/fixtures/golden/golden-demo.expected.json`.
//! Engine changes that silently alter existing show behaviour fail here.

use tpt_app_live_production_core::headless::{rehearse_headless, RehearsalOptions};
use tpt_app_live_production_test::{load_show, repo_root};

#[test]
fn golden_demo_matches_expected_end_to_end() {
    let expected_path = repo_root()
        .join("tests/fixtures/golden/golden-demo.expected.json")
        .to_string_lossy()
        .to_string();
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("cannot read golden fixture {expected_path}: {e}"));
    let expected: serde_json::Value = serde_json::from_str(&expected).expect("golden fixture JSON");

    let show = load_show("shows/examples/golden-demo.tptshow");
    let actual = rehearse_headless(show, RehearsalOptions::default()).expect("rehearsal runs");
    let actual = serde_json::to_value(&actual).expect("serializable result");

    // Full-document comparison with a precise, diff-able error.
    if actual != expected {
        let actual_pretty = serde_json::to_string_pretty(&actual).unwrap();
        panic!(
            "golden mismatch!\n--- expected ---\n{}\n--- actual ---\n{}\nIf this change is \
             intentional, regenerate the fixture with:\n  cargo run -p tpt-app-live-production-cli \
             -- rehearse --show shows/examples/golden-demo.tptshow --json > \
             tests/fixtures/golden/golden-demo.expected.json",
            serde_json::to_string_pretty(&expected).unwrap(),
            actual_pretty
        );
    }
}

#[test]
fn golden_demo_cue_semantics_hold_individually() {
    // Guard the specific load-bearing behaviours even if the full JSON is
    // ever regenerated carelessly.
    let show = load_show("shows/examples/golden-demo.tptshow");
    let result = rehearse_headless(show, RehearsalOptions::default()).expect("runs");

    assert!(result.completed);
    let numbers: Vec<u32> = result.fired.iter().map(|f| f.number).collect();
    assert_eq!(numbers, vec![1, 2, 3, 4]);

    // Cue 1 cuts to cam1; cue 2 fades to cam2; cue 3 cuts to backup;
    // cue 4 fades back to cam1.
    assert_eq!(result.fired[0].program_after, "cam1");
    assert_eq!(result.fired[1].program_after, "cam2");
    assert_eq!(result.fired[2].program_after, "backup");
    assert_eq!(result.fired[3].program_after, "cam1");

    // Cue 2 timed-advances to cue 3 after 2000 ms (recorded times can lag
    // by one 16 ms tick).
    let delta = result.fired[2].fired_at_ms - result.fired[1].fired_at_ms;
    assert!(
        delta >= 2000 - 16,
        "timed advance fired after only {delta} ms"
    );

    // Final lighting is the open look (universe 1: wash1 intensity/red,
    // wash2 intensity).
    assert_eq!(
        result.lighting_end,
        vec![(1, 1, 255), (1, 2, 128), (1, 3, 200)]
    );
}

#[test]
fn golden_demo_rehearsal_is_deterministic() {
    // Two runs produce byte-identical results — the simulated clock makes
    // rehearsal repeatable (a property golden fixtures rely on).
    let run = || {
        let show = load_show("shows/examples/golden-demo.tptshow");
        let result = rehearse_headless(show, RehearsalOptions::default()).expect("runs");
        serde_json::to_string(&result).unwrap()
    };
    assert_eq!(run(), run());
}
