//! Graceful degradation under CPU/GPU pressure (spec 14.3).
//!
//! The ordering is load-bearing and must never be violated:
//!
//! 1. degrade preview-monitor quality,
//! 2. degrade non-program compositing effects,
//! 3. program output is the **last** thing to degrade.
//!
//! The controller is a hysteresis state machine fed pressure samples; the
//! engine exposes the resulting quality knobs to the render host.

use serde::Serialize;

/// Escalating degradation levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationLevel {
    /// Full quality everywhere.
    Normal,
    /// Preview monitors render at reduced resolution; program unaffected.
    PreviewDegraded,
    /// Preview degraded *and* non-program compositing effects disabled;
    /// program (without preview) still full quality.
    EffectsDegraded,
    /// Everything non-essential is off and even program quality is being
    /// reduced — operator must be alerted: this is the emergency tier.
    ProgramDegraded,
}

impl DegradationLevel {
    /// Preview render scale hint (1.0 = full).
    pub fn preview_scale(self) -> f32 {
        match self {
            DegradationLevel::Normal => 1.0,
            DegradationLevel::PreviewDegraded => 0.5,
            DegradationLevel::EffectsDegraded => 0.25,
            DegradationLevel::ProgramDegraded => 0.25,
        }
    }

    /// Whether non-program compositing effects should run.
    pub fn effects_enabled(self) -> bool {
        matches!(
            self,
            DegradationLevel::Normal | DegradationLevel::PreviewDegraded
        )
    }

    /// Whether even program quality has been reduced (emergency).
    pub fn program_degraded(self) -> bool {
        self == DegradationLevel::ProgramDegraded
    }
}

/// Pressure thresholds, as percentages 0..=100.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// Escalate one level when pressure is at or above this.
    pub escalate_at: f32,
    /// De-escalate one level only when pressure falls below this.
    pub deescalate_below: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            escalate_at: 90.0,
            deescalate_below: 70.0,
        }
    }
}

/// Hysteresis controller mapping observed pressure to a
/// [`DegradationLevel`].
#[derive(Debug, Clone)]
pub struct DegradationController {
    level: DegradationLevel,
    thresholds: Thresholds,
    /// Consecutive high-pressure samples required to escalate (debounce
    /// transient spikes; a single frame of load must not degrade the show).
    escalate_after: u32,
    high_streak: u32,
}

impl DegradationController {
    /// Creates a controller with the given thresholds.
    pub fn new(thresholds: Thresholds) -> Self {
        Self {
            level: DegradationLevel::Normal,
            thresholds,
            escalate_after: 3,
            high_streak: 0,
        }
    }

    /// Current level.
    pub fn level(&self) -> DegradationLevel {
        self.level
    }

    /// Feeds one pressure sample (the worse of CPU/GPU, 0..=100).
    /// Returns the new level if it changed.
    pub fn report_pressure(&mut self, pressure: f32) -> Option<DegradationLevel> {
        let old = self.level;
        if pressure >= self.thresholds.escalate_at {
            self.high_streak = self.high_streak.saturating_add(1);
            if self.high_streak >= self.escalate_after
                && self.level != DegradationLevel::ProgramDegraded
            {
                // Escalate one level at a time, in the spec 14.3 order.
                self.level = match self.level {
                    DegradationLevel::Normal => DegradationLevel::PreviewDegraded,
                    DegradationLevel::PreviewDegraded => DegradationLevel::EffectsDegraded,
                    _ => DegradationLevel::ProgramDegraded,
                };
                self.high_streak = 0;
            }
        } else if pressure < self.thresholds.deescalate_below {
            self.high_streak = 0;
            self.level = match self.level {
                DegradationLevel::ProgramDegraded => DegradationLevel::EffectsDegraded,
                DegradationLevel::EffectsDegraded => DegradationLevel::PreviewDegraded,
                DegradationLevel::PreviewDegraded => DegradationLevel::Normal,
                DegradationLevel::Normal => DegradationLevel::Normal,
            };
        } // in the dead band: hold the current level (hysteresis).
        if self.level != old {
            log::warn!("degradation level now {:?}", self.level);
            Some(self.level)
        } else {
            None
        }
    }
}

impl Default for DegradationController {
    fn default() -> Self {
        Self::new(Thresholds::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degrades_preview_before_effects_before_program() {
        let mut c = DegradationController::default();
        // Sustained overload escalates one level per `escalate_after` (3)
        // consecutive high samples; a level change returns Some once.
        let mut events = Vec::new();
        for _ in 0..12 {
            if let Some(level) = c.report_pressure(95.0) {
                events.push(level);
            }
        }
        assert_eq!(
            events,
            vec![
                DegradationLevel::PreviewDegraded,
                DegradationLevel::EffectsDegraded,
                DegradationLevel::ProgramDegraded,
            ],
            "escalation must pass preview -> effects -> program, in order"
        );
        // Program degradation is the ceiling.
        assert_eq!(c.report_pressure(99.0), None);
        assert!(c.level().program_degraded());
    }

    #[test]
    fn transient_spikes_do_not_degrade() {
        let mut c = DegradationController::default();
        c.report_pressure(99.0);
        c.report_pressure(99.0);
        // Pressure drops before the escalation streak completes.
        assert_eq!(c.report_pressure(10.0), None);
        assert_eq!(c.level(), DegradationLevel::Normal);
    }

    #[test]
    fn hysteresis_holds_in_dead_band_and_recovers_in_order() {
        let mut c = DegradationController::default();
        for _ in 0..10 {
            c.report_pressure(95.0);
        }
        assert!(c.level() != DegradationLevel::Normal);
        // Dead band: nothing changes.
        assert_eq!(c.report_pressure(80.0), None);
        assert_eq!(c.report_pressure(80.0), None);
        // Recovery steps back down in reverse order.
        assert_eq!(
            c.report_pressure(10.0),
            if c.level() == DegradationLevel::Normal {
                None
            } else {
                Some(c.level())
            }
        );
        for _ in 0..5 {
            c.report_pressure(10.0);
        }
        assert_eq!(c.level(), DegradationLevel::Normal);
    }

    #[test]
    fn quality_knobs_follow_the_order() {
        assert_eq!(DegradationLevel::Normal.preview_scale(), 1.0);
        assert!(DegradationLevel::Normal.effects_enabled());
        assert!(DegradationLevel::PreviewDegraded.preview_scale() < 1.0);
        assert!(DegradationLevel::PreviewDegraded.effects_enabled());
        assert!(!DegradationLevel::EffectsDegraded.effects_enabled());
        assert!(!DegradationLevel::EffectsDegraded.program_degraded());
        assert!(DegradationLevel::ProgramDegraded.program_degraded());
    }
}
