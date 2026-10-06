//! Show validation (spec 16): the rules behind `validate --show <file>` and
//! the load-time sanity check in the application.
//!
//! Issues are classified as [`Severity::Error`] (the show must not run) or
//! [`Severity::Warning`] (suspicious, but the operator may proceed). The CLI
//! maps errors to exit code 2 and warnings-only to exit code 1.

use crate::ids::is_valid_dmx_address;
use crate::show::Show;
use serde::Serialize;
use std::collections::HashSet;

/// How serious an issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The show must not be run until this is fixed.
    Error,
    /// Suspicious; the operator may proceed.
    Warning,
}

impl Severity {
    /// `"error"` or `"warning"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One validation finding, machine-readable by code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// How serious.
    pub severity: Severity,
    /// Stable machine-readable code, e.g. `"E_DUPLICATE_SOURCE_ID"`.
    pub code: String,
    /// Human-readable description.
    pub message: String,
    /// Cue number the issue relates to, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cue: Option<u32>,
}

/// Constructs an error-severity issue.
fn err(code: &str, message: impl Into<String>) -> Issue {
    Issue {
        severity: Severity::Error,
        code: code.to_string(),
        message: message.into(),
        cue: None,
    }
}

/// Constructs a warning-severity issue.
fn warn(code: &str, message: impl Into<String>) -> Issue {
    Issue {
        severity: Severity::Warning,
        code: code.to_string(),
        message: message.into(),
        cue: None,
    }
}

impl Issue {
    /// Attaches a cue number to this issue.
    pub fn for_cue(mut self, cue: u32) -> Self {
        self.cue = Some(cue);
        self
    }
}

/// Outcome of validating a show.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Findings, ordered errors-first.
    pub issues: Vec<Issue>,
}

impl Report {
    /// True when no [`Severity::Error`] issues were found.
    pub fn is_ok(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// True when there is at least one [`Severity::Warning`] and no errors.
    pub fn has_warnings(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Warning)
    }

    /// Number of findings.
    pub fn len(&self) -> usize {
        self.issues.len()
    }

