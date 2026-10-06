//! Cues and the cue stack (spec 6.5, 6.6, 10).
//!
//! A cue is the product's central abstraction: one operator action that can
//! commit a video transition, change audio, and recall a lighting scene —
//! together, in sync.

use crate::ids::{CueId, LightingSceneId, SourceId};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Cue numbers are operator-facing and must be unique within a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CueNumber(pub u32);

/// A visual transition type (spec 6.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum Transition {
    /// Instant switch.
    Cut,
    /// Cross-fade from the current program source to the preview source.
    Fade {
        /// Fade duration.
        #[serde(with = "crate::showfile::duration_ms", rename = "duration")]
        duration: Duration,
    },
    /// Wipe transition with a basic pattern set.
    Wipe {
        /// Wipe shape.
        pattern: WipePattern,
        /// Wipe duration.
        #[serde(with = "crate::showfile::duration_ms", rename = "duration")]
        duration: Duration,
    },
}

/// Basic wipe patterns in MVP scope (spec 22: "beyond a basic set" is
/// explicitly out of scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WipePattern {
    /// Left-to-right linear wipe.
    Linear,
    /// Expanding rectangle.
    Box,
    /// Expanding circle.
    Iris,
}

impl Transition {
    /// Duration of the transition; a cut is zero-length.
    pub fn duration(&self) -> Duration {
        match self {
            Transition::Cut => Duration::ZERO,
            Transition::Fade { duration } | Transition::Wipe { duration, .. } => *duration,
        }
    }
}

/// How gain/mute changes are applied (spec 8: ramped, not stepped, unless
/// the operator explicitly requests an instant cut).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RampMode {
    /// Use the show's default de-click ramp time.
    #[default]
    Smooth,
    /// Ramp over an explicit duration.
    SmoothOver {
        /// Ramp duration in milliseconds.
        ramp_ms: u64,
    },
    /// Step immediately. Only appropriate when an operator explicitly asks
    /// for a hard cut (a validation warning is raised when a cue uses this).
    Instant,
}

impl RampMode {
    /// Resolves this mode against the show default ramp.
    pub fn duration(self, default_ramp: Duration) -> Duration {
        match self {
            RampMode::Smooth => default_ramp,
            RampMode::SmoothOver { ramp_ms } => Duration::from_millis(ramp_ms),
            RampMode::Instant => Duration::ZERO,
        }
    }
}

/// One audio change carried by a cue (spec 8: e.g. "cue 12 mutes source 3
/// and raises source 1").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AudioChange {
    /// Set a source's channel gain.
    SetGain {
        /// Target source.
        source: SourceId,
        /// New gain in dB.
        gain_db: f64,
        /// How to apply the change.
        #[serde(default)]
        ramp: RampMode,
    },
    /// Mute or unmute a source.
    SetMute {
        /// Target source.
        source: SourceId,
        /// New mute state.
        muted: bool,
        /// How to apply the change.
        #[serde(default)]
        ramp: RampMode,
    },
    /// Set a source's pan (-1.0 hard left .. 1.0 hard right).
    SetPan {
        /// Target source.
        source: SourceId,
        /// New pan position.
        pan: f64,
        /// How to apply the change.
        #[serde(default)]
        ramp: RampMode,
    },
    /// Move a source onto a bus (or remove it when `bus` is null).
    BusAssign {
        /// Target source.
        source: SourceId,
        /// Destination bus id, or null to remove from all buses.
        bus: Option<crate::ids::BusId>,
    },
}

impl AudioChange {
    /// The source this change targets.
    pub fn source(&self) -> &SourceId {
        match self {
            AudioChange::SetGain { source, .. }
            | AudioChange::SetMute { source, .. }
            | AudioChange::SetPan { source, .. }
            | AudioChange::BusAssign { source, .. } => source,
        }
    }
}

/// How a cue is triggered (spec 6.5, 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
#[derive(Default)]
pub enum AdvanceMode {
    /// Wait for the operator's GO.
    #[default]
    Manual,
    /// Advance automatically this long after the cue fires.
    Timed {
        /// Delay after firing, in milliseconds.
        after_ms: u64,
    },
    /// Advance automatically as soon as the previous cue's transition
    /// completes.
    Follow,
}

/// A cue (spec 6.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    /// Stable identifier.
    pub id: CueId,
    /// Operator-facing cue number (unique within the stack).
    pub number: CueNumber,
    /// Operator-facing label, e.g. `"Guest intro"`.
    pub label: String,
    /// Video transition to commit (preview → program).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_transition: Option<Transition>,
    /// Which source to put on preview before the transition runs. Absent
    /// means "use whatever is already on preview".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_source: Option<SourceId>,
    /// Audio changes applied when the cue fires.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_changes: Vec<AudioChange>,
    /// Lighting scene recalled when the cue fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting_scene: Option<LightingSceneId>,
    /// How the *next* cue is triggered after this one fires.
    #[serde(default)]
    pub advance: AdvanceMode,
}

impl Cue {
    /// Convenience constructor with generated id and defaults.
    pub fn new(number: u32, label: impl Into<String>) -> Self {
        Self {
            id: CueId::new(format!("cue-{number}")),
            number: CueNumber(number),
            label: label.into(),
            video_transition: None,
            preview_source: None,
            audio_changes: Vec::new(),
            lighting_scene: None,
            advance: AdvanceMode::Manual,
        }
    }

