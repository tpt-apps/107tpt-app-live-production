//! String-backed identifier newtypes.
//!
//! IDs are stable, human-chosen strings (`"cam1"`, `"pgm_video"`), because
//! show files are hand-editable and must survive round-trips. They serialize
//! as plain strings.

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Creates an identifier from a string.
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Borrows the identifier text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

string_id!(/// Identifies a show.
ShowId);
string_id!(/// Identifies a source (camera, mic, playback asset, graphic).
SourceId);
string_id!(/// Identifies an output (program/aux video, program/aux audio, lighting universe).
OutputId);
string_id!(/// Identifies an audio bus.
BusId);
string_id!(/// Identifies a lighting fixture.
FixtureId);
string_id!(/// Identifies a lighting scene.
LightingSceneId);
string_id!(/// Identifies a cue.
CueId);
string_id!(/// Identifies a pre-recorded playback asset.
AssetId);
string_id!(/// Identifies a DMX universe (e.g. `"1"` or `"stage-net"`).
UniverseId);

/// A DMX channel address, 1-based (1..=512). 0 is invalid.
pub type DmxAddress = u16;

/// Validates a DMX address range.
pub fn is_valid_dmx_address(addr: DmxAddress) -> bool {
    (1..=512).contains(&addr)
}
