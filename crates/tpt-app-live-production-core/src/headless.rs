//! Headless, clock-simulated rehearsal of a cue stack (spec 16).
//!
//! Runs a show through a [`LiveEngine`] in **Rehearsal** mode with a
//! simulated frame clock: no real time passes, no device I/O happens, and
//! program outputs stay gated (structurally — rehearsal mode). Manual cues
//! are advanced as soon as their predecessor's occupancy elapses, which is
//! exactly what an operator pressing GO on time looks like to the engine.
//!
//! This module is the engine behind `tpt-live-production rehearse --show
//! <file> --headless` and the golden cue-stack regression fixtures.

use serde::Serialize;
use std::time::Duration;

use tpt_app_live_production_model::validation;
use tpt_app_live_production_model::Show;

use crate::engine::{EngineConfig, EngineError, LiveEngine};

/// One fired cue's record.
#[derive(Debug, Clone, Serialize)]
pub struct CueRecord {
    /// Cue number.
    pub number: u32,
    /// Cue label.
    pub label: String,
    /// Simulated show-time (ms) when the cue fired.
    pub fired_at_ms: u64,
    /// Program source after the cue's transition settled.
    pub program_after: String,
    /// Whether a lighting scene was attached.
    pub lighting: Option<String>,
    /// Whether audio changes were attached.
    pub audio_changes: usize,
}

/// Result of a headless rehearsal.
#[derive(Debug, Clone, Serialize)]
pub struct RehearsalResult {
    /// Show name.
    pub show: String,
    /// Total cues in the stack.
    pub cues: usize,
    /// Cue records in firing order.
    pub fired: Vec<CueRecord>,
    /// Program source at the end of the run.
    pub final_program: String,
    /// Lighting value snapshots at the end (universe, channel, value).
    pub lighting_end: Vec<(u16, u16, u8)>,
    /// Validation issues found before running (warnings included).
    pub issues: Vec<IssueRecord>,
    /// Whether the run completed the whole stack.
    pub completed: bool,
}

/// A validation issue, machine-readable.
#[derive(Debug, Clone, Serialize)]
pub struct IssueRecord {
    /// `"error"` or `"warning"`.
    pub severity: String,
    /// Stable issue code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Cue number, when applicable.
    pub cue: Option<u32>,
}

/// Options for the rehearsal run.
#[derive(Debug, Clone)]
pub struct RehearsalOptions {
    /// Simulated tick size in ms (default 16 ms ~= one 60 fps frame).
    pub step_ms: u64,
    /// Safety cap on simulated show time.
    pub max_duration: Duration,
    /// Extra simulated time to run after the stack completes, so fades in
    /// flight settle before end-state snapshots are taken (golden fixtures
    /// want final state, not mid-fade state).
    pub settle_ms: u64,
}

impl Default for RehearsalOptions {
    fn default() -> Self {
        Self {
            step_ms: 16,
            max_duration: Duration::from_secs(3600),
            settle_ms: 5000,
        }
    }
}

/// Validates a show and returns issues without running anything (the CLI
/// `validate` path).
pub fn validate_show(show: &Show) -> Vec<IssueRecord> {
    let report = validation::validate(show);
    report
        .issues
        .iter()
        .map(|i| IssueRecord {
            severity: i.severity.as_str().to_string(),
            code: i.code.clone(),
            message: i.message.clone(),
            cue: i.cue,
        })
        .collect()
}

/// Runs a full headless rehearsal of the show's cue stack.
pub fn rehearse_headless(
    mut show: Show,
    options: RehearsalOptions,
) -> Result<RehearsalResult, EngineError> {
    // Rehearsal runs in Rehearsal mode, unconditionally.
    show.mode = tpt_app_live_production_model::OperationMode::Rehearsal;
    let issues = validate_show(&show);
    let mut engine = LiveEngine::build(show, EngineConfig::default())?;

    let step_ms = options.step_ms.max(1);
    let max_ms = options.max_duration.as_millis() as u64;
    let mut now_ms: u64 = 0;
    let mut fired: Vec<CueRecord> = Vec::new();
    let mut last_fired: Option<(u32, String, u64)> = None;
    let mut completed = false;

    while now_ms <= max_ms {
        let out = engine.poll(now_ms);

        // Record anything that fired this tick.
        if let Some((number, label)) = engine.state().current_cue.clone() {
            let changed = match &last_fired {
                Some((n, _, _)) => *n != number,
                None => true,
            };
            if changed {
                let program_after = out
                    .preview
                    .as_ref()
                    .map(|f| f.program.0.clone())
                    .unwrap_or_default();
                let cue_info = engine
                    .show()
                    .cue_stack
                    .cues
                    .iter()
                    .find(|c| c.number.0 == number)
                    .map(|c| {
                        (
                            c.lighting_scene.as_ref().map(|s| s.0.clone()),
                            c.audio_changes.len(),
                        )
                    });
                fired.push(CueRecord {
                    number,
                    label: label.clone(),
                    fired_at_ms: now_ms,
                    program_after,
                    lighting: cue_info.as_ref().and_then(|(l, _)| l.clone()),
                    audio_changes: cue_info.map(|(_, a)| a).unwrap_or(0),
                });
                last_fired = Some((number, label, now_ms));
            }
        }

        if engine_is_finished(&engine) {
            completed = true;
            // Settle tail: keep the clock running so in-flight fades land
            // on their final values before the end-state snapshot.
            if options.settle_ms > 0 {
                let settle_until = now_ms + options.settle_ms;
                while now_ms < settle_until {
                    now_ms = now_ms.saturating_add(step_ms);
                    let _ = engine.poll(now_ms);
                }
            }
            break;
        }

        // Manual cue due: fire the next GO once the previous cue's
        // occupancy has elapsed (operator pressing GO on time).
        if manual_go_due(&engine, now_ms, last_fired.as_ref().map(|(_, _, t)| *t)) {
            engine.go();
        }

        now_ms = now_ms.saturating_add(step_ms);
    }

    let state = engine.state();
    let lighting_end = engine_lighting_end(&engine);

    Ok(RehearsalResult {
        show: state.name,
        cues: state.cues_fired as usize,
        fired,
        final_program: state.program.0,
        lighting_end,
        issues,
        completed,
    })
}