    /// The total time this cue occupies before the next can fire: its
    /// transition duration, or its timed-advance delay, whichever is longer.
    pub fn occupancy(&self) -> Duration {
        let transition = self
            .video_transition
            .as_ref()
            .map(|t| t.duration())
            .unwrap_or_default();
        let timed = match self.advance {
            AdvanceMode::Timed { after_ms } => Duration::from_millis(after_ms),
            _ => Duration::ZERO,
        };
        transition.max(timed)
    }
}

/// The cue stack (spec 6.5, 10).
///
/// `current_index` is `None` before the first GO. Editing operations
/// ([`CueStack::insert`], [`CueStack::remove`], [`CueStack::move_cue`],
/// [`CueStack::replace`]) are safe to call while a cue is live: they never
/// shift the currently-executing index — the live cue stays live and the
/// stack remains consistent (spec 10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CueStack {
    /// Ordered cues.
    pub cues: Vec<Cue>,
    /// Index of the cue that most recently fired; `None` before first GO.
    pub current_index: Option<usize>,
}

impl CueStack {
    /// True before the first GO has been pressed.
    pub fn is_before_first_go(&self) -> bool {
        self.current_index.is_none()
    }

    /// The cue that most recently fired.
    pub fn current(&self) -> Option<&Cue> {
        self.current_index.and_then(|i| self.cues.get(i))
    }

    /// The cue that the next GO will fire.
    pub fn next(&self) -> Option<&Cue> {
        match self.current_index {
            None => self.cues.first(),
            Some(i) => self.cues.get(i + 1),
        }
    }

    /// The index the next GO will fire, if any.
    pub fn next_index(&self) -> Option<usize> {
        match self.current_index {
            None if !self.cues.is_empty() => Some(0),
            Some(i) if i + 1 < self.cues.len() => Some(i + 1),
            _ => None,
        }
    }

    /// Marks `index` as fired (the GO action). Out-of-range indexes are
    /// rejected rather than panicking.
    pub fn mark_fired(&mut self, index: usize) -> Option<&Cue> {
        if index < self.cues.len() {
            self.current_index = Some(index);
            self.cues.get(index)
        } else {
            None
        }
    }

    /// Inserts a cue at `index`, keeping the live cue's index stable.
    ///
    /// Returns the index the cue actually landed at. Inserting at or before
    /// the live cue shifts the live pointer up so the *same cue* stays live.
    pub fn insert(&mut self, index: usize, cue: Cue) -> usize {
        let index = index.min(self.cues.len());
        self.cues.insert(index, cue);
        if let Some(current) = self.current_index {
            if index <= current {
                self.current_index = Some(current + 1);
            }
        }
        index
    }

    /// Appends a cue to the end of the stack.
    pub fn push(&mut self, cue: Cue) {
        self.cues.push(cue);
    }

    /// Removes the cue at `index`. Removing the live cue keeps it playing
    /// logically: `current_index` becomes `None` (stack rewinds to
    /// "before first GO") — the engine has already captured the live cue's
    /// actions.
    pub fn remove(&mut self, index: usize) -> Option<Cue> {
        if index >= self.cues.len() {
            return None;
        }
        let removed = self.cues.remove(index);
        if let Some(current) = self.current_index {
            match index.cmp(&current) {
                std::cmp::Ordering::Less => self.current_index = Some(current - 1),
                std::cmp::Ordering::Equal => self.current_index = None,
                std::cmp::Ordering::Greater => {}
            }
        }
        Some(removed)
    }

    /// Replaces the cue at `index` with `cue`. Replacing the live cue does
    /// not interrupt it — the change applies only if the same cue fires
    /// again (e.g. via a loop-back GO).
    pub fn replace(&mut self, index: usize, cue: Cue) -> Option<Cue> {
        if index >= self.cues.len() {
            return None;
        }
        let old = std::mem::replace(&mut self.cues[index], cue);
        Some(old)
    }

    /// Moves a cue from `from` to `to` (final position semantics, like a
    /// drag-and-drop reorder). The live cue stays live.
    pub fn move_cue(&mut self, from: usize, to: usize) -> Option<()> {
        if from >= self.cues.len() || self.cues.is_empty() {
            return None;
        }
        // Capture the live cue's identity first so its (possibly shifted)
        // position can be recomputed after the move.
        let live_id = self
            .current_index
            .and_then(|i| self.cues.get(i))
            .map(|c| c.id.clone());
        let to = to.min(self.cues.len() - 1);
        let cue = self.cues.remove(from);
        self.cues.insert(to, cue);
        if let Some(id) = live_id {
            if let Some(pos) = self.cues.iter().position(|c| c.id == id) {
                self.current_index = Some(pos);
            }
        }
        Some(())
    }

    /// Resets the stack to "before first GO".
    pub fn rewind(&mut self) {
        self.current_index = None;
    }

    /// Number of cues.
    pub fn len(&self) -> usize {
        self.cues.len()
    }

    /// True when the stack has no cues.
    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }
}
