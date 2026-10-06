//! Input-loss failsafe policies (spec 14.1).
//!
//! A live show cannot tolerate a silent failure: losing an input triggers
//! an operator-configured, deterministic policy. Audio defaults to
//! **mute-on-error** — corrupted or glitched audio must never reach
//! program.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tpt_app_live_production_model::ids::SourceId;

/// What the program output does when a live video input is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum VideoFailsafePolicy {
    /// Hold the last good frame on air until the operator intervenes.
    FreezeLastFrame,
    /// Cut to a designated backup source.
    CutToBackup {
        /// The backup source id.
        backup: SourceId,
    },
    /// Cut to a "signal lost" slate source.
    Slate {
        /// The slate source id.
        slate: SourceId,
    },
}

/// Failsafe configuration (spec 14.1, 22).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FailsafeConfig {
    /// Video input-loss policy.
    pub video: VideoFailsafePolicy,
    /// Mute an audio source on signal loss rather than passing corruption
    /// to program. Default true.
    pub audio_mute_on_error: bool,
    /// How long an input may go silent before it is considered lost.
    pub input_timeout: Duration,
}

impl Default for FailsafeConfig {
    fn default() -> Self {
        Self {
            video: VideoFailsafePolicy::FreezeLastFrame,
            audio_mute_on_error: true,
            input_timeout: Duration::from_millis(500),
        }
    }
}

/// Per-input health tracking.
#[derive(Debug, Default, Clone)]
pub struct InputHealth {
    last_ok_ms: HashMap<SourceId, u64>,
    lost: HashSet<SourceId>,
    /// Sources muted by the failsafe (vs. by the operator) so recovery can
    /// report accurately.
    muted_by_failsafe: HashSet<SourceId>,
}

/// What input-loss detection decided, consumed by the engine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum InputLossDecision {
    /// Nothing to do.
    Healthy,
    /// This input just transitioned to lost; the engine must apply policy.
    Lost,
    /// The input was lost and remains lost.
    StillLost,
    /// A previously lost input just came back.
    Restored,
}

impl InputHealth {
    /// Records a healthy frame/sample arrival for `source`.
    pub fn report_ok(&mut self, source: &SourceId, now_ms: u64) -> InputLossDecision {
        let was_lost = self.lost.remove(source);
        self.last_ok_ms.insert(source.clone(), now_ms);
        if was_lost {
            self.muted_by_failsafe.remove(source);
            InputLossDecision::Restored
        } else {
            InputLossDecision::Healthy
        }
    }

    /// Evaluates whether `source` should now be considered lost.
    pub fn evaluate(
        &mut self,
        source: &SourceId,
        now_ms: u64,
        timeout: Duration,
    ) -> InputLossDecision {
        if self.lost.contains(source) {
            return InputLossDecision::StillLost;
        }
        let last = self.last_ok_ms.get(source).copied();
        let stale = match last {
            // Never-seen inputs are not "lost" — they are simply not
            // flowing yet (e.g. configured but not connected pre-show).
            None => false,
            Some(last_ms) => now_ms.saturating_sub(last_ms) > timeout.as_millis() as u64,
        };
        if stale {
            self.lost.insert(source.clone());
            InputLossDecision::Lost
        } else {
            InputLossDecision::Healthy
        }
    }

    /// True while the source is considered lost.
    pub fn is_lost(&self, source: &SourceId) -> bool {
        self.lost.contains(source)
    }

    /// Marks the source as muted by the failsafe (bookkeeping).
    pub fn mark_muted_by_failsafe(&mut self, source: &SourceId) {
        self.muted_by_failsafe.insert(source.clone());
    }

    /// All currently lost inputs.
    pub fn lost_inputs(&self) -> Vec<SourceId> {
        let mut v: Vec<SourceId> = self.lost.iter().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_freeze_and_mute() {
        let cfg = FailsafeConfig::default();
        assert_eq!(cfg.video, VideoFailsafePolicy::FreezeLastFrame);
        assert!(cfg.audio_mute_on_error);
    }

    #[test]
    fn input_becomes_lost_after_timeout() {
        let mut h = InputHealth::default();
        let cam = SourceId::new("cam1");
        h.report_ok(&cam, 0);
        // 400ms in with a 500ms timeout: healthy.
        assert_eq!(
            h.evaluate(&cam, 400, Duration::from_millis(500)),
            InputLossDecision::Healthy
        );
        // 600ms: lost.
        assert_eq!(
            h.evaluate(&cam, 600, Duration::from_millis(500)),
            InputLossDecision::Lost
        );
        // Still lost.
        assert_eq!(
            h.evaluate(&cam, 700, Duration::from_millis(500)),
            InputLossDecision::StillLost
        );
        // Recovery.
        assert_eq!(h.report_ok(&cam, 800), InputLossDecision::Restored);
        assert_eq!(
            h.evaluate(&cam, 900, Duration::from_millis(500)),
            InputLossDecision::Healthy
        );
    }

    #[test]
    fn never_seen_input_is_not_lost() {
        let mut h = InputHealth::default();
        let cam = SourceId::new("configured_but_not_connected");
        assert_eq!(
            h.evaluate(&cam, 60_000, Duration::from_millis(500)),
            InputLossDecision::Healthy
        );
    }

    #[test]
    fn lost_inputs_are_listed() {
        let mut h = InputHealth::default();
        let a = SourceId::new("a");
        let b = SourceId::new("b");
        h.report_ok(&a, 0);
        h.report_ok(&b, 0);
        let timeout = Duration::from_millis(100);
        let _ = h.evaluate(&a, 200, timeout);
        let _ = h.evaluate(&b, 200, timeout);
        assert_eq!(h.lost_inputs(), vec![a, b]);
    }
}
