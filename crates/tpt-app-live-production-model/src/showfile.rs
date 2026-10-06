//! Versioned, human-readable show-file format (`.tptshow`, TOML).
//!
//! Schema version 1. The file is a faithful, serde-friendly mirror of the
//! domain [`Show`]; conversions in both directions are lossless for
//! supported constructs. Durations are stored as integer milliseconds.
//!
//! ```toml
//! schema_version = 1
//! name = "Sunday Service"
//!
//! [settings]
//! video_fps = 60
//!
//! [[sources]]
//! id = "cam1"
//! kind = "live_video_input"
//! label = "Camera 1"
//!
//! [[cues]]
//! number = 1
//! label = "Cold open"
//! video = { transition = "cut", source = "cam1" }
//! ```

use crate::cue::{AdvanceMode, AudioChange, Cue, CueStack, Transition, WipePattern};
use crate::ids::{AssetId, CueId, FixtureId, LightingSceneId};
use crate::lighting::{ChannelValues, LightingScene};
use crate::show::{
    AudioBus, ChannelGain, OperationMode, Output, OutputKind, Show, Source, SourceKind,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Current schema version written by this build.
pub const SCHEMA_VERSION: u32 = 1;

/// Serde helpers: `Duration` as integer milliseconds.
pub mod duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    /// Serializes a duration as milliseconds.
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_millis().serialize(s)
    }

    /// Deserializes milliseconds into a duration.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

/// Errors raised while loading or saving a show file.
#[derive(Debug, thiserror::Error)]
pub enum ShowFileError {
    /// The file could not be read from disk.
    #[error("cannot read show file {path}: {source}")]
    Io {
        /// File path.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML or does not match the schema.
    #[error("invalid show file: {0}")]
    Parse(#[from] toml::de::Error),
    /// The file declares a schema version this build cannot read.
    #[error("unsupported schema_version {found} (this build reads version {supported})")]
    UnsupportedSchema {
        /// Version found in the file.
        found: u32,
        /// Highest version this build supports.
        supported: u32,
    },
    /// The file could not be written.
    #[error("cannot write show file {path}: {source}")]
    WriteIo {
        /// File path.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The show could not be represented in the on-disk schema.
    #[error("cannot encode show: {0}")]
    Encode(String),
}

/// File representation of a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFile {
    /// Source id.
    pub id: String,
    /// Source kind.
    pub kind: String,
    /// Label.
    #[serde(default)]
    pub label: String,
    /// Asset reference for `playback_asset` sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
}

/// File representation of an output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputFile {
    /// Output id.
    pub id: String,
    /// Output kind.
    pub kind: String,
    /// Label.
    #[serde(default)]
    pub label: String,
    /// DMX universe number for `lighting_universe` outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe: Option<u16>,
}

/// File representation of a bus input patch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelGainFile {
    /// Source id.
    pub source: String,
    /// Static trim gain in dB.
    #[serde(default)]
    pub gain_db: f64,
}

/// File representation of a bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioBusFile {
    /// Bus id.
    pub id: String,
    /// Label.
    #[serde(default)]
    pub label: String,
    /// Patched sources.
    #[serde(default)]
    pub inputs: Vec<ChannelGainFile>,
    /// Output this bus feeds.
    pub output: String,
}

/// File representation of a fixture channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DmxChannelFile {
    /// Absolute address (1..=512).
    pub address: u16,
    /// Role hint.
    #[serde(default)]
    pub role: String,
}

/// File representation of a fixture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureFile {
    /// Fixture id.
    pub id: String,
    /// Label.
    #[serde(default)]
    pub label: String,
    /// Universe id.
    pub universe: String,
    /// Patched channels.
    #[serde(default)]
    pub channels: Vec<DmxChannelFile>,
}

/// File representation of a scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneFile {
    /// Scene id.
    pub id: String,
    /// Label.
    #[serde(default)]
    pub label: String,
    /// Fade time in milliseconds.
    #[serde(default)]
    pub fade_ms: u64,
    /// Absolute values per fixture.
    #[serde(default)]
    pub values: Vec<SceneValuesFile>,
}

