//! Show, sources, outputs, and audio buses (spec 6.1–6.3).

use crate::ids::{BusId, OutputId, ShowId, SourceId};
use serde::{Deserialize, Serialize};

/// Rehearsal vs Live operation (spec 3.3, 6.1).
///
/// The distinction is load-bearing: in [`OperationMode::Rehearsal`] no signal
/// may reach program video, program audio, lighting fixtures, or any other
/// audience-facing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OperationMode {
    /// Build and rehearse the show. Zero signal to live outputs.
    #[default]
    Rehearsal,
    /// On air. Engines drive real outputs.
    Live,
}

impl OperationMode {
    /// True when the engine is allowed to drive live outputs.
    pub fn is_live(self) -> bool {
        matches!(self, OperationMode::Live)
    }

    /// Human-readable label used by the UI MODE indicator (colour is applied
    /// by the UI on top of this text — never colour alone).
    pub fn label(self) -> &'static str {
        match self {
            OperationMode::Rehearsal => "REHEARSAL",
            OperationMode::Live => "LIVE",
        }
    }
}

/// The kind of signal a source provides (spec 6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// A live camera or video feed.
    LiveVideoInput,
    /// A live microphone or line input.
    LiveAudioInput,
    /// A pre-loaded playback asset (video clip or audio track).
    PlaybackAsset(#[serde(rename = "asset")] crate::ids::AssetId),
    /// A still graphic / overlay layer.
    Graphic,
}

/// An input to the show (spec 6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Stable identifier referenced by buses and cues.
    pub id: SourceId,
    /// What kind of signal this source carries.
    pub kind: SourceKind,
    /// Operator-facing name.
    pub label: String,
}

/// The kind of signal an output carries (spec 6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputKind {
    /// Main program video (what the audience sees).
    ProgramVideo,
    /// Auxiliary video (confidence monitor, IMAG feed, recording).
    AuxVideo,
    /// Main program audio (what the audience hears).
    ProgramAudio,
    /// Auxiliary audio (stage monitors, recording feed).
    AuxAudio,
    /// A lighting DMX universe.
    LightingUniverse,
}

/// An output of the show (spec 6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// Stable identifier referenced by buses and the lighting engine.
    pub id: OutputId,
    /// What kind of signal this output carries.
    pub kind: OutputKind,
    /// Operator-facing name.
    pub label: String,
    /// For [`OutputKind::LightingUniverse`]: the DMX universe number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe: Option<u16>,
}

/// Gain applied to a source when it enters a bus, in dB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelGain {
    /// The source being patched into the bus.
    pub source: SourceId,
    /// Static trim gain in dB (cue automation applies on top of this).
    #[serde(default)]
    pub gain_db: f64,
}

/// A mix bus: a set of patched input sources summed to one output (spec 6.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioBus {
    /// Stable identifier.
    pub id: BusId,
    /// Operator-facing name.
    pub label: String,
    /// Sources patched into this bus with static trim.
    #[serde(default)]
    pub inputs: Vec<ChannelGain>,
    /// The output this bus feeds.
    pub output: OutputId,
}

/// Engine-level settings carried in the show file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowSettings {
    /// Program video frame rate. Switch timing is frame-accurate against this.
    pub video_fps: u32,
    /// Audio sample rate in Hz.
    pub sample_rate: u32,
    /// Audio render block size in frames. The mixer never allocates within
    /// a block.
    pub block_frames: u32,
    /// Default de-click ramp time for cue-triggered gain/mute changes.
    pub ramp_ms: u64,
}

impl Default for ShowSettings {
    fn default() -> Self {
        Self {
            video_fps: 60,
            sample_rate: 48_000,
            block_frames: 128,
            ramp_ms: 25,
        }
    }
}

/// The complete show (spec 6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Show {
    /// Stable identifier.
    pub id: ShowId,
    /// Operator-facing show name.
    pub name: String,
    /// All input sources.
    pub sources: Vec<Source>,
    /// All outputs.
    pub outputs: Vec<Output>,
    /// All audio buses.
    pub buses: Vec<AudioBus>,
    /// All lighting fixtures (see [`crate::lighting`]).
    pub fixtures: Vec<crate::lighting::Fixture>,
    /// All lighting scenes (see [`crate::lighting`]).
    pub scenes: Vec<crate::lighting::LightingScene>,
    /// The cue stack driving the show (see [`crate::cue`]).
    pub cue_stack: crate::cue::CueStack,
    /// Rehearsal vs live.
    pub mode: OperationMode,
    /// Engine settings.
    pub settings: ShowSettings,
}

impl Show {
    /// Creates an empty show with the given id and name.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: crate::ids::ShowId::new(id),
            name: name.into(),
            sources: Vec::new(),
            outputs: Vec::new(),
            buses: Vec::new(),
            fixtures: Vec::new(),
            scenes: Vec::new(),
            cue_stack: crate::cue::CueStack::default(),
            mode: OperationMode::Rehearsal,
            settings: ShowSettings::default(),
        }
    }

    /// Looks up a source by id.
    pub fn source(&self, id: &SourceId) -> Option<&Source> {
        self.sources.iter().find(|s| &s.id == id)
    }

    /// Looks up an output by id.
    pub fn output(&self, id: &OutputId) -> Option<&Output> {
        self.outputs.iter().find(|o| &o.id == id)
    }

    /// Looks up a bus by id.
    pub fn bus(&self, id: &BusId) -> Option<&AudioBus> {
        self.buses.iter().find(|b| &b.id == id)
    }

    /// Looks up a fixture by id.
    pub fn fixture(&self, id: &crate::ids::FixtureId) -> Option<&crate::lighting::Fixture> {
        self.fixtures.iter().find(|f| &f.id == id)
    }

    /// Looks up a lighting scene by id.
    pub fn scene(
        &self,
        id: &crate::ids::LightingSceneId,
    ) -> Option<&crate::lighting::LightingScene> {
        self.scenes.iter().find(|s| &s.id == id)
    }

    /// Returns the first program-video output id, if any.
    pub fn program_video_output(&self) -> Option<&OutputId> {
        self.outputs
            .iter()
            .find(|o| o.kind == OutputKind::ProgramVideo)
            .map(|o| &o.id)
    }

    /// Returns the first program-audio output id, if any.
    pub fn program_audio_output(&self) -> Option<&OutputId> {
        self.outputs
            .iter()
            .find(|o| o.kind == OutputKind::ProgramAudio)
            .map(|o| &o.id)
    }
}
