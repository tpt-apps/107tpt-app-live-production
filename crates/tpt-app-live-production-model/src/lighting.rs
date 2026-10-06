//! Lighting fixtures and scenes (spec 6.4).

use crate::ids::{DmxAddress, FixtureId, LightingSceneId, UniverseId};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// One DMX channel belonging to a fixture: its absolute address in the
/// fixture's universe plus the role the channel plays (used for sane
/// defaults and UI hints).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmxChannel {
    /// Absolute DMX address (1..=512) within the fixture's universe.
    pub address: DmxAddress,
    /// Channel role, e.g. `"intensity"`, `"red"`, `"gobo"`. Free-form; MVP
    /// only needs intensity-aware failsafe blackout semantics.
    #[serde(default)]
    pub role: String,
}

impl DmxChannel {
    /// Convenience constructor for an intensity channel.
    pub fn intensity(address: DmxAddress) -> Self {
        Self {
            address,
            role: "intensity".to_string(),
        }
    }
}

/// A DMX fixture patched into a universe (spec 6.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixture {
    /// Stable identifier referenced by scenes.
    pub id: FixtureId,
    /// Operator-facing name.
    pub label: String,
    /// The universe this fixture is patched into.
    pub universe: UniverseId,
    /// The DMX channels this fixture occupies.
    pub channels: Vec<DmxChannel>,
}

/// Per-channel 8-bit values for one fixture, aligned with the fixture's
/// [`Fixture::channels`] ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelValues(pub Vec<u8>);

impl ChannelValues {
    /// Builds values from a list.
    pub fn new(values: impl Into<Vec<u8>>) -> Self {
        Self(values.into())
    }
}

/// A recallable lighting look (spec 6.4): absolute values per fixture plus a
/// fade time. MVP scope is scene recall only — no effects/chases (spec 9, 22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightingScene {
    /// Stable identifier referenced by cues.
    pub id: LightingSceneId,
    /// Operator-facing name.
    pub label: String,
    /// Absolute channel values per fixture.
    pub values: Vec<(FixtureId, ChannelValues)>,
    /// Fade time: how long the engine takes to ramp from the current state
    /// into this scene.
    #[serde(with = "crate::showfile::duration_ms")]
    pub fade: Duration,
}

impl LightingScene {
    /// Convenience constructor.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        values: Vec<(FixtureId, ChannelValues)>,
        fade: Duration,
    ) -> Self {
        Self {
            id: LightingSceneId::new(id),
            label: label.into(),
            values,
            fade,
        }
    }
}