/// Per-fixture values inside a scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneValuesFile {
    /// Fixture id.
    pub fixture: String,
    /// Channel values, aligned with the fixture's channel list.
    pub channels: Vec<u8>,
}

/// File representation of a video transition inside a cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoTransitionFile {
    /// `cut` | `fade` | `wipe`.
    pub transition: String,
    /// Source to preview before the transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Fade/wipe duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Wipe pattern for wipes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// File representation of one audio change inside a cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioChangeFile {
    /// `set_gain` | `set_mute` | `set_pan` | `bus_assign`.
    pub action: String,
    /// Target source.
    pub source: String,
    /// Gain in dB (set_gain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f64>,
    /// Mute state (set_mute).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    /// Pan -1.0..1.0 (set_pan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<f64>,
    /// Target bus (bus_assign); null removes from all buses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
    /// `smooth` (default) | `smooth_over` | `instant`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp: Option<String>,
    /// Ramp duration for `smooth_over`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp_ms: Option<u64>,
}

/// File representation of an advance mode inside a cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvanceFile {
    /// `manual` | `timed` | `follow`.
    pub mode: String,
    /// Delay for `timed`, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_ms: Option<u64>,
}

/// File representation of a cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CueFile {
    /// Cue number.
    pub number: u32,
    /// Label.
    #[serde(default)]
    pub label: String,
    /// Optional cue id (generated from the number when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Video transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoTransitionFile>,
    /// Audio changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioChangeFile>,
    /// Lighting scene id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<String>,
    /// Advance mode; defaults to manual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance: Option<AdvanceFile>,
}

/// File representation of engine settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsFile {
    /// Program frame rate.
    pub video_fps: u32,
    /// Audio sample rate.
    pub sample_rate: u32,
    /// Audio render block size.
    pub block_frames: u32,
    /// Default de-click ramp in milliseconds.
    pub ramp_ms: u64,
}

impl Default for SettingsFile {
    fn default() -> Self {
        let s = crate::show::ShowSettings::default();
        Self {
            video_fps: s.video_fps,
            sample_rate: s.sample_rate,
            block_frames: s.block_frames,
            ramp_ms: s.ramp_ms,
        }
    }
}

/// The on-disk show file (schema version 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShowFile {
    /// Schema version; must be `<=` [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Show name.
    pub name: String,
    /// Optional show id (derived from the file stem when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Engine settings.
    #[serde(default)]
    pub settings: SettingsFile,
    /// Sources.
    #[serde(default)]
    pub sources: Vec<SourceFile>,
    /// Outputs.
    #[serde(default)]
    pub outputs: Vec<OutputFile>,
    /// Audio buses.
    #[serde(default)]
    pub buses: Vec<AudioBusFile>,
    /// Fixtures.
    #[serde(default)]
    pub fixtures: Vec<FixtureFile>,
    /// Lighting scenes.
    #[serde(default)]
    pub scenes: Vec<SceneFile>,
    /// Cues.
    #[serde(default)]
    pub cues: Vec<CueFile>,
}

impl Default for ShowFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: "Untitled Show".to_string(),
            id: None,
            settings: SettingsFile::default(),
            sources: Vec::new(),
            outputs: Vec::new(),
            buses: Vec::new(),
            fixtures: Vec::new(),
            scenes: Vec::new(),
            cues: Vec::new(),
        }
    }
}

impl ShowFile {
    /// Parses a show file from a TOML string, checking the schema version.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_src: &str) -> Result<Self, ShowFileError> {
        let file: ShowFile = toml::from_str(toml_src)?;
        if file.schema_version > SCHEMA_VERSION {
            return Err(ShowFileError::UnsupportedSchema {
                found: file.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(file)
    }

    /// Loads and parses a show file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ShowFileError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ShowFileError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_str(&text)
    }

    /// Serializes to pretty TOML.
    pub fn to_toml(&self) -> Result<String, ShowFileError> {
        toml::to_string_pretty(self).map_err(|e| ShowFileError::Encode(e.to_string()))
    }

    /// Writes the show file to disk.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ShowFileError> {
        let path = path.as_ref();
        let text = self.to_toml()?;
        std::fs::write(path, text).map_err(|source| ShowFileError::WriteIo {
            path: path.display().to_string(),
            source,
        })
    }
}