    /// True when there are no findings at all.
    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Maximum fade/wipe duration the engine considers sane; longer durations
/// produce a warning (they are almost certainly mistakes).
const MAX_TRANSITION_MS: u64 = 10_000;

/// Gain limits accepted in cues and bus patches; out-of-range values warn
/// and are clamped by the engine.
const MAX_GAIN_DB: f64 = 12.0;
const MIN_GAIN_DB: f64 = -120.0;

/// Validates a show and returns all findings.
pub fn validate(show: &Show) -> Report {
    let mut issues: Vec<Issue> = Vec::new();

    // --- settings -------------------------------------------------------
    if show.settings.video_fps == 0 {
        issues.push(err(
            "E_INVALID_FPS",
            "settings.video_fps must be greater than 0",
        ));
    }
    if show.settings.video_fps > 1000 {
        issues.push(warn(
            "W_UNUSUAL_FPS",
            format!("settings.video_fps {} is unusual", show.settings.video_fps),
        ));
    }
    if show.settings.sample_rate == 0 {
        issues.push(err(
            "E_INVALID_SAMPLE_RATE",
            "settings.sample_rate must be greater than 0",
        ));
    }
    if show.settings.block_frames == 0 || show.settings.block_frames > 8192 {
        issues.push(err(
            "E_INVALID_BLOCK_FRAMES",
            format!(
                "settings.block_frames must be in 1..=8192 (got {})",
                show.settings.block_frames
            ),
        ));
    }

    // --- sources --------------------------------------------------------
    let mut source_ids = HashSet::new();
    for s in &show.sources {
        if !source_ids.insert(s.id.0.clone()) {
            issues.push(err(
                "E_DUPLICATE_SOURCE_ID",
                format!("duplicate source id '{}'", s.id),
            ));
        }
        if s.id.0.is_empty() {
            issues.push(err("E_EMPTY_SOURCE_ID", "source id must not be empty"));
        }
        if let crate::show::SourceKind::PlaybackAsset(asset) = &s.kind {
            if asset.0.is_empty() {
                issues.push(err(
                    "E_EMPTY_ASSET_ID",
                    format!("source '{}' has an empty asset reference", s.id),
                ));
            }
        }
    }

    // --- outputs --------------------------------------------------------
    let mut output_ids = HashSet::new();
    let mut has_program_video = false;
    let mut has_program_audio = false;
    for o in &show.outputs {
        if !output_ids.insert(o.id.0.clone()) {
            issues.push(err(
                "E_DUPLICATE_OUTPUT_ID",
                format!("duplicate output id '{}'", o.id),
            ));
        }
        if o.id.0.is_empty() {
            issues.push(err("E_EMPTY_OUTPUT_ID", "output id must not be empty"));
        }
        match o.kind {
            crate::show::OutputKind::ProgramVideo => has_program_video = true,
            crate::show::OutputKind::ProgramAudio => has_program_audio = true,
            crate::show::OutputKind::LightingUniverse if o.universe.is_none() => {
                issues.push(err(
                    "E_MISSING_UNIVERSE_NUMBER",
                    format!("lighting output '{}' must set `universe`", o.id),
                ));
            }
            _ => {}
        }
    }
    if show
        .sources
        .iter()
        .any(|s| s.kind == crate::show::SourceKind::LiveVideoInput)
        && !has_program_video
    {
        issues.push(warn(
            "W_NO_PROGRAM_VIDEO",
            "show has video sources but no program_video output",
        ));
    }
    if !show.buses.is_empty() && !has_program_audio {
        issues.push(warn(
            "W_NO_PROGRAM_AUDIO",
            "show has audio buses but no program_audio output",
        ));
    }

    // --- buses ----------------------------------------------------------
    let mut bus_ids = HashSet::new();
    for b in &show.buses {
        if !bus_ids.insert(b.id.0.clone()) {
            issues.push(err(
                "E_DUPLICATE_BUS_ID",
                format!("duplicate bus id '{}'", b.id),
            ));
        }
        if !output_ids.contains(b.output.0.as_str()) {
            issues.push(err(
                "E_BUS_UNKNOWN_OUTPUT",
                format!("bus '{}' feeds unknown output '{}'", b.id, b.output),
            ));
        }
        let mut patched = HashSet::new();
        for input in &b.inputs {
            if !source_ids.contains(input.source.0.as_str()) {
                issues.push(err(
                    "E_BUS_UNKNOWN_SOURCE",
                    format!("bus '{}' patches unknown source '{}'", b.id, input.source),
                ));
            }
            if !patched.insert(input.source.0.clone()) {
                issues.push(warn(
                    "W_BUS_DUPLICATE_PATCH",
                    format!(
                        "bus '{}' patches source '{}' more than once",
                        b.id, input.source
                    ),
                ));
            }
            if !(MIN_GAIN_DB..=MAX_GAIN_DB).contains(&input.gain_db) {
                issues.push(warn(
                    "W_GAIN_OUT_OF_RANGE",
                    format!(
                        "bus '{}' trim for '{}' is {} dB outside {MIN_GAIN_DB}..{MAX_GAIN_DB}",
                        b.id, input.source, input.gain_db
                    ),
                ));
            }
        }
    }

    // --- fixtures & scenes ----------------------------------------------
    let mut fixture_ids = HashSet::new();
    for f in &show.fixtures {
        if !fixture_ids.insert(f.id.0.clone()) {
            issues.push(err(
                "E_DUPLICATE_FIXTURE_ID",
                format!("duplicate fixture id '{}'", f.id),
            ));
        }
        let mut addresses = HashSet::new();
        for ch in &f.channels {
            if !is_valid_dmx_address(ch.address) {
                issues.push(err(
                    "E_INVALID_DMX_ADDRESS",
                    format!(
                        "fixture '{}' has invalid DMX address {} (must be 1..=512)",
                        f.id, ch.address
                    ),
                ));
            }
            if !addresses.insert(ch.address) {
                issues.push(err(
                    "E_DUPLICATE_DMX_ADDRESS",
                    format!("fixture '{}' patches address {} twice", f.id, ch.address),
                ));
            }
        }
    }

    let mut scene_ids = HashSet::new();
    for s in &show.scenes {
        if !scene_ids.insert(s.id.0.clone()) {
            issues.push(err(
                "E_DUPLICATE_SCENE_ID",
                format!("duplicate scene id '{}'", s.id),
            ));
        }
        let mut seen = HashSet::new();
        for (fid, values) in &s.values {
            if !fixture_ids.contains(fid.0.as_str()) {
                issues.push(err(
                    "E_SCENE_UNKNOWN_FIXTURE",
                    format!("scene '{}' references unknown fixture '{}'", s.id, fid),
                ));
            }
            if !seen.insert(fid.0.clone()) {
                issues.push(err(
                    "E_SCENE_DUPLICATE_FIXTURE",
                    format!("scene '{}' sets fixture '{}' twice", s.id, fid),
                ));
            }
            if let Some(f) = show.fixture(fid) {
                if values.0.len() != f.channels.len() {
                    issues.push(err(
                        "E_SCENE_CHANNEL_COUNT",
                        format!(
                            "scene '{}' sets {} values for fixture '{}' which has {} channels",
                            s.id,
                            values.0.len(),
                            fid,
                            f.channels.len()
                        ),
                    ));
                }
            }
        }
    }

    // --- cue stack -------------------------------------------------------
    let mut numbers = HashSet::new();
    let mut last_number: Option<u32> = None;
    if show.cue_stack.is_empty() {
        issues.push(warn("W_EMPTY_CUE_STACK", "the cue stack is empty"));
    }
    for cue in &show.cue_stack.cues {
        let cue_no = cue.number.0;
        let mut attach = |i: Issue| {
            issues.push(i.for_cue(cue_no));
        };
        if !numbers.insert(cue_no) {
            attach(err(
                "E_DUPLICATE_CUE_NUMBER",
                format!("duplicate cue number {cue_no}"),
            ));
        }
        if let Some(prev) = last_number {
            if cue_no <= prev {
                attach(warn(
                    "W_CUE_NUMBERS_NOT_MONOTONIC",
                    format!("cue {cue_no} does not follow cue {prev} in stack order"),
                ));
            }
        }
        last_number = Some(cue_no);

        if let Some(preview) = &cue.preview_source {
            if !source_ids.contains(preview.0.as_str()) {
                attach(err(
                    "E_CUE_UNKNOWN_SOURCE",
                    format!("cue references unknown source '{preview}'"),
                ));
            }
        }
        if let Some(t) = &cue.video_transition {
            if matches!(
                t,
                crate::cue::Transition::Fade { .. } | crate::cue::Transition::Wipe { .. }
            ) {
                let ms = t.duration().as_millis() as u64;
                if ms == 0 {
                    attach(warn(
                        "W_ZERO_TRANSITION",
                        "zero-length fade/wipe behaves as a cut",
                    ));
                } else if ms > MAX_TRANSITION_MS {
                    attach(warn(
                        "W_LONG_TRANSITION",
                        format!("transition of {ms} ms is unusually long"),
                    ));
                }
            }
        }
        for change in &cue.audio_changes {
            let instant_ramp = matches!(
                change,
                crate::cue::AudioChange::SetGain {
                    ramp: crate::cue::RampMode::Instant,
                    ..
                } | crate::cue::AudioChange::SetMute {
                    ramp: crate::cue::RampMode::Instant,
                    ..
                } | crate::cue::AudioChange::SetPan {
                    ramp: crate::cue::RampMode::Instant,
                    ..
                }
            );
            match change {
                crate::cue::AudioChange::SetGain {
                    source, gain_db, ..
                } => {
                    if !source_ids.contains(source.0.as_str()) {
                        attach(err(
                            "E_CUE_UNKNOWN_SOURCE",
                            format!("cue gain change targets unknown source '{source}'"),
                        ));
                    }
                    if !(*gain_db >= MIN_GAIN_DB && *gain_db <= MAX_GAIN_DB) {
                        attach(warn(
                            "W_GAIN_OUT_OF_RANGE",
                            format!(
                                "cue sets gain {gain_db} dB outside {MIN_GAIN_DB}..{MAX_GAIN_DB}"
                            ),
                        ));
                    }
                }
                crate::cue::AudioChange::SetMute { source, .. } => {
                    if !source_ids.contains(source.0.as_str()) {
                        attach(err(
                            "E_CUE_UNKNOWN_SOURCE",
                            format!("cue mute change targets unknown source '{source}'"),
                        ));
                    }
                }
                crate::cue::AudioChange::SetPan { source, pan, .. } => {
                    if !source_ids.contains(source.0.as_str()) {
                        attach(err(
                            "E_CUE_UNKNOWN_SOURCE",
                            format!("cue pan change targets unknown source '{source}'"),
                        ));
                    }
                    if !(-1.0..=1.0).contains(pan) {
                        attach(warn(
                            "W_PAN_OUT_OF_RANGE",
                            format!("cue sets pan {pan} outside -1.0..1.0 (it will be clamped)"),
                        ));
                    }
                }
                crate::cue::AudioChange::BusAssign { source, bus } => {
                    if !source_ids.contains(source.0.as_str()) {
                        attach(err(
                            "E_CUE_UNKNOWN_SOURCE",
                            format!("cue bus assignment targets unknown source '{source}'"),
                        ));
                    }
                    if let Some(bus) = bus {
                        if !bus_ids.contains(bus.0.as_str()) {
                            attach(err(
                                "E_CUE_UNKNOWN_BUS",
                                format!("cue assigns to unknown bus '{bus}'"),
                            ));
                        }
                    }
                }
            }
            if instant_ramp {
                attach(warn(
                    "W_INSTANT_RAMP",
                    format!(
                        "cue uses an instant (non-ramped) audio change on '{}'; this can click",
                        change.source()
                    ),
                ));
            }
        }
        if let Some(scene) = &cue.lighting_scene {
            if !scene_ids.contains(scene.0.as_str()) {
                attach(err(
                    "E_CUE_UNKNOWN_SCENE",
                    format!("cue recalls unknown lighting scene '{scene}'"),
                ));
            }
        }
        if cue.video_transition.is_none()
            && cue.audio_changes.is_empty()
            && cue.lighting_scene.is_none()
        {
            attach(warn(
                "W_EMPTY_CUE",
                "cue has no video, audio, or lighting actions",
            ));
        }
        if matches!(cue.advance, crate::cue::AdvanceMode::Timed { after_ms: 0 }) {
            attach(warn(
                "W_ZERO_TIMED_ADVANCE",
                "timed advance of 0 ms behaves like follow",
            ));
        }
    }

    issues.sort_by_key(|i| match i.severity {
        Severity::Error => 0,
        Severity::Warning => 1,
    });
    Report { issues }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{AdvanceMode, AudioChange, Cue, CueNumber, RampMode, Transition};
    use crate::ids::{CueId, LightingSceneId, OutputId, SourceId};
    use crate::show::{
        AudioBus, ChannelGain, OperationMode, Output, OutputKind, Source, SourceKind,
    };
    use std::time::Duration;

    fn base_show() -> Show {
        let mut show = Show::new("test", "Test Show");
        show.outputs = vec![
            Output {
                id: OutputId::new("pgm_video"),
                kind: OutputKind::ProgramVideo,
                label: "PGM".into(),
                universe: None,
            },
            Output {
                id: OutputId::new("pa"),
                kind: OutputKind::ProgramAudio,
                label: "PA".into(),
                universe: None,
            },
        ];
        show.sources = vec![
            Source {
                id: SourceId::new("cam1"),
                kind: SourceKind::LiveVideoInput,
                label: "Cam 1".into(),
            },
            Source {
                id: SourceId::new("mic1"),
                kind: SourceKind::LiveAudioInput,
                label: "Mic 1".into(),
            },
        ];
        show.buses = vec![AudioBus {
            id: crate::ids::BusId::new("program"),
            label: "Program".into(),
            inputs: vec![ChannelGain {
                source: SourceId::new("mic1"),
                gain_db: 0.0,
            }],
            output: OutputId::new("pa"),
        }];
        show
    }

    #[test]
    fn clean_show_validates_ok() {
        let mut show = base_show();
        let mut cue = Cue::new(1, "open");
        cue.video_transition = Some(Transition::Cut);
        cue.preview_source = Some(SourceId::new("cam1"));
        show.cue_stack.push(cue);
        let report = validate(&show);
        assert!(report.is_ok(), "unexpected issues: {:?}", report.issues);
        assert!(!report.has_warnings());
    }

    #[test]
    fn duplicate_and_unknown_references_are_errors() {
        let mut show = base_show();
        show.sources.push(Source {
            id: SourceId::new("cam1"),
            kind: SourceKind::LiveVideoInput,
            label: "dup".into(),
        });
        let mut cue = Cue::new(1, "broken");
        cue.preview_source = Some(SourceId::new("ghost_cam"));
        cue.audio_changes = vec![AudioChange::SetMute {
            source: SourceId::new("ghost_mic"),
            muted: true,
            ramp: RampMode::Smooth,
        }];
        cue.lighting_scene = Some(LightingSceneId::new("ghost_scene"));
        show.cue_stack.push(cue);
        let report = validate(&show);
        assert!(!report.is_ok());
        let codes: Vec<&str> = report.issues.iter().map(|i| i.code.as_str()).collect();
        assert!(codes.contains(&"E_DUPLICATE_SOURCE_ID"), "{codes:?}");
        assert!(codes.contains(&"E_CUE_UNKNOWN_SOURCE"), "{codes:?}");
        assert!(codes.contains(&"E_CUE_UNKNOWN_SCENE"), "{codes:?}");
    }

    #[test]
    fn boundary_and_malformed_values_warn_not_crash() {
        let mut show = base_show();
        let mut cue = Cue::new(1, "extremes");
        cue.video_transition = Some(Transition::Fade {
            duration: Duration::from_millis(0),
        });
        cue.audio_changes = vec![
            AudioChange::SetGain {
                source: SourceId::new("mic1"),
                gain_db: -500.0,
                ramp: RampMode::Instant,
            },
            AudioChange::SetPan {
                source: SourceId::new("mic1"),
                pan: 7.5,
                ramp: RampMode::Smooth,
            },
        ];
        show.cue_stack.push(cue);
        show.fixtures.push(crate::lighting::Fixture {
            id: crate::ids::FixtureId::new("f1"),
            label: "F1".into(),
            universe: crate::ids::UniverseId::new("1"),
            channels: vec![crate::lighting::DmxChannel {
                address: 0, // invalid: below range boundary
                role: "intensity".into(),
            }],
        });
        let report = validate(&show);
        // Out-of-range gains/pans/ramps are warnings (engine clamps), but an
        // invalid DMX address is a hard error.
        assert!(!report.is_ok());
        let codes: Vec<&str> = report.issues.iter().map(|i| i.code.as_str()).collect();
        assert!(codes.contains(&"W_ZERO_TRANSITION"), "{codes:?}");
        assert!(codes.contains(&"W_GAIN_OUT_OF_RANGE"), "{codes:?}");
        assert!(codes.contains(&"W_PAN_OUT_OF_RANGE"), "{codes:?}");
        assert!(codes.contains(&"W_INSTANT_RAMP"), "{codes:?}");
        assert!(codes.contains(&"E_INVALID_DMX_ADDRESS"), "{codes:?}");
    }

    #[test]
    fn duplicate_cue_numbers_are_errors_with_cue_context() {
        let mut show = base_show();
        show.cue_stack.push(Cue {
            id: CueId::new("a"),
            number: CueNumber(1),
            label: "one".into(),
            video_transition: None,
            preview_source: None,
            audio_changes: vec![],
            lighting_scene: None,
            advance: AdvanceMode::Manual,
        });
        show.cue_stack.push(Cue {
            id: CueId::new("b"),
            number: CueNumber(1),
            label: "one again".into(),
            video_transition: None,
            preview_source: None,
            audio_changes: vec![],
            lighting_scene: None,
            advance: AdvanceMode::Manual,
        });
        let report = validate(&show);
        let dup = report
            .issues
            .iter()
            .find(|i| i.code == "E_DUPLICATE_CUE_NUMBER")
            .expect("duplicate cue number issue");
        assert_eq!(dup.cue, Some(1));
    }

    #[test]
    fn empty_stack_warns() {
        let show = base_show();
        let report = validate(&show);
        assert!(report.is_ok());
        let codes: Vec<&str> = report.issues.iter().map(|i| i.code.as_str()).collect();
        assert!(codes.contains(&"W_EMPTY_CUE_STACK"), "{codes:?}");
    }

    #[test]
    fn mode_defaults_to_rehearsal() {
        let show = base_show();
        assert_eq!(show.mode, OperationMode::Rehearsal);
    }
}
