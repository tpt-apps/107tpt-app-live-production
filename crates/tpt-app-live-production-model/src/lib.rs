//! Core domain model for TPT Live Production (spec section 6).
//!
//! This crate is the single source of truth for the show domain: sources,
//! outputs, audio buses, fixtures, lighting scenes, cues, and the cue stack.
//! It deliberately contains no engine logic, no I/O, and no threads — the
//! engine crates consume these types.
//!
//! The versioned, human-readable show-file representation lives in
//! [`showfile`]; [`validation`] holds the shared validation rules used by
//! both the application and the CLI.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cue;
pub mod ids;
pub mod lighting;
pub mod show;
pub mod showfile;
pub mod validation;

pub use cue::{
    AdvanceMode, AudioChange, Cue, CueNumber, CueStack, RampMode, Transition, WipePattern,
};
pub use ids::{
    AssetId, BusId, CueId, FixtureId, LightingSceneId, OutputId, ShowId, SourceId, UniverseId,
};
pub use lighting::{ChannelValues, DmxChannel, Fixture};
pub use show::{
    AudioBus, ChannelGain, OperationMode, Output, OutputKind, Show, ShowSettings, Source,
    SourceKind,
};
pub use showfile::ShowFileError;