fn parse_source_kind(kind: &str, asset: Option<String>) -> Result<SourceKind, ShowFileError> {
    match kind {
        "live_video_input" => Ok(SourceKind::LiveVideoInput),
        "live_audio_input" => Ok(SourceKind::LiveAudioInput),
        "playback_asset" => {
            let asset = asset.ok_or_else(|| {
                ShowFileError::Encode(
                    "source kind 'playback_asset' requires an `asset` field".to_string(),
                )
            })?;
            Ok(SourceKind::PlaybackAsset(AssetId::new(asset)))
        }
        "graphic" => Ok(SourceKind::Graphic),
        other => Err(ShowFileError::Encode(format!(
            "unknown source kind '{other}'"
        ))),
    }
}

fn source_kind_to_string(kind: &SourceKind) -> (&'static str, Option<String>) {
    match kind {
        SourceKind::LiveVideoInput => ("live_video_input", None),
        SourceKind::LiveAudioInput => ("live_audio_input", None),
        SourceKind::PlaybackAsset(a) => ("playback_asset", Some(a.0.clone())),
        SourceKind::Graphic => ("graphic", None),
    }
}

fn parse_output_kind(kind: &str) -> Result<OutputKind, ShowFileError> {
    match kind {
        "program_video" => Ok(OutputKind::ProgramVideo),
        "aux_video" => Ok(OutputKind::AuxVideo),
        "program_audio" => Ok(OutputKind::ProgramAudio),
        "aux_audio" => Ok(OutputKind::AuxAudio),
        "lighting_universe" => Ok(OutputKind::LightingUniverse),
        other => Err(ShowFileError::Encode(format!(
            "unknown output kind '{other}'"
        ))),
    }
}

fn output_kind_to_string(kind: &OutputKind) -> &'static str {
    match kind {
        OutputKind::ProgramVideo => "program_video",
        OutputKind::AuxVideo => "aux_video",
        OutputKind::ProgramAudio => "program_audio",
        OutputKind::AuxAudio => "aux_audio",
        OutputKind::LightingUniverse => "lighting_universe",
    }
}

fn parse_transition(v: &VideoTransitionFile) -> Result<Option<Transition>, ShowFileError> {
    let transition = match v.transition.as_str() {
        "cut" => Transition::Cut,
        "fade" => Transition::Fade {
            duration: Duration::from_millis(v.duration_ms.unwrap_or(500)),
        },
        "wipe" => {
            let pattern = match v.pattern.as_deref().unwrap_or("linear") {
                "linear" => WipePattern::Linear,
                "box" => WipePattern::Box,
                "iris" => WipePattern::Iris,
                other => {
                    return Err(ShowFileError::Encode(format!(
                        "unknown wipe pattern '{other}'"
                    )))
                }
            };
            Transition::Wipe {
                pattern,
                duration: Duration::from_millis(v.duration_ms.unwrap_or(500)),
            }
        }
        other => {
            return Err(ShowFileError::Encode(format!(
                "unknown transition '{other}'"
            )))
        }
    };
    Ok(Some(transition))
}

fn transition_to_file(t: &Transition, preview: Option<&str>) -> VideoTransitionFile {
    let (transition, duration_ms, pattern) = match t {
        Transition::Cut => ("cut", None, None),
        Transition::Fade { duration } => ("fade", Some(duration.as_millis() as u64), None),
        Transition::Wipe { pattern, duration } => (
            "wipe",
            Some(duration.as_millis() as u64),
            Some(
                match pattern {
                    WipePattern::Linear => "linear",
                    WipePattern::Box => "box",
                    WipePattern::Iris => "iris",
                }
                .to_string(),
            ),
        ),
    };
    VideoTransitionFile {
        transition: transition.to_string(),
        source: preview.map(|s| s.to_string()),
        duration_ms,
        pattern,
    }
}

