//! Latency / frame-timing regression gates (spec 19, 21.3).
//!
//! These are pass/fail CI gates, not informal measurements. Thresholds are
//! profile-scaled: debug builds get a generous multiple (they are ~10-20x
//! slower); CI must run `cargo test --release` for the strict gates.
//! `TPT_LATENCY_GATE=strict` forces the strict thresholds anywhere.

use std::time::{Duration, Instant};

use tpt_app_live_production_core::engine::{EngineConfig, LiveEngine};
use tpt_app_live_production_mixer::{Mixer, RenderInputs};
use tpt_app_live_production_model::cue::Transition;
use tpt_app_live_production_model::ids::{BusId, SourceId};
use tpt_app_live_production_model::{OperationMode, Show};
use tpt_app_live_production_switcher::Switcher;
use tpt_app_live_production_test::load_show;

fn strict() -> bool {
    std::env::var("TPT_LATENCY_GATE")
        .map(|v| v.eq_ignore_ascii_case("strict"))
        .unwrap_or(false)
}

/// Budget multiplier for the current profile (debug builds are much
/// slower; release is the real gate).
fn slack() -> u32 {
    if cfg!(debug_assertions) && !strict() {
        25
    } else {
        1
    }
}

fn live_engine() -> LiveEngine {
    let mut show: Show = load_show("shows/examples/golden-demo.tptshow");
    show.mode = OperationMode::Live;
    LiveEngine::build(show, EngineConfig::default()).expect("engine")
}

/// Gate 1: switch latency (spec 19). A CUT must land on program within one
/// frame of the operator action; engine-side the take+tick must be far
/// below a frame's duration.
#[test]
fn gate_switch_latency() {
    let mut switcher = Switcher::new(60, SourceId::new("cam1")).unwrap();
    switcher.register_source(SourceId::new("cam1"));
    switcher.register_source(SourceId::new("cam2"));

    let frame_budget = Duration::from_millis(16_666_667 / 1_000_000); // 16.6ms
    let budget = frame_budget / 10 * slack().max(1);

    let mut samples = Vec::with_capacity(2_000);
    for i in 0..2_000u64 {
        let start = Instant::now();
        switcher
            .select_preview(SourceId::new(if i % 2 == 0 { "cam2" } else { "cam1" }))
            .unwrap();
        switcher.take(&Transition::Cut, i).unwrap();
        let frame = switcher.tick(i);
        let elapsed = start.elapsed();
        assert_eq!(
            frame.program,
            SourceId::new(if i % 2 == 0 { "cam2" } else { "cam1" }),
            "cut must land on the same tick"
        );
        samples.push(elapsed);
    }
    let worst = samples.iter().max().unwrap();
    assert!(
        *worst <= budget,
        "switch path took {worst:?}, budget {budget:?} (profile slack x{})",
        slack()
    );
}

/// Gate 2: audio block rendering stays inside the block budget (spec 19,
/// 3.1 — real-time-safe render path). 128 frames @ 48 kHz = 2.67 ms.
#[test]
fn gate_audio_block_budget() {
    let mut mixer = Mixer::new(48_000, 128, Duration::from_millis(25)).unwrap();
    mixer.add_bus(BusId::new("program")).unwrap();
    mixer.add_bus(BusId::new("aux")).unwrap();
    let mic = SourceId::new("mic1");
    mixer.add_source(mic.clone()).unwrap();
    mixer.patch(&mic, &BusId::new("program")).unwrap();

    let input = vec![0.25f32; 128 * 2];
    let mut program = vec![0.0f32; 128 * 2];
    let mut aux = vec![0.0f32; 128 * 2];
    let block_budget = Duration::from_nanos(128 * 1_000_000_000u64 / 48_000);
    let budget = block_budget
        .checked_div(slack().max(1))
        .unwrap_or(block_budget);

    let mut worst = Duration::ZERO;
    for _ in 0..4_000 {
        let mut inputs = RenderInputs::new();
        inputs.push(&mic, &input);
        let start = Instant::now();
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        mixer.render_block(&inputs, &mut outs);
        let elapsed = start.elapsed();
        if elapsed > worst {
            worst = elapsed;
        }
    }
    assert!(
        worst <= budget,
        "audio block took {worst:?}, block budget {block_budget:?}, test budget {budget:?}"
    );
}

/// Gate 3: frame stability under load (spec 19): polling the full engine
/// (cues + switcher + lighting + failsafes) must fit the frame budget for
/// a continuous run, with program frames never skipped because of engine
/// cost.
#[test]
fn gate_frame_stability_under_load() {
    let mut engine = live_engine();
    engine.set_mode(OperationMode::Live);
    engine.go();

    let frame_ms = 1000 / u64::from(engine.show().settings.video_fps.max(1));
    let budget = Duration::from_millis(frame_ms) / 4 * slack().max(1);

    let mut now = 0u64;
    let mut worst = Duration::ZERO;
    let frames = 6_000u64; // ~100 s of simulated show time
    for i in 0..frames {
        // Simulated operator load: preview flips and takes throughout.
        if i % 901 == 0 {
            let _ = engine.select_preview(SourceId::new("cam2"));
        }
        if i % 1807 == 0 {
            let _ = engine.take(Transition::Cut);
        }
        let start = Instant::now();
        let _ = engine.poll(now);
        let elapsed = start.elapsed();
        if elapsed > worst {
            worst = elapsed;
        }
        now += frame_ms;
    }
    assert!(
        worst <= budget,
        "engine poll took {worst:?}, frame budget {}ms, test budget {budget:?}",
        frame_ms
    );
}

/// Gate 4: show load time (spec 19 "startup/load"): building an engine
/// from a show file must be near-instant so it never delays a show start.
#[test]
fn gate_show_load_time() {
    let budget = Duration::from_millis(50 * u64::from(slack().max(1)));
    let start = Instant::now();
    let _engine = live_engine();
    let elapsed = start.elapsed();
    assert!(
        elapsed <= budget,
        "engine build took {elapsed:?}, budget {budget:?}"
    );
}
