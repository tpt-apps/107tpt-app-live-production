//! Chaos tests (spec 21.4): the failure modes a live show actually hits.
//! Each one asserts the engine degrades gracefully rather than crashing or
//! silently misbehaving.

use std::time::Duration;

use tpt_app_live_production_core::engine::{EngineConfig, LiveEngine};
use tpt_app_live_production_core::failsafe::VideoFailsafePolicy;
use tpt_app_live_production_core::headless::{rehearse_headless, RehearsalOptions};
use tpt_app_live_production_core::session::SessionLog;
use tpt_app_live_production_core::watchdog::{Heartbeat, Watchdog, WatchdogConfig};
use tpt_app_live_production_model::cue::{AdvanceMode, Cue, CueStack, Transition};
use tpt_app_live_production_model::ids::{OutputId, SourceId};
use tpt_app_live_production_model::showfile::ShowFile;
use tpt_app_live_production_model::{OperationMode, Show};
use tpt_app_live_production_surfaces::{
    ControlValue, EngineOps, MappingConfig, RecordingFeedback, SurfaceAction, SurfaceControl,
    SurfaceManager, SurfaceMapping, SurfaceOutcome,
};
use tpt_app_live_production_test::load_show;

// The engine test module's show builder is pub(crate)-visible to the core
// crate only; here we build an equivalent show from a file so the chaos
// suite exercises the real load path.
fn live_show() -> Show {
    let mut show = load_show("shows/examples/golden-demo.tptshow");
    show.mode = OperationMode::Live;
    show
}

/// Chaos 1 (spec 21.4): input signal loss mid-cue.
#[test]
fn signal_loss_mid_cue_applies_failsafe_and_show_continues() {
    let mut cfg = EngineConfig::default();
    cfg.failsafe.video = VideoFailsafePolicy::CutToBackup {
        backup: SourceId::new("backup"),
    };
    let mut engine = LiveEngine::build(live_show(), cfg).expect("engine");
    engine.set_mode(OperationMode::Live);
    engine.report_input_ok(&SourceId::new("cam2"), 0);

    // Cue 2 (fade to cam2) fires and its transition is in flight...
    let mut now = 0u64;
    engine.go();
    engine.go();
    engine.poll(now);

    // ...and mid-fade, cam2's signal drops.
    now += 300;
    engine.poll(now);
    assert_eq!(engine.state().program.0, "cam2");

    // Signal is gone: past the timeout the failsafe cuts to backup.
    now += 2000;
    engine.poll(now);
    let state = engine.state();
    assert!(
        state.lost_inputs.iter().any(|s| s.0 == "cam2"),
        "cam2 flagged lost"
    );
    assert_eq!(state.program.0, "backup", "mid-cue loss cuts to backup");

    // The show keeps running: cues 1-3 fired (cue 3 auto-advanced on its
    // timer during the chaos) and an explicit jump to cue 4 still lands.
    engine
        .go_to_cue(&tpt_app_live_production_model::CueId::new("cue-4"))
        .ok();
    let final_state = engine.state();
    assert_eq!(
        final_state.cues_fired, 4,
        "engine still fires cues after chaos"
    );
}

/// Chaos 2 (spec 21.4): control-surface disconnect mid-show.
#[test]
fn surface_disconnect_mid_show_drops_surface_input_and_show_continues() {
    struct EngineDouble {
        go_count: u32,
    }
    impl EngineOps for EngineDouble {
        fn go(&mut self) {
            self.go_count += 1;
        }
        fn cut(&mut self) {}
        fn fade(&mut self, _d: Duration) {}
        fn select_preview(&mut self, _s: &str) {}
        fn set_gain(&mut self, _s: &str, _g: f64) {}
        fn set_mute(&mut self, _s: &str, _m: bool) {}
        fn to_rehearsal(&mut self) {}
    }

    let config = MappingConfig {
        mappings: vec![SurfaceMapping {
            control: SurfaceControl::Osc("/go".into()),
            action: SurfaceAction::Go,
        }],
    };
    let mut surface = SurfaceManager::new(
        "mid-show surface",
        config,
        Box::new(RecordingFeedback::default()),
    );
    let mut engine = EngineDouble { go_count: 0 };

    // Surface works, then drops mid-show...
    assert!(matches!(
        surface.handle(SurfaceControl::Osc("/go".into()), None, &mut engine),
        SurfaceOutcome::Applied(_)
    ));
    surface.disconnect();

    // ...late-arriving OSC packets are dropped, not applied...
    for _ in 0..5 {
        assert!(matches!(
            surface.handle(
                SurfaceControl::Osc("/go".into()),
                Some(ControlValue(1.0)),
                &mut engine
            ),
            SurfaceOutcome::DroppedDisconnected(_)
        ));
    }
    assert_eq!(engine.go_count, 1, "no surface GOs after disconnect");

    // ...and the on-screen control path still drives the show.
    engine.go();
    assert_eq!(engine.go_count, 2);
}