fn parse_ramp(
    ramp: &Option<String>,
    ramp_ms: &Option<u64>,
) -> Result<crate::cue::RampMode, ShowFileError> {
    match (ramp.as_deref(), ramp_ms) {
        (None | Some("smooth"), _) => Ok(crate::cue::RampMode::Smooth),
        (Some("smooth_over"), Some(ms)) => Ok(crate::cue::RampMode::SmoothOver { ramp_ms: *ms }),
        (Some("smooth_over"), None) => Err(ShowFileError::Encode(
            "ramp 'smooth_over' requires ramp_ms".to_string(),
        )),
        (Some("instant"), _) => Ok(crate::cue::RampMode::Instant),
        (Some(other), _) => Err(ShowFileError::Encode(format!(
            "unknown ramp mode '{other}'"
        ))),
    }
}

fn ramp_to_file(ramp: &crate::cue::RampMode) -> (Option<String>, Option<u64>) {
    match ramp {
        crate::cue::RampMode::Smooth => (None, None),
        crate::cue::RampMode::SmoothOver { ramp_ms } => {
            (Some("smooth_over".to_string()), Some(*ramp_ms))
        }
        crate::cue::RampMode::Instant => (Some("instant".to_string()), None),
    }
}

fn parse_audio_change(a: &AudioChangeFile) -> Result<AudioChange, ShowFileError> {
    let ramp = parse_ramp(&a.ramp, &a.ramp_ms)?;
    let source = crate::ids::SourceId::new(a.source.clone());
    let change = match a.action.as_str() {
        "set_gain" => AudioChange::SetGain {
            source,
            gain_db: a.gain_db.unwrap_or(0.0),
            ramp,
        },
        "set_mute" => AudioChange::SetMute {
            source,
            muted: a.muted.unwrap_or(true),
            ramp,
        },
        "set_pan" => AudioChange::SetPan {
            source,
            pan: a.pan.unwrap_or(0.0),
            ramp,
        },
        "bus_assign" => AudioChange::BusAssign {
            source,
            bus: a.bus.clone().map(crate::ids::BusId::new),
        },
        other => {
            return Err(ShowFileError::Encode(format!(
                "unknown audio action '{other}'"
            )))
        }
    };
    Ok(change)
}

fn audio_change_to_file(a: &AudioChange) -> AudioChangeFile {
    let (action, source, gain_db, muted, pan, bus) = match a {
        AudioChange::SetGain {
            source, gain_db, ..
        } => ("set_gain", source, Some(*gain_db), None, None, None),
        AudioChange::SetMute { source, muted, .. } => {
            ("set_mute", source, None, Some(*muted), None, None)
        }
        AudioChange::SetPan { source, pan, .. } => {
            ("set_pan", source, None, None, Some(*pan), None)
        }
        AudioChange::BusAssign { source, bus } => (
            "bus_assign",
            source,
            None,
            None,
            None,
            bus.as_ref().map(|b| b.0.clone()),
        ),
    };
    let ramp_field = match a {
        AudioChange::SetGain { ramp, .. }
        | AudioChange::SetMute { ramp, .. }
        | AudioChange::SetPan { ramp, .. } => Some(ramp),
        AudioChange::BusAssign { .. } => None,
    };
    let (ramp, ramp_ms) = ramp_field.map(ramp_to_file).unwrap_or((None, None));
    AudioChangeFile {
        action: action.to_string(),
        source: source.0.clone(),
        gain_db,
        muted,
        pan,
        bus,
        ramp,
        ramp_ms,
    }
}

fn parse_advance(a: &Option<AdvanceFile>) -> Result<AdvanceMode, ShowFileError> {
    match a {
        None => Ok(AdvanceMode::Manual),
        Some(f) => match f.mode.as_str() {
            "manual" => Ok(AdvanceMode::Manual),
            "follow" => Ok(AdvanceMode::Follow),
            "timed" => Ok(AdvanceMode::Timed {
                after_ms: f.after_ms.unwrap_or(0),
            }),
            other => Err(ShowFileError::Encode(format!(
                "unknown advance mode '{other}'"
            ))),
        },
    }
}