fn engine_is_finished(engine: &LiveEngine) -> bool {
    // The stack is finished when there is no next cue.
    engine.cue_stack().stack().next().is_none()
}

fn manual_go_due(engine: &LiveEngine, now_ms: u64, last_fired_at: Option<u64>) -> bool {
    let stack = engine.cue_stack().stack();
    // A cue's `advance` field drives how it EXITS (i.e. when the next cue
    // fires). The operator's GO is what fires the next cue when the live
    // cue is Manual; Timed/Follow cues advance themselves via poll.
    let Some(tpt_app_live_production_model::AdvanceMode::Manual) =
        stack.current().map(|c| c.advance)
    else {
        // Before first GO (no current cue): GO immediately. Live cue on
        // Timed/Follow: poll handles it.
        return stack.is_before_first_go();
    };
    // The live cue's occupancy decides when the operator's next GO would
    // naturally land.
    let occupancy_ms = stack
        .current()
        .map(|c| c.occupancy().as_millis() as u64)
        .unwrap_or(0);
    let _ = next_check(stack);
    match last_fired_at {
        None => true,
        Some(t) => now_ms.saturating_sub(t) >= occupancy_ms.max(1),
    }
}

fn next_check(stack: &tpt_app_live_production_model::CueStack) -> bool {
    stack.next().is_some()
}

fn engine_lighting_end(engine: &LiveEngine) -> Vec<(u16, u16, u8)> {
    // Non-zero channel values at the end of the run (universe, address,
    // value) -- enough for golden fixtures and post-run inspection.
    engine
        .lighting_snapshots()
        .into_iter()
        .flat_map(|snap| {
            let universe = snap.universe;
            let mut out = Vec::new();
            for (i, v) in snap.data.iter().enumerate() {
                if *v != 0 {
                    out.push((universe, i as u16 + 1, *v));
                }
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::test_show;

    #[test]
    fn rehearsal_runs_the_whole_stack() {
        let show = test_show();
        let result = rehearse_headless(show, RehearsalOptions::default()).expect("rehearsal");
        assert!(result.completed, "stack completed");
        assert_eq!(result.cues, 3, "all three cues fired");
        assert_eq!(result.final_program, "cam1", "cue 3 wraps back to cam1");
        // Cue ordering recorded.
        let numbers: Vec<u32> = result.fired.iter().map(|f| f.number).collect();
        assert_eq!(numbers, vec![1, 2, 3]);
        // Cue 2 attached the lighting scene.
        assert_eq!(result.fired[1].lighting.as_deref(), Some("open"));
    }

    #[test]
    fn rehearsal_reports_timing_information() {
        let show = test_show();
        let result = rehearse_headless(show, RehearsalOptions::default()).unwrap();
        // Cue 2 fires after cue 1 (manual, immediate first GO).
        assert!(result.fired[0].fired_at_ms <= result.fired[1].fired_at_ms);
        // Cue 3 fires via timed advance >=2000 ms after cue 2. Recorded
        // fire times lag the real firing by up to one 16 ms step (records
        // are taken on the tick after the GO), hence the slop.
        let delta = result.fired[2].fired_at_ms - result.fired[1].fired_at_ms;
        assert!(delta >= 2000 - 16, "timed advance waited only {delta}ms");
    }

    #[test]
    fn rehearsal_is_isolated_from_live_outputs() {
        // rehearse_headless forces Rehearsal mode; the engine structurally
        // gates outputs there (see engine tests). Here we assert the run
        // still produces meaningful state.
        let show = test_show();
        let result = rehearse_headless(show, RehearsalOptions::default()).unwrap();
        assert!(result.completed);
    }

    #[test]
    fn validation_issues_are_reported_not_fatal_when_warnings() {
        let mut show = test_show();
        show.cue_stack.cues[2].audio_changes =
            vec![tpt_app_live_production_model::AudioChange::SetGain {
                source: tpt_app_live_production_model::SourceId::new("mic1"),
                gain_db: 500.0,
                ramp: tpt_app_live_production_model::RampMode::Instant,
            }];
        let result = rehearse_headless(show, RehearsalOptions::default()).unwrap();
        assert!(
            result
                .issues
                .iter()
                .any(|i| i.code == "W_GAIN_OUT_OF_RANGE"),
            "warnings surface in the report: {:?}",
            result.issues
        );
        assert!(result.completed);
    }

    #[test]
    fn error_severity_issues_abort_the_run() {
        let mut show = test_show();
        show.cue_stack.cues[0].preview_source =
            Some(tpt_app_live_production_model::SourceId::new("ghost"));
        let err = rehearse_headless(show, RehearsalOptions::default()).unwrap_err();
        assert!(matches!(err, EngineError::InvalidShow { .. }));
    }

    #[test]
    fn lighting_end_reports_scene_values() {
        let show = test_show();
        let result = rehearse_headless(show, RehearsalOptions::default()).unwrap();
        // The open scene set fixture channel 1 (universe 1) to 255.
        assert!(
            result.lighting_end.contains(&(1, 1, 255)),
            "lighting end state: {:?}",
            result.lighting_end
        );
    }
}
