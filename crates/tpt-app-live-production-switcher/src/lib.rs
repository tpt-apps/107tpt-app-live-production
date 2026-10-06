//! PVW/PGM video switcher (spec 7).
//!
//! The switcher is a pure, frame-indexed state machine: it decides *what*
//! each output shows on every frame; a render backend (GPU compositor or
//! test harness) executes the decision. Decoupling the decision from the
//! pixels is what makes frame-accurate switch timing testable without a GPU.
//!
//! Invariants (spec 7):
//! - selecting a new PVW source never affects PGM output,
//! - a transition commits the PVW source to PGM,
//! - switch timing is frame-accurate against the program frame rate,
//! - the last committed frame is retained for the failsafe
//!   "freeze last good frame" policy ([`Switcher::last_good_program`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashSet;
use std::time::Duration;

use serde::Serialize;
use tpt_app_live_production_model::cue::{Transition, WipePattern};
use tpt_app_live_production_model::ids::SourceId;

/// Switcher errors.
#[derive(Debug, thiserror::Error)]
pub enum SwitcherError {
    /// The referenced source was never registered.
    #[error("unknown source '{0}'")]
    UnknownSource(SourceId),
    /// The program frame rate must be greater than zero.
    #[error("frame rate must be greater than zero")]
    InvalidFrameRate,
    /// The transition duration quantizes to zero frames at the current
    /// frame rate; policy treats it as a cut.
    #[error("transition duration quantizes to zero frames; treated as a cut")]
    ZeroFrameTransition,
}

/// Maps wall-clock durations onto program frame indices.
///
/// All switch timing derives from this clock, so a cut or fade is exact in
/// *frames*, which is the unit the program output actually runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameClock {
    fps: u32,
}

impl FrameClock {
    /// Creates a clock for the given integer frame rate. `fps` must be
    /// non-zero.
    pub fn new(fps: u32) -> Result<Self, SwitcherError> {
        if fps == 0 {
            return Err(SwitcherError::InvalidFrameRate);
        }
        Ok(Self { fps })
    }

    /// Program frame rate.
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// Duration of one frame.
    pub fn frame_duration(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.fps))
    }

    /// Smallest number of frames that covers `duration` (a 0 duration is 0
    /// frames; anything above 0 is at least 1 frame).
    ///
    /// Computed as `ceil(duration x fps)` in exact integer arithmetic so
    /// e.g. 800 ms at 60 fps is exactly 48 frames (no truncation drift).
    pub fn frames_for(&self, duration: Duration) -> u64 {
        let scaled = duration.as_nanos() * u128::from(self.fps);
        scaled.div_ceil(1_000_000_000) as u64
    }
}

/// The transition kinds the engine executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Hard switch.
    Cut,
    /// Cross-fade.
    Fade,
    /// Wipe with a pattern.
    Wipe(WipePattern),
}

impl TransitionKind {
    fn of(transition: &Transition) -> Self {
        match transition {
            Transition::Cut => TransitionKind::Cut,
            Transition::Fade { .. } => TransitionKind::Fade,
            Transition::Wipe { pattern, .. } => TransitionKind::Wipe(*pattern),
        }
    }
}

/// Progress of the active transition for one frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransitionProgress {
    /// Which source program is transitioning away from.
    pub from: SourceId,
    /// Which source program is transitioning to (the committed PVW source).
    pub to: SourceId,
    /// 0.0..=1.0 position through the transition this frame.
    pub t: f64,
    /// Frame index (relative to transition start) of this frame.
    pub frame: u64,
    /// Total frames the transition spans.
    pub total_frames: u64,
    /// The transition kind.
    pub kind: TransitionKind,
}