fn advance_to_file(a: &AdvanceMode) -> Option<AdvanceFile> {
    match a {
        AdvanceMode::Manual => None,
        AdvanceMode::Follow => Some(AdvanceFile {
            mode: "follow".to_string(),
            after_ms: None,
        }),
        AdvanceMode::Timed { after_ms } => Some(AdvanceFile {
            mode: "timed".to_string(),
            after_ms: Some(*after_ms),
        }),
    }
}

impl From<&Show> for ShowFile {
    fn from(show: &Show) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: show.name.clone(),
            id: Some(show.id.0.clone()),
            settings: SettingsFile {
                video_fps: show.settings.video_fps,
                sample_rate: show.settings.sample_rate,
                block_frames: show.settings.block_frames,
                ramp_ms: show.settings.ramp_ms,
            },
            sources: show
                .sources
                .iter()
                .map(|s| {
                    let (kind, asset) = source_kind_to_string(&s.kind);
                    SourceFile {
                        id: s.id.0.clone(),
                        kind: kind.to_string(),
                        label: s.label.clone(),
                        asset,
                    }
                })
                .collect(),
            outputs: show
                .outputs
                .iter()
                .map(|o| OutputFile {
                    id: o.id.0.clone(),
                    kind: output_kind_to_string(&o.kind).to_string(),
                    label: o.label.clone(),
                    universe: o.universe,
                })
                .collect(),
            buses: show
                .buses
                .iter()
                .map(|b| AudioBusFile {
                    id: b.id.0.clone(),
                    label: b.label.clone(),
                    inputs: b
                        .inputs
                        .iter()
                        .map(|i| ChannelGainFile {
                            source: i.source.0.clone(),
                            gain_db: i.gain_db,
                        })
                        .collect(),
                    output: b.output.0.clone(),
                })
                .collect(),
            fixtures: show
                .fixtures
                .iter()
                .map(|f| FixtureFile {
                    id: f.id.0.clone(),
                    label: f.label.clone(),
                    universe: f.universe.0.clone(),
                    channels: f
                        .channels
                        .iter()
                        .map(|c| DmxChannelFile {
                            address: c.address,
                            role: c.role.clone(),
                        })
                        .collect(),
                })
                .collect(),
            scenes: show
                .scenes
                .iter()
                .map(|s| SceneFile {
                    id: s.id.0.clone(),
                    label: s.label.clone(),
                    fade_ms: s.fade.as_millis() as u64,
                    values: s
                        .values
                        .iter()
                        .map(|(fid, vals)| SceneValuesFile {
                            fixture: fid.0.clone(),
                            channels: vals.0.clone(),
                        })
                        .collect(),
                })
                .collect(),
            cues: show
                .cue_stack
                .cues
                .iter()
                .map(|c| CueFile {
                    number: c.number.0,
                    label: c.label.clone(),
                    id: Some(c.id.0.clone()),
                    video: c.video_transition.as_ref().map(|t| {
                        transition_to_file(t, c.preview_source.as_ref().map(|s| s.0.as_str()))
                    }),
                    audio: c.audio_changes.iter().map(audio_change_to_file).collect(),
                    lighting: c.lighting_scene.as_ref().map(|s| s.0.clone()),
                    advance: advance_to_file(&c.advance),
                })
                .collect(),
        }
    }
}

impl TryFrom<ShowFile> for Show {
    type Error = ShowFileError;

