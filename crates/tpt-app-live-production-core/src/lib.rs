//! Real-time engine core for TPT Live Production.
//!
//! [`LiveEngine`] unifies the switcher, mixer, lighting engine, and cue
//! stack behind one clock and — critically — one gate: in
//! [`OperationMode::Rehearsal`] (spec 3.3) the engine computes internal
//! state but delivers **zero signal** to program video, program audio, or
//! DMX. Program outputs are [`FrameOutputs`] fields that are simply never
//! populated in rehearsal, so isolation is structural, not advisory.
//!
//! Module map:
//! - [`events`]: the engine event bus (UI, API, logs subscribe)
//! - [`failsafe`]: input-loss policies and audio mute-on-error (spec 14.1)
//! - [`degrade`]: graceful degradation ordering under load (spec 14.3)
//! - [`watchdog`]: heartbeat supervision and recovery (spec 14.2)
//! - [`session`]: embedded session log store (spec 18, JSONL)
//! - [`headless`]: clock-simulated rehearsal driver for the CLI (spec 16)
//! - [`api`] (feature `api`): optional localhost control API (spec 17)

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod degrade;
pub mod engine;
pub mod events;
pub mod failsafe;
pub mod headless;
pub mod session;
pub mod watchdog;

#[cfg(feature = "api")]
pub mod api;

pub use degrade::{DegradationController, DegradationLevel};
pub use engine::{EngineConfig, EngineError, FrameOutputs, LiveEngine, ShowState};
pub use events::{EngineEvent, EventBus};
pub use failsafe::{FailsafeConfig, VideoFailsafePolicy};
pub use session::{SessionEvent, SessionLog};
pub use watchdog::{Heartbeat, Watchdog, WatchdogConfig};