/// What every video output should render on one frame.
///
/// The render backend composites: when `transition` is `Some`, program is a
/// blend of `transition.from` and `transition.to` at `t`; otherwise it shows
/// `program` alone. `overlay` composites on top when present.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchFrame {
    /// Source program shows this frame (post-transition target when idle).
    pub program: SourceId,
    /// Source preview shows this frame. Never affects `program` (spec 7).
    pub preview: SourceId,
    /// Transition progress, when one is active.
    pub transition: Option<TransitionProgress>,
    /// Overlay layer composited over program, when armed.
    pub overlay: Option<SourceId>,
    /// Absolute program frame index this plan applies to.
    pub frame_index: u64,
}

impl SwitchFrame {
    /// True when program is mid-transition on this frame.
    pub fn is_transitioning(&self) -> bool {
        self.transition.is_some()
    }
}

/// A single overlay layer (spec 7: "basic compositing (single overlay
/// layer)").
#[derive(Debug, Clone, PartialEq, Eq)]
struct Overlay {
    source: SourceId,
}

/// The PVW/PGM switcher state machine.
#[derive(Debug, Clone)]
pub struct Switcher {
    clock: FrameClock,
    sources: HashSet<SourceId>,
    preview: SourceId,
    program: SourceId,
    active: Option<ActiveTransition>,
    overlay: Option<Overlay>,
    /// Last frame index handed to [`Switcher::tick`].
    last_frame: Option<u64>,
    /// The most recent fully-good program source (for failsafe freeze).
    last_good_program: SourceId,
}

#[derive(Debug, Clone)]
struct ActiveTransition {
    kind: TransitionKind,
    from: SourceId,
    to: SourceId,
    start_frame: u64,
    total_frames: u64,
}

impl Switcher {
    /// Creates a switcher running at `fps`, with `initial_program` on
    /// program and preview.
    pub fn new(fps: u32, initial_program: SourceId) -> Result<Self, SwitcherError> {
        let clock = FrameClock::new(fps)?;
        Ok(Self {
            clock,
            sources: HashSet::new(),
            preview: initial_program.clone(),
            last_good_program: initial_program.clone(),
            program: initial_program,
            active: None,
            overlay: None,
            last_frame: None,
        })
    }

    /// The program frame rate clock.
    pub fn clock(&self) -> &FrameClock {
        &self.clock
    }

    /// Registers a source the switcher may select. Unknown sources are
    /// rejected on select/commit rather than panicking mid-show.
    pub fn register_source(&mut self, source: SourceId) {
        self.sources.insert(source);
    }

    /// Currently registered sources.
    pub fn sources(&self) -> &HashSet<SourceId> {
        &self.sources
    }

    /// Current preview source.
    pub fn preview(&self) -> &SourceId {
        &self.preview
    }

    /// Current program source (the committed target while a transition runs).
    pub fn program(&self) -> &SourceId {
        &self.program
    }

    /// True while a transition is in flight.
    pub fn is_transitioning(&self) -> bool {
        self.active.is_some()
    }

    /// Selects the PVW source. **Never affects program** (spec 7); this is
    /// the load-bearing invariant of the bus model.
    pub fn select_preview(&mut self, source: SourceId) -> Result<(), SwitcherError> {
        self.ensure_known(&source)?;
        self.preview = source;
        Ok(())
    }

    /// Arms or disarms the single overlay layer. `None` clears it.
    pub fn set_overlay(&mut self, source: Option<SourceId>) -> Result<(), SwitcherError> {
        match source {
            Some(s) => {
                self.ensure_known(&s)?;
                self.overlay = Some(Overlay { source: s });
            }
            None => self.overlay = None,
        }
        Ok(())
    }

    /// The active overlay source, if any.
    pub fn overlay(&self) -> Option<&SourceId> {
        self.overlay.as_ref().map(|o| &o.source)
    }

