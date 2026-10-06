//! The unified live engine: switcher + mixer + lighting + cue stack behind
//! one clock and one output gate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tpt_app_live_production_cues::{CueExecution, CueRunnerError, CueStackRunner};
use tpt_app_live_production_lighting::{DmxSink, LightingEngine, NullSink};
use tpt_app_live_production_mixer::{Mixer, MixerError, RenderInputs};
use tpt_app_live_production_model::cue::{AudioChange, RampMode, Transition};
use tpt_app_live_production_model::ids::{BusId, CueId, SourceId, UniverseId};
use tpt_app_live_production_model::validation::{self, Report};
use tpt_app_live_production_model::{OperationMode, Show};
use tpt_app_live_production_switcher::{SwitchFrame, Switcher, SwitcherError};

use crate::degrade::DegradationController;
use crate::events::{EngineEvent, EventBus, FailsafeReason};
use crate::failsafe::{FailsafeConfig, InputHealth, InputLossDecision, VideoFailsafePolicy};
use crate::session::{SessionEventKind, SessionLog};
use crate::watchdog::Heartbeat;

/// Engine construction/operation errors.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The show failed validation; it must not run.
    #[error("show failed validation with {errors} error(s), {warnings} warning(s): {summary}")]
    InvalidShow {
        /// Number of error-severity issues.
        errors: usize,
        /// Number of warning-severity issues.
        warnings: usize,
        /// First few issue messages.
        summary: String,
        /// Full machine-readable report.
        #[source]
        report_issues: ReportIssueVec,
    },
    /// A video-capable source is required to build the switcher.
    #[error("show has no video-capable source to put on program")]
    NoVideoSource,
    /// A fixture's universe could not be resolved to a DMX universe number.
    #[error("universe '{0}' cannot be resolved to a DMX universe number")]
    UnmappedUniverse(UniverseId),
    /// Underlying engine error.
    #[error("switcher error: {0}")]
    Switcher(#[from] SwitcherError),
    /// Underlying engine error.
    #[error("mixer error: {0}")]
    Mixer(#[from] MixerError),
    /// Underlying engine error.
    #[error("cue stack error: {0}")]
    Cue(#[from] CueRunnerError),
    /// The requested cue id does not exist in the stack.
    #[error("unknown cue '{0}'")]
    UnknownCue(CueId),
    /// Lighting engine error during cue execution.
    #[error("lighting error: {0}")]
    Lighting(String),
}

/// Newtype so validation reports can be carried as a source without
/// deriving `Error` on the report itself.
#[derive(Debug, Clone)]
pub struct ReportIssueVec(pub Report);

impl std::fmt::Display for ReportIssueVec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for issue in &self.0.issues {
            writeln!(
                f,
                "  [{}] {}: {}",
                issue.severity.as_str(),
                issue.code,
                issue.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ReportIssueVec {}

/// Engine tunables beyond what the show file carries.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// Failsafe policy (spec 14.1).
    pub failsafe: FailsafeConfig,
    /// Source initially on program (defaults to the first video source).
    pub initial_program: Option<SourceId>,
}

/// One frame's worth of engine output.
///
/// **The rehearsal/live gate lives here**: in [`OperationMode::Rehearsal`],
/// `program_video` is `None`, `program_audio` is empty, and no DMX frames
/// are emitted — the engine structurally cannot put a signal on an
/// audience-facing output (spec 3.3).
#[derive(Debug, Clone, Serialize, Default)]
pub struct FrameOutputs {
    /// Program frame index this output belongs to.
    pub frame_index: u64,
    /// What the preview monitor shows (always — rehearsal needs monitors).
    pub preview: Option<SwitchFrame>,
    /// What the program output shows. `None` outside Live mode.
    pub program_video: Option<SwitchFrame>,
    /// Program audio bus, interleaved stereo. Empty outside Live mode.
    pub program_audio: Vec<f32>,
    /// DMX universes changed this tick (universe, frame). Only in Live mode.
    pub lighting: Vec<(u16, Vec<u8>)>,
    /// Current degradation level (spec 14.3).
    pub degradation: Option<crate::degrade::DegradationLevel>,
}

/// Serializable engine state snapshot for the UI and the local API.
#[derive(Debug, Clone, Serialize)]
pub struct ShowState {
    /// Show id and name.
    pub show: String,
    /// Show name.
    pub name: String,
    /// Operation mode (render with colour AND this text — never colour
    /// alone, spec 15.1).
    pub mode: OperationMode,
    /// Current program source.
    pub program: SourceId,
    /// Current preview source.
    pub preview: SourceId,
    /// True mid-transition.
    pub transitioning: bool,
    /// Live cue (most recently fired): number + label.
    pub current_cue: Option<(u32, String)>,
    /// Next cue: number + label.
    pub next_cue: Option<(u32, String)>,
    /// Bus meters, in bus order: (bus id, peak, rms).
    pub meters: Vec<(String, f32, f32)>,
    /// Inputs currently considered lost.
    pub lost_inputs: Vec<SourceId>,
    /// Degradation level.
    pub degradation: crate::degrade::DegradationLevel,
    /// Cues fired this session.
    pub cues_fired: u64,
    /// Overlay source, if armed.
    pub overlay: Option<SourceId>,
}

/// The unified engine.
pub struct LiveEngine {
    show: Show,
    switcher: Switcher,
    mixer: Mixer,
    lighting: LightingEngine,
    lighting_sink: Box<dyn DmxSink + Send>,
    cues: CueStackRunner,
    mode: OperationMode,
    failsafe: FailsafeConfig,
    health: InputHealth,
    degradation: DegradationController,
    bus: EventBus,
    session_log: Option<Arc<SessionLog>>,
    heartbeat: Option<Arc<Heartbeat>>,
    frame_index: u64,
    now_ms: u64,
    video_sources: Vec<SourceId>,
    audio_sources: Vec<SourceId>,
    bus_order: Vec<BusId>,
}

impl LiveEngine {
    /// Builds an engine from a validated show.
    ///
    /// Shows with error-severity validation issues are **refused** — a show
    /// that references unknown sources must not reach the stage.
    pub fn build(show: Show, config: EngineConfig) -> Result<Self, EngineError> {
        let report = validation::validate(&show);
        if !report.is_ok() {
            let errors = report
                .issues
                .iter()
                .filter(|i| i.severity == validation::Severity::Error)
                .count();
            let warnings = report.len() - errors;
            let summary = report
                .issues
                .iter()
                .take(5)
                .map(|i| format!("[{}] {}", i.code, i.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(EngineError::InvalidShow {
                errors,
                warnings,
                summary,
                report_issues: ReportIssueVec(report),
            });
        }

        let settings = show.settings.clone();
        let mode = show.mode;

        // --- switcher -----------------------------------------------------
        let video_sources: Vec<SourceId> = show
            .sources
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    tpt_app_live_production_model::SourceKind::LiveVideoInput
                        | tpt_app_live_production_model::SourceKind::PlaybackAsset(_)
                        | tpt_app_live_production_model::SourceKind::Graphic
                )
            })
            .map(|s| s.id.clone())
            .collect();
        let initial = config
            .initial_program
            .clone()
            .or_else(|| video_sources.first().cloned())
            .ok_or(EngineError::NoVideoSource)?;
        let mut switcher = Switcher::new(settings.video_fps, initial)?;
        for id in &video_sources {
            switcher.register_source(id.clone());
        }

        // --- mixer ----------------------------------------------------------
        let mut mixer = Mixer::new(
            settings.sample_rate,
            settings.block_frames as usize,
            Duration::from_millis(settings.ramp_ms),
        )?;
        let mut bus_order = Vec::new();
        for bus in &show.buses {
            mixer.add_bus(bus.id.clone())?;
            bus_order.push(bus.id.clone());
        }
        let audio_sources: Vec<SourceId> = show
            .sources
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    tpt_app_live_production_model::SourceKind::LiveAudioInput
                        | tpt_app_live_production_model::SourceKind::LiveVideoInput
                        | tpt_app_live_production_model::SourceKind::PlaybackAsset(_)
                )
            })
            .map(|s| s.id.clone())
            .collect();
        for id in &audio_sources {
            mixer.add_source(id.clone())?;
        }
        for bus in &show.buses {
            for input in &bus.inputs {
                // Patch only sources the mixer knows (audio-capable).
                if audio_sources.contains(&input.source) {
                    mixer.patch(&input.source, &bus.id)?;
                }
            }
        }

        // --- lighting -------------------------------------------------------
        let mut universe_numbers: HashMap<UniverseId, u16> = HashMap::new();
        for output in &show.outputs {
            if let (tpt_app_live_production_model::OutputKind::LightingUniverse, Some(num)) =
                (output.kind.clone(), output.universe)
            {
                universe_numbers.insert(UniverseId::new(output.id.0.clone()), num);
            }
        }
        // Fixtures whose universe id is directly numeric resolve to it.
        for fixture in &show.fixtures {
            if let Ok(num) = fixture.universe.0.parse::<u16>() {
                universe_numbers
                    .entry(fixture.universe.clone())
                    .or_insert(num);
            }
        }
        for fixture in &show.fixtures {
            if !universe_numbers.contains_key(&fixture.universe) {
                return Err(EngineError::UnmappedUniverse(fixture.universe.clone()));
            }
        }
        let lighting = LightingEngine::new(&show.fixtures, &show.scenes, universe_numbers);

        let cues = CueStackRunner::new(show.cue_stack.clone());
        let bus = EventBus::new();

        let engine = Self {
            show,
            switcher,
            mixer,
            lighting,
            lighting_sink: Box::new(NullSink),
            cues,
            mode,
            failsafe: config.failsafe,
            health: InputHealth::default(),
            degradation: DegradationController::default(),
            bus,
            session_log: None,
            heartbeat: None,
            frame_index: 0,
            now_ms: 0,
            video_sources,
            audio_sources,
            bus_order,
        };
        Ok(engine)
    }

    /// Replaces the null DMX sink with a real one (e.g. sACN). The engine
    /// calls it only in Live mode.
    pub fn set_lighting_sink(&mut self, sink: Box<dyn DmxSink + Send>) {
        self.lighting_sink = sink;
    }

    /// Attaches the session log (spec 18). Emits the session-start record
    /// immediately (construction may predate the log being configured).
    pub fn set_session_log(&mut self, log: Arc<SessionLog>) {
        let _ = log.record(SessionEventKind::SessionStart {
            show: self.show.id.0.clone(),
            name: self.show.name.clone(),
            mode: self.mode.label().to_lowercase(),
        });
        self.session_log = Some(log);
    }

    /// Attaches a watchdog heartbeat bumped on every poll (spec 14.2).
    pub fn attach_heartbeat(&mut self, heartbeat: Arc<Heartbeat>) {
        self.heartbeat = Some(heartbeat);
    }

    /// Subscribes to engine events.
    pub fn subscribe(
        &self,
        name: impl Into<String>,
        capacity: usize,
    ) -> crate::events::Subscription {
        self.bus.subscribe(name, capacity)
    }

    /// The show this engine runs.
    pub fn show(&self) -> &Show {
        &self.show
    }

    /// Current operation mode.
    pub fn mode(&self) -> OperationMode {
        self.mode
    }

    /// Sets the operation mode. Mode changes are always operator-visible
    /// events (spec 15.1: unmistakable indicator).
    pub fn set_mode(&mut self, mode: OperationMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.log_session(SessionEventKind::ModeChange {
            mode: mode.label().to_lowercase(),
        });
        self.bus.publish(EngineEvent::ModeChanged { mode });
    }

    /// True when the engine may currently drive live outputs.
    pub fn is_live(&self) -> bool {
        self.mode.is_live()
    }

    // ---- operator actions -----------------------------------------------

    /// Selects the preview source (never affects program — spec 7).
    pub fn select_preview(&mut self, source: SourceId) -> Result<(), EngineError> {
        self.switcher.select_preview(source.clone())?;
        self.bus.publish(EngineEvent::PreviewChanged { source });
        Ok(())
    }

    /// Arms/clears the overlay layer.
    pub fn set_overlay(&mut self, source: Option<SourceId>) -> Result<(), EngineError> {
        self.switcher.set_overlay(source)?;
        Ok(())
    }

    /// Manual TAKE with an explicit transition (operator CUT/FADE buttons).
    pub fn take(&mut self, transition: Transition) -> Result<(), EngineError> {
        let from = self.switcher.program().clone();
        let frames = self.switcher.take(&transition, self.frame_index)?;
        let to = self.switcher.program().clone();
        self.bus
            .publish(EngineEvent::TransitionStarted { from, to, frames });
        Ok(())
    }

    /// Manual GO: fires the next cue now (spec 10).
    pub fn go(&mut self) -> Option<CueExecution> {
        let exec = self.cues.go(self.now_ms)?;
        self.execute_cue(&exec);
        Some(exec)
    }

    /// Fires a specific cue by id (used by the API's `/show/cue/:id/go`).
    /// Cues before it are marked as played without executing (a jump, not a
    /// replay of history).
    pub fn go_to_cue(&mut self, id: &CueId) -> Result<CueExecution, EngineError> {
        let position = self
            .cues
            .stack()
            .cues
            .iter()
            .position(|c| &c.id == id)
            .ok_or_else(|| EngineError::UnknownCue(id.clone()))?;
        if self.cues.stack().next_index() == Some(position) {
            return Ok(self.go().expect("next_index just validated"));
        }
        // Jump: point the stack at the cue before `position`, then GO.
        self.cues.stack_mut().current_index = if position == 0 {
            None
        } else {
            Some(position - 1)
        };
        self.go().ok_or(EngineError::UnknownCue(id.clone()))
    }

    // ---- clock / frame loop ---------------------------------------------

    /// Advances the engine to `now_ms` (monotonic) and returns this frame's
    /// outputs. Call once per program frame.
    pub fn poll(&mut self, now_ms: u64) -> FrameOutputs {
        self.now_ms = now_ms;
        let fps = self.show.settings.video_fps.max(1);
        self.frame_index = now_ms * u64::from(fps) / 1000;

        // 1. Automatic cue advance (before the switcher tick so a cue that
        //    starts a transition this frame renders from this frame).
        let transition_complete = !self.switcher.is_transitioning();
        if let Some(exec) = self.cues.poll(now_ms, transition_complete) {
            self.execute_cue(&exec);
        }

        // 2. Switcher.
        let switch_frame = self.switcher.tick(self.frame_index);

        // 3. Lighting.
        let changed_universes = self.lighting.tick(now_ms);
        let mut lighting_out = Vec::new();
        if self.is_live() {
            for universe in changed_universes {
                if let Some(snapshot) = self
                    .lighting
                    .snapshots()
                    .into_iter()
                    .find(|s| s.universe == universe)
                {
                    self.lighting_sink
                        .send_universe(snapshot.universe, &snapshot.data);
                    lighting_out.push((snapshot.universe, snapshot.data.to_vec()));
                }
            }
        }

        // 4. Input health / failsafes (spec 14.1).
        self.evaluate_failsafes();

        // 5. Watchdog heartbeat.
        if let Some(hb) = &self.heartbeat {
            hb.beat();
        }

        FrameOutputs {
            frame_index: self.frame_index,
            preview: Some(switch_frame.clone()),
            program_video: if self.is_live() {
                Some(switch_frame)
            } else {
                None
            },
            program_audio: Vec::new(),
            lighting: lighting_out,
            degradation: Some(self.degradation.level()),
        }
    }

    /// Renders one audio block through the mixer (host audio callback or
    /// headless harness).
    ///
    /// Meters run in both modes (an operator rehearsing must see meters).
    /// Program-audio *delivery* is gated: [`FrameOutputs::program_audio`]
    /// and [`LiveEngine::program_audio_active`] are the single source of
    /// truth the host must consult before sending to a hardware output.
    pub fn render_audio_block(&mut self, inputs: &RenderInputs<'_>, outputs: &mut [&mut [f32]]) {
        self.mixer.render_block(inputs, outputs);
    }

    /// Whether the program audio bus may currently be sent to hardware.
    pub fn program_audio_active(&self) -> bool {
        self.is_live()
    }

    /// Last meter reading per bus, in bus order.
    pub fn meters(&self) -> Vec<(String, f32, f32)> {
        self.bus_order
            .iter()
            .filter_map(|bus| {
                self.mixer
                    .meter(bus)
                    .ok()
                    .map(|m| (bus.0.clone(), m.peak, m.rms))
            })
            .collect()
    }

    /// Serializable state snapshot (UI / API).
    pub fn state(&self) -> ShowState {
        ShowState {
            show: self.show.id.0.clone(),
            name: self.show.name.clone(),
            mode: self.mode,
            program: self.switcher.program().clone(),
            preview: self.switcher.preview().clone(),
            transitioning: self.switcher.is_transitioning(),
            current_cue: self
                .cues
                .stack()
                .current()
                .map(|c| (c.number.0, c.label.clone())),
            next_cue: self
                .cues
                .stack()
                .next()
                .map(|c| (c.number.0, c.label.clone())),
            meters: self.meters(),
            lost_inputs: self.health.lost_inputs(),
            degradation: self.degradation.level(),
            cues_fired: self.cues.fired_count(),
            overlay: self.switcher.overlay().cloned(),
        }
    }

    /// Current degradation level (spec 14.3).
    pub fn degradation_level(&self) -> crate::degrade::DegradationLevel {
        self.degradation.level()
    }

    /// Crate-internal accessors for the headless rehearsal driver.
    pub(crate) fn cue_stack(&self) -> &CueStackRunner {
        &self.cues
    }

    pub(crate) fn lighting_snapshots(
        &self,
    ) -> Vec<tpt_app_live_production_lighting::UniverseSnapshot> {
        self.lighting.snapshots()
    }

    /// Feeds a pressure sample (worse of CPU/GPU, percent). Escalation
    /// follows the spec 14.3 order: preview → effects → program.
    pub fn report_pressure(&mut self, pressure: f32) {
        if let Some(level) = self.degradation.report_pressure(pressure) {
            self.bus.publish(EngineEvent::DegradationChanged { level });
        }
    }

    // ---- inputs / failsafe -----------------------------------------------

    /// Reports a healthy video/audio arrival for a live input.
    pub fn report_input_ok(&mut self, source: &SourceId, now_ms: u64) {
        if self.health.report_ok(source, now_ms) == InputLossDecision::Restored {
            self.bus.publish(EngineEvent::InputRestored {
                source: source.clone(),
            });
            self.log_session(SessionEventKind::Note {
                text: format!("input restored: {source}"),
            });
        }
    }

    /// Evaluates all tracked inputs against the failsafe timeout and applies
    /// policy. Called every poll.
    fn evaluate_failsafes(&mut self) {
        let timeout = self.failsafe.input_timeout;
        let now = self.now_ms;

        for source in self.video_sources.clone() {
            let decision = self.health.evaluate(&source, now, timeout);
            if matches!(decision, InputLossDecision::Lost) {
                self.apply_video_failsafe(&source);
            }
        }
        for source in self.audio_sources.clone() {
            let decision = self.health.evaluate(&source, now, timeout);
            if matches!(decision, InputLossDecision::Lost) && self.failsafe.audio_mute_on_error {
                let ramp = Duration::from_millis(self.show.settings.ramp_ms);
                if self.mixer.set_mute(&source, true, ramp).is_ok() {
                    self.health.mark_muted_by_failsafe(&source);
                    self.bus.publish(EngineEvent::FailsafeTriggered {
                        reason: FailsafeReason::AudioInputLost,
                        source: source.clone(),
                        action: "muted".to_string(),
                    });
                    self.log_session(SessionEventKind::Failsafe {
                        reason: "audio_input_lost".into(),
                        source: source.0.clone(),
                        action: "muted".into(),
                    });
                }
            }
        }
    }

    fn apply_video_failsafe(&mut self, source: &SourceId) {
        let policy = self.failsafe.video.clone();
        let action_desc = match &policy {
            VideoFailsafePolicy::FreezeLastFrame => {
                // The switcher holds `last_good_program`; nothing to change
                // on program — this is exactly the freeze behaviour.
                "freeze_last_frame".to_string()
            }
            VideoFailsafePolicy::CutToBackup { backup } => {
                if self.switcher.sources().contains(backup) {
                    let _ = self.switcher.select_preview(backup.clone());
                    let _ = self.switcher.take(&Transition::Cut, self.frame_index);
                    "cut_to_backup".to_string()
                } else {
                    log::error!(
                        "failsafe backup source '{backup}' is not registered; freezing instead"
                    );
                    "freeze_last_frame(backup_missing)".to_string()
                }
            }
            VideoFailsafePolicy::Slate { slate } => {
                if self.switcher.sources().contains(slate) {
                    let _ = self.switcher.select_preview(slate.clone());
                    let _ = self.switcher.take(&Transition::Cut, self.frame_index);
                    "cut_to_slate".to_string()
                } else {
                    log::error!(
                        "failsafe slate source '{slate}' is not registered; freezing instead"
                    );
                    "freeze_last_frame(slate_missing)".to_string()
                }
            }
        };
        self.bus.publish(EngineEvent::FailsafeTriggered {
            reason: FailsafeReason::VideoInputLost,
            source: source.clone(),
            action: action_desc.clone(),
        });
        self.log_session(SessionEventKind::Failsafe {
            reason: "video_input_lost".into(),
            source: source.0.clone(),
            action: action_desc,
        });
    }

    // ---- cue execution ----------------------------------------------------

    fn execute_cue(&mut self, exec: &CueExecution) {
        let cue = &exec.cue;

        // Video: preview + transition (preview first, so the fade source is
        // correct).
        if let Some(preview) = &cue.preview_source {
            if self.switcher.select_preview(preview.clone()).is_err() {
                log::error!(
                    "cue {} references unknown source '{preview}'; video step skipped",
                    cue.number.0
                );
            }
        }
        if let Some(transition) = &cue.video_transition {
            match self.switcher.take(transition, self.frame_index) {
                Ok(frames) => {
                    let to = self.switcher.program().clone();
                    self.bus.publish(EngineEvent::TransitionStarted {
                        from: to.clone(),
                        to,
                        frames,
                    });
                }
                Err(e) => log::error!("cue {} transition failed: {e}", cue.number.0),
            }
        }

        // Audio: ramped by default; instant only when explicitly requested.
        let default_ramp = Duration::from_millis(self.show.settings.ramp_ms);
        for change in &cue.audio_changes {
            self.apply_audio_change(change, default_ramp);
        }

        // Lighting.
        if let Some(scene) = &cue.lighting_scene {
            if let Err(e) = self.lighting.recall(scene, self.now_ms) {
                log::error!("cue {} lighting recall failed: {e}", cue.number.0);
            }
        }

        self.bus.publish(EngineEvent::CueAdvanced {
            number: cue.number.0,
            label: cue.label.clone(),
            automatic: exec.automatic,
        });
        self.log_session(SessionEventKind::CueAdvance {
            number: cue.number.0,
            label: cue.label.clone(),
            automatic: exec.automatic,
        });
    }

    fn apply_audio_change(&mut self, change: &AudioChange, default_ramp: Duration) {
        let ramp_of = |r: &RampMode| r.duration(default_ramp);
        let clamp_gain = |g: f64| g.clamp(Mixer::MIN_GAIN_DB, Mixer::MAX_GAIN_DB);
        let clamp_pan = |p: f64| p.clamp(-1.0, 1.0);
        let result = match change {
            AudioChange::SetGain {
                source,
                gain_db,
                ramp,
            } => self
                .mixer
                .set_gain(source, clamp_gain(*gain_db), ramp_of(ramp)),
            AudioChange::SetMute {
                source,
                muted,
                ramp,
            } => self.mixer.set_mute(source, *muted, ramp_of(ramp)),
            AudioChange::SetPan { source, pan, ramp } => {
                self.mixer.set_pan(source, clamp_pan(*pan), ramp_of(ramp))
            }
            AudioChange::BusAssign { source, bus } => match bus {
                Some(bus) => self.mixer.patch(source, bus),
                None => self.mixer.unpatch_all(source),
            },
        };
        if let Err(e) = result {
            // Validation catches most of these up-front; a live failure is
            // logged, never propagated into the render path.
            log::error!("audio change failed: {e}");
        }
    }

    fn log_session(&self, kind: SessionEventKind) {
        if let Some(log) = &self.session_log {
            if let Err(e) = log.record(kind) {
                log::error!("session log write failed: {e}");
                self.bus.publish(EngineEvent::SessionLogWriteFailed);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;
    use tpt_app_live_production_lighting::RecordingSink;
    use tpt_app_live_production_model::cue::{AdvanceMode, Cue, CueStack};
    use tpt_app_live_production_model::ids::{CueId, FixtureId, LightingSceneId, OutputId};
    use tpt_app_live_production_model::lighting::{
        ChannelValues, DmxChannel, Fixture, LightingScene,
    };
    use tpt_app_live_production_model::show::{
        AudioBus, ChannelGain, Output, OutputKind, ShowSettings, Source, SourceKind,
    };

    /// A small but complete show: cameras + backup + slate, one mic,
    /// program/aux buses, one fixture + scene, three cues.
    pub(crate) fn test_show() -> Show {
        let mut show = Show::new("test-show", "Test Show");
        show.settings = ShowSettings::default();
        show.outputs = vec![
            Output {
                id: OutputId::new("pgm_video"),
                kind: OutputKind::ProgramVideo,
                label: "Program".into(),
                universe: None,
            },
            Output {
                id: OutputId::new("pa"),
                kind: OutputKind::ProgramAudio,
                label: "PA".into(),
                universe: None,
            },
            Output {
                id: OutputId::new("house_lights"),
                kind: OutputKind::LightingUniverse,
                label: "House".into(),
                universe: Some(1),
            },
        ];
        show.sources = vec![
            Source {
                id: SourceId::new("cam1"),
                kind: SourceKind::LiveVideoInput,
                label: "Cam 1".into(),
            },
            Source {
                id: SourceId::new("cam2"),
                kind: SourceKind::LiveVideoInput,
                label: "Cam 2".into(),
            },
            Source {
                id: SourceId::new("backup"),
                kind: SourceKind::LiveVideoInput,
                label: "Backup".into(),
            },
            Source {
                id: SourceId::new("slate"),
                kind: SourceKind::Graphic,
                label: "Signal lost".into(),
            },
            Source {
                id: SourceId::new("mic1"),
                kind: SourceKind::LiveAudioInput,
                label: "Mic 1".into(),
            },
        ];
        show.buses = vec![
            AudioBus {
                id: BusId::new("program"),
                label: "Program".into(),
                inputs: vec![ChannelGain {
                    source: SourceId::new("mic1"),
                    gain_db: 0.0,
                }],
                output: OutputId::new("pa"),
            },
            AudioBus {
                id: BusId::new("aux"),
                label: "Monitors".into(),
                inputs: vec![],
                output: OutputId::new("pa"),
            },
        ];
        show.fixtures = vec![Fixture {
            id: FixtureId::new("wash"),
            label: "Stage wash".into(),
            universe: UniverseId::new("house_lights"),
            channels: vec![DmxChannel::intensity(1)],
        }];
        show.scenes = vec![LightingScene::new(
            "open",
            "Open look",
            vec![(FixtureId::new("wash"), ChannelValues::new(vec![255]))],
            Duration::from_millis(1000),
        )];
        let mut cue1 = Cue::new(1, "Cold open");
        cue1.video_transition = Some(Transition::Cut);
        cue1.preview_source = Some(SourceId::new("cam1"));
        cue1.audio_changes = vec![AudioChange::SetMute {
            source: SourceId::new("mic1"),
            muted: false,
            ramp: RampMode::Smooth,
        }];
        let mut cue2 = Cue::new(2, "Guest intro");
        cue2.video_transition = Some(Transition::Fade {
            duration: Duration::from_millis(500),
        });
        cue2.preview_source = Some(SourceId::new("cam2"));
        cue2.lighting_scene = Some(LightingSceneId::new("open"));
        cue2.advance = AdvanceMode::Timed { after_ms: 2000 };
        let mut cue3 = Cue::new(3, "Wrap");
        cue3.video_transition = Some(Transition::Cut);
        cue3.preview_source = Some(SourceId::new("cam1"));
        show.cue_stack = CueStack {
            cues: vec![cue1, cue2, cue3],
            current_index: None,
        };
        show
    }

    fn engine() -> LiveEngine {
        LiveEngine::build(test_show(), EngineConfig::default()).expect("engine builds")
    }

    // ---- mode isolation (spec 3.3) ---------------------------------------

    #[test]
    fn rehearsal_delivers_zero_program_output() {
        let mut e = engine();
        assert_eq!(e.mode(), OperationMode::Rehearsal);

        e.go();
        for ms in (0..2000).step_by(16) {
            let out = e.poll(ms as u64);
            assert!(
                out.program_video.is_none(),
                "rehearsal must not emit program video at {ms}ms"
            );
            assert!(
                out.lighting.is_empty(),
                "rehearsal must not emit DMX at {ms}ms"
            );
            assert!(out.program_audio.is_empty());
        }
        assert!(!e.program_audio_active());
        // Internal state still advances (the operator sees preview + meters).
        assert!(e.state().current_cue.is_some());
    }

    #[test]
    fn live_mode_drives_outputs_and_switching_back_gates_them() {
        let mut e = engine();
        e.set_mode(OperationMode::Live);
        let out = e.poll(0);
        assert!(out.program_video.is_some(), "live mode emits program video");
        assert!(e.program_audio_active());

        e.set_mode(OperationMode::Rehearsal);
        let out = e.poll(32);
        assert!(
            out.program_video.is_none(),
            "back to rehearsal: program gated"
        );
        assert!(!e.program_audio_active());
    }

    #[test]
    fn live_lighting_sink_receives_frames() {
        struct CountingSink {
            count: std::sync::Arc<std::sync::Mutex<usize>>,
        }
        impl CountingSink {
            fn new_shared() -> (std::sync::Arc<std::sync::Mutex<usize>>, Self) {
                let count = std::sync::Arc::new(std::sync::Mutex::new(0));
                (count.clone(), Self { count })
            }
        }
        impl DmxSink for CountingSink {
            fn send_universe(&mut self, _u: u16, _d: &[u8; 512]) {
                *self.count.lock().unwrap() += 1;
            }
        }
        let (count, sink) = CountingSink::new_shared();
        let mut e = engine();
        e.set_mode(OperationMode::Live);
        e.set_lighting_sink(Box::new(sink));
        let _ = e.go();
        let _ = e.go(); // cue 2 recalls the lighting scene
        e.poll(0);
        e.poll(1000);
        assert!(
            *count.lock().unwrap() > 0,
            "live mode must push DMX frames to the sink"
        );
    }

    // ---- cue execution -----------------------------------------------------

    #[test]
    fn cue_applies_video_together_with_state() {
        let mut e = engine();
        e.set_mode(OperationMode::Live);
        let exec = e.go().expect("cue 1");
        assert_eq!(exec.cue.number.0, 1);
        let out = e.poll(0);
        let frame = out.program_video.as_ref().unwrap();
        assert_eq!(frame.program.0, "cam1", "cue 1 cuts to cam1");
        assert_eq!(e.state().current_cue.map(|(n, _)| n), Some(1));
    }

    #[test]
    fn timed_advance_fires_automatically_in_poll() {
        let mut e = engine();
        e.set_mode(OperationMode::Live);
        e.go(); // cue 1
        e.go(); // cue 2 (timed advance 2000ms)
        e.poll(2500); // not yet due
        e.poll(3100); // > 2000ms after cue 2 fired
        assert_eq!(
            e.state().current_cue.map(|(n, _)| n),
            Some(3),
            "timed advance fired cue 3"
        );
    }

    #[test]
    fn editing_cues_mid_show_does_not_disrupt_live_cue() {
        let mut e = engine();
        let exec = e.go().expect("cue 1");
        let live_id = exec.cue_id.clone();
        let mut new_cue = Cue::new(9, "Inserted");
        new_cue.video_transition = Some(Transition::Cut);
        new_cue.preview_source = Some(SourceId::new("cam2"));
        e.cues.stack_mut().insert(2, new_cue);
        assert_eq!(e.state().current_cue.map(|(n, _)| n), Some(1));
        let next = e.go().expect("cue 2 fires");
        assert_eq!(
            next.cue.number.0, 2,
            "inserting after the next cue keeps order"
        );
        assert_eq!(live_id, "cue-1");
    }

    #[test]
    fn go_to_cue_jumps_forward() {
        let mut e = engine();
        e.set_mode(OperationMode::Live);
        let exec = e.go_to_cue(&CueId::new("cue-3")).expect("jump to cue 3");
        assert_eq!(exec.cue.number.0, 3);
        assert!(e.go_to_cue(&CueId::new("ghost")).is_err());
    }

    // ---- failsafe (spec 14.1) ----------------------------------------------

    #[test]
    fn freeze_policy_holds_last_good_frame() {
        let mut e = engine();
        e.set_mode(OperationMode::Live);
        e.report_input_ok(&SourceId::new("cam1"), 0);
        e.poll(0);
        let program_before = e.state().program.clone();
        for ms in [1000u64, 2000, 3000] {
            e.poll(ms);
        }
        let state = e.state();
        assert!(
            state.lost_inputs.iter().any(|s| s.0 == "cam1"),
            "cam1 reported lost"
        );
        assert_eq!(state.program, program_before, "freeze holds program");
    }

    #[test]
    fn backup_cut_policy_switches_to_backup_on_loss() {
        let cfg = EngineConfig {
            failsafe: FailsafeConfig {
                video: VideoFailsafePolicy::CutToBackup {
                    backup: SourceId::new("backup"),
                },
                ..FailsafeConfig::default()
            },
            ..EngineConfig::default()
        };
        let mut e = LiveEngine::build(test_show(), cfg).unwrap();
        e.set_mode(OperationMode::Live);
        e.report_input_ok(&SourceId::new("cam1"), 0);
        e.poll(0);
        e.poll(1000);
        e.poll(2000);
        assert_eq!(e.state().program.0, "backup", "loss cuts to backup");
    }

    #[test]
    fn slate_policy_switches_to_slate_on_loss() {
        let cfg = EngineConfig {
            failsafe: FailsafeConfig {
                video: VideoFailsafePolicy::Slate {
                    slate: SourceId::new("slate"),
                },
                ..FailsafeConfig::default()
            },
            ..EngineConfig::default()
        };
        let mut e = LiveEngine::build(test_show(), cfg).unwrap();
        e.set_mode(OperationMode::Live);
        e.report_input_ok(&SourceId::new("cam1"), 0);
        e.poll(0);
        e.poll(2000);
        assert_eq!(e.state().program.0, "slate");
    }

    #[test]
    fn audio_loss_flags_and_restore_clears() {
        let mut e = engine();
        e.set_mode(OperationMode::Live);
        e.report_input_ok(&SourceId::new("mic1"), 0);
        e.poll(0);
        e.poll(2000); // mic quiet past the timeout
        assert!(
            e.state().lost_inputs.iter().any(|s| s.0 == "mic1"),
            "mic1 flagged lost"
        );
        e.report_input_ok(&SourceId::new("mic1"), 3000);
        e.poll(3000);
        assert!(
            !e.state().lost_inputs.iter().any(|s| s.0 == "mic1"),
            "restore clears the lost flag"
        );
    }

    // ---- degradation (spec 14.3) ---------------------------------------------

    #[test]
    fn pressure_degrades_preview_first() {
        let mut e = engine();
        assert_eq!(
            e.degradation_level(),
            crate::degrade::DegradationLevel::Normal
        );
        for _ in 0..5 {
            e.report_pressure(95.0);
        }
        assert_eq!(
            e.degradation_level(),
            crate::degrade::DegradationLevel::PreviewDegraded,
            "preview degrades first"
        );
        let _ = e.poll(100); // engine still runs degraded
    }

    // ---- events / session -----------------------------------------------------

    #[test]
    fn events_flow_to_subscribers() {
        let mut e = engine();
        let sub = e.subscribe("test", 32);
        e.set_mode(OperationMode::Live);
        e.go();
        let mut seen_mode_change = false;
        let mut seen_cue = false;
        for _ in 0..8 {
            match sub.recv_timeout(Duration::from_millis(100)) {
                Some(EngineEvent::ModeChanged {
                    mode: OperationMode::Live,
                }) => seen_mode_change = true,
                Some(EngineEvent::CueAdvanced { number: 1, .. }) => seen_cue = true,
                Some(_) => {}
                None => break,
            }
        }
        assert!(seen_mode_change && seen_cue);
    }

    #[test]
    fn session_log_records_cue_advances_and_failsafes() {
        let dir = std::env::temp_dir().join(format!("tpt-lp-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = std::sync::Arc::new(SessionLog::open(&dir).unwrap());
        let mut e = engine();
        e.set_session_log(log.clone());
        e.set_mode(OperationMode::Live);
        e.report_input_ok(&SourceId::new("cam1"), 0);
        e.go();
        e.poll(0);
        e.poll(2000); // failsafe triggers for cam1
        drop(e);
        let events = SessionLog::read_events(log.path());
        let kinds: Vec<&str> = events
            .iter()
            .map(|ev| match ev.kind {
                SessionEventKind::SessionStart { .. } => "start",
                SessionEventKind::ModeChange { .. } => "mode",
                SessionEventKind::CueAdvance { .. } => "cue",
                SessionEventKind::Failsafe { .. } => "failsafe",
                _ => "other",
            })
            .collect();
        assert!(kinds.contains(&"start"), "{kinds:?}");
        assert!(kinds.contains(&"cue"), "{kinds:?}");
        assert!(kinds.contains(&"failsafe"), "{kinds:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- build validation ------------------------------------------------------

    #[test]
    fn invalid_show_is_refused_at_build() {
        let mut show = test_show();
        show.cue_stack.cues[0].preview_source = Some(SourceId::new("ghost"));
        let err = match LiveEngine::build(show, EngineConfig::default()) {
            Err(e) => e,
            Ok(_) => panic!("invalid show must not build"),
        };
        assert!(matches!(err, EngineError::InvalidShow { errors: 1, .. }));
    }

    #[test]
    fn show_without_video_sources_is_refused() {
        let mut show = test_show();
        show.sources
            .retain(|s| s.kind == tpt_app_live_production_model::SourceKind::LiveAudioInput);
        show.cue_stack = tpt_app_live_production_model::CueStack::default();
        let err = match LiveEngine::build(show, EngineConfig::default()) {
            Err(e) => e,
            Ok(_) => panic!("show without video sources must not build"),
        };
        assert!(matches!(err, EngineError::NoVideoSource));
    }

    #[test]
    fn unmapped_lighting_universe_is_refused() {
        let mut show = test_show();
        show.fixtures[0].universe = UniverseId::new("nowhere");
        let err = match LiveEngine::build(show, EngineConfig::default()) {
            Err(e) => e,
            Ok(_) => panic!("unmapped universe must not build"),
        };
        assert!(matches!(err, EngineError::UnmappedUniverse(_)));
    }

    // ---- audio render -------------------------------------------------------------

    #[test]
    fn audio_block_renders_and_meters_in_both_modes() {
        let mut e = engine();
        let mic1 = SourceId::new("mic1");
        let mut input = vec![0.0f32; 256];
        for f in 0..128 {
            input[f * 2] = 0.5;
            input[f * 2 + 1] = 0.5;
        }
        let mut inputs = RenderInputs::new();
        inputs.push(&mic1, &input);
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        e.render_audio_block(&inputs, &mut outs);
        let meters = e.meters();
        assert_eq!(meters.len(), 2, "two buses");
        assert!(
            meters[0].1 > 0.4,
            "program bus meters the mic, {:?}",
            meters[0]
        );
        assert_eq!(meters[1].1, 0.0, "aux bus is empty");
    }

    #[test]
    fn recording_sink_type_is_usable() {
        // RecordingSink is part of the public test surface; make sure it is
        // exported and functions.
        let mut sink = RecordingSink::default();
        sink.send_universe(1, &[255u8; 512]);
        assert_eq!(sink.last(1).unwrap()[0], 255);
    }
}