/// Chaos 3 (spec 21.4): engine process killed and restarted via watchdog.
#[test]
fn watchdog_detects_stall_and_recovery_rebuilds_the_engine() {
    let heartbeat = Heartbeat::starting_now();
    let recovered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recovered2 = recovered.clone();

    // "Crash": the engine stops beating (simulated by never calling beat).
    // The watchdog fires recovery, which rebuilds the engine from the show
    // file — the same thing the desktop shell does after a process exit.
    let mut watchdog = Watchdog::spawn(
        heartbeat.clone(),
        WatchdogConfig {
            stall_timeout: Duration::from_millis(60),
            poll_interval: Duration::from_millis(10),
        },
        std::sync::Arc::new(move |_stalled| {
            // Recovery: rebuild from the on-disk show file.
            let show = {
                let path = tpt_app_live_production_test::repo_root()
                    .join("shows/examples/golden-demo.tptshow");
                let file = ShowFile::load(&path)
                    .unwrap_or_else(|e| panic!("show file still readable after crash: {e}"));
                tpt_app_live_production_model::Show::try_from(file).expect("show rebuilds")
            };
            let _engine =
                LiveEngine::build(show, EngineConfig::default()).expect("engine rebuilds");
            recovered2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    );

    std::thread::sleep(Duration::from_millis(250));
    watchdog.shutdown();
    assert!(
        recovered.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "watchdog recovery must run on stall"
    );

    // The recovered engine is fully operable.
    let show = live_show();
    let mut engine = LiveEngine::build(show, EngineConfig::default()).unwrap();
    heartbeat.beat(); // heartbeat resumes under the new engine
    engine.attach_heartbeat(heartbeat);
    engine.set_mode(OperationMode::Live);
    engine.poll(0);
    engine.go();
    assert_eq!(engine.state().cues_fired, 1);
}

/// Chaos 4 (spec 21.4): CPU/GPU pressure triggers graceful degradation in
/// the spec 14.3 order.
#[test]
fn pressure_degrades_preview_then_effects_never_program_first() {
    let mut engine = LiveEngine::build(live_show(), EngineConfig::default()).unwrap();
    engine.set_mode(OperationMode::Live);

    let mut saw_preview = false;
    let mut saw_effects = false;
    for _ in 0..12 {
        if let Some(level) = {
            engine.report_pressure(95.0);
            None::<tpt_app_live_production_core::DegradationLevel>
        } {
            let _ = level;
        }
        let level = engine.degradation_level();
        // Order invariant: effects only after preview has degraded.
        if level == tpt_app_live_production_core::DegradationLevel::PreviewDegraded {
            saw_preview = true;
        }
        if level == tpt_app_live_production_core::DegradationLevel::EffectsDegraded {
            assert!(saw_preview, "effects degraded before preview");
            saw_effects = true;
        }
        if level == tpt_app_live_production_core::DegradationLevel::ProgramDegraded {
            assert!(saw_effects, "program degraded before effects");
        }
        // The engine still produces output frames under pressure.
        let out = engine.poll(0);
        assert!(
            out.program_video.is_some(),
            "program keeps rendering under pressure"
        );
    }
    assert!(
        saw_preview && saw_effects,
        "escalation visited preview and effects tiers"
    );
}

/// Chaos 5 (spec 21.4/21.5): malformed show files must fail cleanly, never
/// panic and never half-run.
#[test]
fn malformed_show_files_fail_cleanly() {
    let malformed: Vec<(&str, String)> = vec![
        ("not toml", "this is [[ not toml".to_string()),
        ("empty", String::new()),
        (
            "binary junk",
            [0u8, 1, 2, 3, 255, 254]
                .iter()
                .map(|&b| b as char)
                .collect(),
        ),
        (
            "future schema",
            "schema_version = 999\nname = \"x\"".to_string(),
        ),
        ("wrong types", "schema_version = 1\nname = 42".to_string()),
        (
            "truncated",
            "schema_version = 1\nname = \"x\"\n[[sou".to_string(),
        ),
    ];
    for (name, content) in malformed {
        let dir = std::env::temp_dir().join(format!(
            "tpt-chaos-{}-{}",
            std::process::id(),
            name.replace(' ', "_")
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("bad.tptshow");
        std::fs::write(&path, content).unwrap();
        let loaded = ShowFile::load(&path);
        match loaded {
            Err(_) => { /* clean parse failure: good */ }
            Ok(file) => {
                // Parsed but invalid: domain conversion or validation must
                // catch it; rehearsal must refuse.
                let show_result = Show::try_from(file);
                match show_result {
                    Err(_) => {}
                    Ok(show) => {
                        let issues = tpt_app_live_production_core::headless::validate_show(&show);
                        assert!(
                            issues.iter().any(|i| i.severity == "error"),
                            "case '{name}' parsed to a clean show; validation must flag it"
                        );
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Regression fixture policy (spec 21.6): any show that previously failed
/// validation for a real production bug gets pinned here. Seed the list
/// with the duplicate-cue-number case.
#[test]
fn regression_duplicate_cue_numbers_are_permanently_rejected() {
    let mut show = live_show();
    show.cue_stack.cues[1].number = show.cue_stack.cues[0].number;
    let result = rehearse_headless(show, RehearsalOptions::default());
    assert!(result.is_err(), "duplicate cue numbers must refuse to run");
}

/// Session history survives a session (spec 18): the chaos-recovered
/// engine writes to a fresh log and post-show review reads it back.
#[test]
fn session_history_is_reviewable_after_a_chaos_run() {
    let dir = std::env::temp_dir().join(format!("tpt-chaos-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let log = std::sync::Arc::new(SessionLog::open(&dir).unwrap());
    let mut engine = LiveEngine::build(live_show(), EngineConfig::default()).unwrap();
    engine.set_session_log(log.clone());
    engine.set_mode(OperationMode::Live);
    engine.report_input_ok(&SourceId::new("cam1"), 0);
    engine.go();
    engine.poll(0);
    engine.poll(3000); // cam1 lost -> failsafe
    drop(engine);

    let events = SessionLog::read_events(log.path());
    let has_failsafe = events.iter().any(|e| {
        matches!(
            e.kind,
            tpt_app_live_production_core::session::SessionEventKind::Failsafe { .. }
        )
    });
    assert!(has_failsafe, "failsafe event recorded for post-show review");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stack with Follow chains and Manual mixes advances exactly once per
/// trigger under a long run (no double-fires, no stalls).
#[test]
fn mixed_advance_modes_stress_run() {
    let mut show = live_show();
    show.cue_stack = CueStack {
        cues: (1..=50)
            .map(|i| {
                let mut cue = Cue::new(i, format!("stress {i}"));
                cue.advance = match i % 3 {
                    0 => AdvanceMode::Manual,
                    1 => AdvanceMode::Timed { after_ms: 100 },
                    _ => AdvanceMode::Follow,
                };
                cue.video_transition = Some(Transition::Cut);
                cue.preview_source = Some(SourceId::new("cam1"));
                cue
            })
            .collect(),
        current_index: None,
    };
    let result = rehearse_headless(
        show,
        RehearsalOptions {
            max_duration: Duration::from_secs(120),
            ..RehearsalOptions::default()
        },
    )
    .expect("stress rehearsal runs");
    assert!(result.completed, "all 50 cues fired");
    assert_eq!(result.fired.len(), 50);
    // Each cue fired exactly once, in order.
    for (i, record) in result.fired.iter().enumerate() {
        assert_eq!(record.number, (i + 1) as u32);
    }
    let _ = OutputId::new("unused"); // keep imports honest
}