    /// Commits the current PVW source to PGM using `transition`.
    ///
    /// Taking again while a transition is active first *completes* the
    /// in-flight transition at its target (last-operator-action-wins, the
    /// behaviour physical switchers have), then starts the new transition.
    ///
    /// Returns the number of frames the transition will span (0 for a cut).
    pub fn take(
        &mut self,
        transition: &Transition,
        frame_index: u64,
    ) -> Result<u64, SwitcherError> {
        self.ensure_known(&self.preview)?;

        // Complete any in-flight transition instantly at its target.
        if let Some(active) = self.active.take() {
            self.program = active.to;
        }

        let total_frames = self.clock.frames_for(transition.duration());
        let kind = TransitionKind::of(transition);
        match kind {
            TransitionKind::Cut => {
                self.commit_preview_to_program();
                Ok(0)
            }
            _ => {
                if total_frames == 0 {
                    // A zero/negligible-duration fade is a cut by policy.
                    self.commit_preview_to_program();
                    return Err(SwitcherError::ZeroFrameTransition);
                }
                self.active = Some(ActiveTransition {
                    kind,
                    from: self.program.clone(),
                    to: self.preview.clone(),
                    start_frame: frame_index,
                    total_frames,
                });
                self.program = self.preview.clone();
                Ok(total_frames)
            }
        }
    }

    /// Advances the state machine to `frame_index` and returns the render
    /// plan for that frame.
    ///
    /// Frame indices must be non-decreasing; the engine drives this from the
    /// program frame clock. Allocation-free per call: use [`tick_into`] on
    /// a caller-owned frame on the per-frame render path.
    pub fn tick(&mut self, frame_index: u64) -> SwitchFrame {
        let mut out = SwitchFrame {
            program: self.program.clone(),
            preview: self.preview.clone(),
            transition: None,
            overlay: self.overlay.as_ref().map(|o| o.source.clone()),
            frame_index,
        };
        self.tick_into(frame_index, &mut out);
        out
    }

    /// Allocation-free steady-state variant of [`tick`]: updates `out`
    /// in place (internally via `clone_from`, which reuses the existing
    /// string allocations). First use of each field allocates; every
    /// subsequent call on the same `out` reuses those allocations, so the
    /// per-frame render path allocates nothing (spec 3.1).
    pub fn tick_into(&mut self, frame_index: u64, out: &mut SwitchFrame) {
        if let Some(last) = self.last_frame {
            debug_assert!(frame_index >= last, "switcher time went backwards");
        }
        self.last_frame = Some(frame_index);

        enum Phase {
            Idle,
            Completed,
            InFlight { t: f64, elapsed: u64 },
        }
        let phase = match &self.active {
            None => Phase::Idle,
            Some(active) => {
                let elapsed = frame_index.saturating_sub(active.start_frame);
                if elapsed >= active.total_frames {
                    Phase::Completed
                } else {
                    Phase::InFlight {
                        t: ((elapsed + 1) as f64 / active.total_frames as f64).min(1.0),
                        elapsed,
                    }
                }
            }
        };
        if let Phase::Completed = phase {
            self.active = None;
            self.last_good_program.clone_from(&self.program);
        }

        out.program.clone_from(&self.program);
        out.preview.clone_from(&self.preview);
        out.frame_index = frame_index;
        match (&self.overlay, out.overlay.as_mut()) {
            (Some(o), Some(prev)) => prev.clone_from(&o.source),
            (Some(o), None) => out.overlay = Some(o.source.clone()),
            (None, Some(_)) => out.overlay = None,
            (None, None) => {}
        }
        // Build the progress from `self.active` without cloning it.
        match (&self.active, &phase, out.transition.as_mut()) {
            (Some(active), Phase::InFlight { t, elapsed }, Some(prev)) => {
                prev.from.clone_from(&active.from);
                prev.to.clone_from(&active.to);
                prev.t = *t;
                prev.frame = *elapsed;
                prev.total_frames = active.total_frames;
                prev.kind = active.kind;
            }
            (Some(active), Phase::InFlight { t, elapsed }, None) => {
                out.transition = Some(TransitionProgress {
                    from: active.from.clone(),
                    to: active.to.clone(),
                    t: *t,
                    frame: *elapsed,
                    total_frames: active.total_frames,
                    kind: active.kind,
                });
            }
            _ => out.transition = None,
        }
    }