    fn try_from(file: ShowFile) -> Result<Self, Self::Error> {
        if file.schema_version > SCHEMA_VERSION {
            return Err(ShowFileError::UnsupportedSchema {
                found: file.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        let mut sources = Vec::with_capacity(file.sources.len());
        for s in &file.sources {
            sources.push(Source {
                id: crate::ids::SourceId::new(s.id.clone()),
                kind: parse_source_kind(&s.kind, s.asset.clone())?,
                label: if s.label.is_empty() {
                    s.id.clone()
                } else {
                    s.label.clone()
                },
            });
        }
        let mut outputs = Vec::with_capacity(file.outputs.len());
        for o in &file.outputs {
            outputs.push(Output {
                id: crate::ids::OutputId::new(o.id.clone()),
                kind: parse_output_kind(&o.kind)?,
                label: if o.label.is_empty() {
                    o.id.clone()
                } else {
                    o.label.clone()
                },
                universe: o.universe,
            });
        }
        let mut buses = Vec::with_capacity(file.buses.len());
        for b in &file.buses {
            buses.push(AudioBus {
                id: crate::ids::BusId::new(b.id.clone()),
                label: if b.label.is_empty() {
                    b.id.clone()
                } else {
                    b.label.clone()
                },
                inputs: b
                    .inputs
                    .iter()
                    .map(|i| ChannelGain {
                        source: crate::ids::SourceId::new(i.source.clone()),
                        gain_db: i.gain_db,
                    })
                    .collect(),
                output: crate::ids::OutputId::new(b.output.clone()),
            });
        }
        let mut fixtures = Vec::with_capacity(file.fixtures.len());
        for f in &file.fixtures {
            fixtures.push(crate::lighting::Fixture {
                id: FixtureId::new(f.id.clone()),
                label: if f.label.is_empty() {
                    f.id.clone()
                } else {
                    f.label.clone()
                },
                universe: crate::ids::UniverseId::new(f.universe.clone()),
                channels: f
                    .channels
                    .iter()
                    .map(|c| crate::lighting::DmxChannel {
                        address: c.address,
                        role: c.role.clone(),
                    })
                    .collect(),
            });
        }
        let mut scenes = Vec::with_capacity(file.scenes.len());
        for s in &file.scenes {
            scenes.push(LightingScene {
                id: LightingSceneId::new(s.id.clone()),
                label: if s.label.is_empty() {
                    s.id.clone()
                } else {
                    s.label.clone()
                },
                values: s
                    .values
                    .iter()
                    .map(|v| {
                        (
                            FixtureId::new(v.fixture.clone()),
                            ChannelValues::new(v.channels.clone()),
                        )
                    })
                    .collect(),
                fade: Duration::from_millis(s.fade_ms),
            });
        }
        let mut cues = Vec::with_capacity(file.cues.len());
        for c in &file.cues {
            cues.push(Cue {
                id: c
                    .id
                    .clone()
                    .map(CueId::new)
                    .unwrap_or_else(|| CueId::new(format!("cue-{}", c.number))),
                number: crate::cue::CueNumber(c.number),
                label: if c.label.is_empty() {
                    format!("Cue {}", c.number)
                } else {
                    c.label.clone()
                },
                video_transition: match &c.video {
                    Some(v) => parse_transition(v)?,
                    None => None,
                },
                preview_source: c
                    .video
                    .as_ref()
                    .and_then(|v| v.source.clone())
                    .map(crate::ids::SourceId::new),
                audio_changes: c
                    .audio
                    .iter()
                    .map(parse_audio_change)
                    .collect::<Result<Vec<_>, _>>()?,
                lighting_scene: c.lighting.clone().map(LightingSceneId::new),
                advance: parse_advance(&c.advance)?,
            });
        }
        Ok(Show {
            id: crate::ids::ShowId::new(file.id.unwrap_or_else(|| file.name.to_snake_case())),
            name: file.name,
            sources,
            outputs,
            buses,
            fixtures,
            scenes,
            cue_stack: CueStack {
                cues,
                current_index: None,
            },
            mode: OperationMode::Rehearsal,
            settings: crate::show::ShowSettings {
                video_fps: file.settings.video_fps,
                sample_rate: file.settings.sample_rate,
                block_frames: file.settings.block_frames,
                ramp_ms: file.settings.ramp_ms,
            },
        })
    }
}

trait ToSnakeCase {
    fn to_snake_case(&self) -> String;
}

impl ToSnakeCase for String {
    fn to_snake_case(&self) -> String {
        let mut out = String::with_capacity(self.len());
        for (i, ch) in self.chars().enumerate() {
            if ch.is_alphanumeric() {
                if ch.is_uppercase() && i > 0 {
                    out.push('_');
                }
                out.push(ch.to_ascii_lowercase());
            } else if !out.ends_with('-') {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // (helper removed — single replaces are inlined per test)

    const SAMPLE: &str = r#"
schema_version = 1
name = "Sunday Service"

[settings]
video_fps = 60
sample_rate = 48000
block_frames = 128
ramp_ms = 25

[[outputs]]
id = "pgm_video"
kind = "program_video"
label = "Program"

[[outputs]]
id = "pa"
kind = "program_audio"
label = "PA"

[[sources]]
id = "cam1"
kind = "live_video_input"
label = "Camera 1"

[[sources]]
id = "host_mic"
kind = "live_audio_input"
label = "Host mic"

[[buses]]
id = "program_bus"
label = "Program"
output = "pa"
inputs = [{ source = "host_mic", gain_db = -3.0 }]

[[fixtures]]
id = "wash1"
label = "Stage wash"
universe = "1"
channels = [{ address = 1, role = "intensity" }, { address = 2, role = "red" }]

[[scenes]]
id = "open_scene"
label = "Open"
fade_ms = 1500
values = [{ fixture = "wash1", channels = [255, 128] }]

[[cues]]
number = 1
label = "Cold open"
video = { transition = "cut", source = "cam1" }
audio = [{ action = "set_mute", source = "host_mic", muted = false }]

[[cues]]
number = 2
label = "Guest intro"
video = { transition = "fade", source = "cam2_placeholder", duration_ms = 800 }
lighting = "open_scene"
advance = { mode = "timed", after_ms = 5000 }
"#;

    #[test]
    fn valid_file_parses() {
        let file = ShowFile::from_str(SAMPLE).expect("parse");
        assert_eq!(file.schema_version, 1);
        assert_eq!(file.cues.len(), 2);
        assert_eq!(file.sources.len(), 2);
    }

    #[test]
    fn future_schema_is_rejected() {
        let future = SAMPLE.replace("schema_version = 1", "schema_version = 99");
        let err = ShowFile::from_str(&future).unwrap_err();
        assert!(matches!(
            err,
            ShowFileError::UnsupportedSchema {
                found: 99,
                supported: 1
            }
        ));
    }

    #[test]
    fn malformed_toml_is_rejected() {
        let err = ShowFile::from_str("this is not toml [[[").unwrap_err();
        assert!(matches!(err, ShowFileError::Parse(_)));
    }

    #[test]
    fn round_trips_through_domain_and_back() {
        let file = ShowFile::from_str(SAMPLE).expect("parse");
        let show = Show::try_from(file).expect("build");
        assert_eq!(show.name, "Sunday Service");
        assert_eq!(show.cue_stack.len(), 2);
        assert_eq!(show.mode, OperationMode::Rehearsal);

        let back = ShowFile::from(&show);
        let show2 = Show::try_from(back.clone()).expect("rebuild");
        assert_eq!(show, show2);

        // And the TOML text round-trips through parse as well.
        let text = back.to_toml().expect("encode");
        ShowFile::from_str(&text).expect("reparse");
    }

    #[test]
    fn playback_asset_requires_asset_field() {
        let bad = SAMPLE.replace(
            "kind = \"live_video_input\"\nlabel = \"Camera 1\"",
            "kind = \"playback_asset\"\nlabel = \"VT\"",
        );
        let file = ShowFile::from_str(&bad).expect("parses as schema");
        let err = Show::try_from(file).unwrap_err().to_string();
        assert!(err.contains("playback_asset"), "unexpected error: {err}");
    }

    #[test]
    fn empty_file_uses_defaults() {
        let file = ShowFile::from_str("schema_version = 1\nname = \"empty\"").expect("parse");
        let show = Show::try_from(file).expect("build");
        assert!(show.cue_stack.is_empty());
        assert_eq!(show.settings.video_fps, 60);
        assert_eq!(show.mode, OperationMode::Rehearsal);
    }
}