    /// Commits preview to program, reusing existing allocations.
    fn commit_preview_to_program(&mut self) {
        self.program.clone_from(&self.preview);
        self.last_good_program.clone_from(&self.program);
    }

    /// The last source that reached program cleanly — the frame the
    /// failsafe "freeze last good frame" policy (spec 14.1) holds on air.
    pub fn last_good_program(&self) -> &SourceId {
        &self.last_good_program
    }

    fn ensure_known(&self, source: &SourceId) -> Result<(), SwitcherError> {
        // An unregistered source is a configuration bug; reject it instead
        // of blacking the program output mid-show.
        if self.sources.contains(source) {
            Ok(())
        } else {
            Err(SwitcherError::UnknownSource(source.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sid(name: &str) -> SourceId {
        SourceId::new(name)
    }

    fn switcher() -> Switcher {
        let mut s = Switcher::new(60, sid("cam1")).unwrap();
        for name in ["cam1", "cam2", "cam3", "vt", "slate"] {
            s.register_source(sid(name));
        }
        s
    }

    // ---- valid input ----------------------------------------------------

    #[test]
    fn preview_selection_never_affects_program() {
        let mut s = switcher();
        assert_eq!(s.program(), &sid("cam1"));
        s.select_preview(sid("cam2")).unwrap();
        let frame = s.tick(0);
        assert_eq!(frame.program, sid("cam1"), "PVW select must not touch PGM");
        assert_eq!(frame.preview, sid("cam2"));
    }

    #[test]
    fn cut_commits_preview_to_program_on_the_same_frame() {
        let mut s = switcher();
        s.select_preview(sid("cam2")).unwrap();
        s.select_preview(sid("cam3")).unwrap();
        let frames = s.take(&Transition::Cut, 10).unwrap();
        assert_eq!(frames, 0, "cut spans zero frames");
        let frame = s.tick(10);
        assert_eq!(frame.program, sid("cam3"));
        assert!(!frame.is_transitioning());
    }

    #[test]
    fn fade_spans_exactly_the_quantized_frames() {
        let mut s = switcher();
        s.select_preview(sid("cam2")).unwrap();
        // 800 ms at 60 fps = 48 frames exactly.
        let frames = s
            .take(
                &Transition::Fade {
                    duration: Duration::from_millis(800),
                },
                100,
            )
            .unwrap();
        assert_eq!(frames, 48);

        let first = s.tick(100);
        let p = first.transition.as_ref().unwrap();
        assert_eq!(p.from, sid("cam1"));
        assert_eq!(p.to, sid("cam2"));
        assert!((p.t - (1.0 / 48.0)).abs() < 1e-9);

        // t = (elapsed + 1) / total, so the halfway blend is elapsed 23.
        let mid = s.tick(100 + 23);
        assert!((mid.transition.as_ref().unwrap().t - 0.5).abs() < 1e-9);

        // The final blend frame lands exactly on the target (t == 1.0);
        // the next frame shows program with no transition active.
        let last_blend = s.tick(100 + 47);
        let p = last_blend.transition.as_ref().unwrap();
        assert_eq!(p.frame, 47);
        assert!((p.t - 1.0).abs() < 1e-9);

        let done = s.tick(100 + 48);
        assert!(!done.is_transitioning());
        assert_eq!(done.program, sid("cam2"));
    }

    #[test]
    fn wipe_carries_its_pattern() {
        let mut s = switcher();
        s.select_preview(sid("vt")).unwrap();
        s.take(
            &Transition::Wipe {
                pattern: WipePattern::Iris,
                duration: Duration::from_millis(500),
            },
            0,
        )
        .unwrap();
        let frame = s.tick(0);
        assert_eq!(
            frame.transition.as_ref().unwrap().kind,
            TransitionKind::Wipe(WipePattern::Iris)
        );
    }

    #[test]
    fn overlay_appears_in_and_disappears_from_plan() {
        let mut s = switcher();
        s.set_overlay(Some(sid("slate"))).unwrap();
        assert_eq!(s.tick(0).overlay, Some(sid("slate")));
        s.set_overlay(None).unwrap();
        assert_eq!(s.tick(1).overlay, None);
    }

    #[test]
    fn last_good_program_tracks_committed_sources() {
        let mut s = switcher();
        s.select_preview(sid("cam2")).unwrap();
        s.take(&Transition::Cut, 0).unwrap();
        s.tick(0);
        assert_eq!(s.last_good_program(), &sid("cam2"));
    }

    // ---- boundary -------------------------------------------------------

    #[test]
    fn sub_frame_fade_quantizes_to_one_frame() {
        let mut s = switcher();
        s.select_preview(sid("cam2")).unwrap();
        let frames = s
            .take(
                &Transition::Fade {
                    duration: Duration::from_nanos(1),
                },
                0,
            )
            .unwrap();
        assert_eq!(frames, 1, "any non-zero duration is at least one frame");
        assert!(s.tick(0).is_transitioning());
        assert!(!s.tick(1).is_transitioning());
    }

    #[test]
    fn take_during_active_transition_completes_the_old_one_first() {
        let mut s = switcher();
        s.select_preview(sid("cam2")).unwrap();
        s.take(
            &Transition::Fade {
                duration: Duration::from_millis(800),
            },
            0,
        )
        .unwrap();
        s.tick(10); // mid-fade cam1 -> cam2

        s.select_preview(sid("cam3")).unwrap();
        s.take(
            &Transition::Fade {
                duration: Duration::from_millis(800),
            },
            11,
        )
        .unwrap();
        let frame = s.tick(11);
        let p = frame.transition.as_ref().unwrap();
        // The new fade runs from the *completed target* of the old fade.
        assert_eq!(p.from, sid("cam2"));
        assert_eq!(p.to, sid("cam3"));
    }

    #[test]
    fn frame_clock_boundaries() {
        let clock = FrameClock::new(60).unwrap();
        assert_eq!(clock.frames_for(Duration::ZERO), 0);
        assert_eq!(clock.frames_for(Duration::from_nanos(1)), 1);
        assert_eq!(clock.frames_for(clock.frame_duration()), 1);
        assert_eq!(
            clock.frames_for(clock.frame_duration() + Duration::from_nanos(1)),
            2
        );
        assert!(FrameClock::new(0).is_err());
    }

    // ---- invalid / malformed -------------------------------------------

    #[test]
    fn unknown_preview_source_is_rejected() {
        let mut s = switcher();
        let err = s.select_preview(sid("ghost")).unwrap_err();
        assert!(matches!(err, SwitcherError::UnknownSource(_)));
        // Program and preview unchanged.
        assert_eq!(s.preview(), &sid("cam1"));
    }

    #[test]
    fn unknown_overlay_source_is_rejected() {
        let mut s = switcher();
        assert!(s.set_overlay(Some(sid("ghost"))).is_err());
        assert_eq!(s.overlay(), None);
    }

    #[test]
    fn zero_frame_transition_is_reported_as_cut() {
        let mut s = switcher();
        s.select_preview(sid("cam2")).unwrap();
        let result = s.take(
            &Transition::Fade {
                duration: Duration::ZERO,
            },
            0,
        );
        assert!(matches!(result, Err(SwitcherError::ZeroFrameTransition)));
        // Policy: treated as a cut, so program changed.
        assert_eq!(s.program(), &sid("cam2"));
    }

    #[test]
    fn taking_an_unregistered_preview_is_rejected() {
        let mut s = Switcher::new(60, sid("cam1")).unwrap();
        s.register_source(sid("cam1"));
        // cam2 was never registered.
        assert!(s.select_preview(sid("cam2")).is_err());
    }
}
