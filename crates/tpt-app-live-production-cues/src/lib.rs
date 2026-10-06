//! Cue stack execution (spec 10).
//!
//! [`CueStackRunner`] owns the timing state machine around a
//! [`CueStack`](tpt_app_live_production_model::cue::CueStack): which cue
//! fired when, whether the next cue is due (timed/follow advance), and
//! manual GO. It deliberately does **not** touch video/audio/lighting — it
//! yields [`CueExecution`]s that the core engine applies, so cue ordering
//! logic stays independently testable.
//!
//! Live-safety invariant (spec 10): reordering, inserting, or editing a cue
//! must not disrupt a currently-live cue. Editing goes through the
//! [`CueStack`] operations, which keep the live cue's identity stable; the
//! runner additionally tracks the live cue by **id**, so an edit that moves
//! it in the vec does not confuse advance logic.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::Serialize;
use tpt_app_live_production_model::cue::{AdvanceMode, Cue, CueStack};

/// One cue firing, yielded by [`CueStackRunner::go`] / [`CueStackRunner::poll`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CueExecution {
    /// Index the cue fired from (position in the stack at fire time).
    pub index: usize,
    /// The cue's stable id.
    pub cue_id: String,
    /// The cue itself.
    #[serde(skip)]
    pub cue: Cue,
    /// Monotonic ms timestamp of the firing.
    pub fired_at_ms: u64,
    /// Whether this firing was automatic (timed/follow) rather than a
    /// manual GO.
    pub automatic: bool,
}

/// Cue runner errors.
#[derive(Debug, thiserror::Error)]
pub enum CueRunnerError {
    /// An edit referenced an index outside the stack.
    #[error("cue index {index} out of range (stack has {len} cues)")]
    IndexOutOfBounds {
        /// The bad index.
        index: usize,
        /// Current stack length.
        len: usize,
    },
}

/// The cue stack timing state machine.
#[derive(Debug, Clone)]
pub struct CueStackRunner {
    stack: CueStack,
    /// Id of the most recently fired cue.
    live_cue_id: Option<String>,
    /// When the live cue fired.
    fired_at_ms: u64,
    /// Advance mode of the live cue (drives the *next* cue's trigger).
    live_advance: Option<AdvanceMode>,
    /// Whether the live cue's video transition has completed (follow
    /// advance waits for this).
    transition_complete: bool,
    /// Total cues fired (for session logs).
    fired_count: u64,
}

impl CueStackRunner {
    /// Creates a runner over a cue stack (before first GO).
    pub fn new(stack: CueStack) -> Self {
        Self {
            stack,
            live_cue_id: None,
            fired_at_ms: 0,
            live_advance: None,
            transition_complete: true,
            fired_count: 0,
        }
    }

    /// The underlying stack (for UI display and persistence).
    pub fn stack(&self) -> &CueStack {
        &self.stack
    }

    /// Mutable access for editing; prefer the runner's own edit methods,
    /// which preserve live-cue invariants.
    pub fn stack_mut(&mut self) -> &mut CueStack {
        &mut self.stack
    }

    /// Id of the live (most recently fired) cue, if any.
    pub fn live_cue_id(&self) -> Option<&str> {
        self.live_cue_id.as_deref()
    }

    /// The cue the next GO/poll will fire.
    pub fn next_cue(&self) -> Option<&Cue> {
        self.stack.next()
    }

    /// Total cues fired this session.
    pub fn fired_count(&self) -> u64 {
        self.fired_count
    }

    /// True when the stack is exhausted (every cue has fired).
    pub fn is_finished(&self) -> bool {
        self.stack.next_index().is_none()
    }

    /// Manual GO: fires the next cue now.
    pub fn go(&mut self, now_ms: u64) -> Option<CueExecution> {
        self.fire(now_ms, false)
    }

    /// Automatic advance: fires the next cue when the live cue's advance
    /// mode says so.
    ///
    /// `transition_complete` reports whether the live cue's video
    /// transition has finished (follow advance requires it; timed advance
    /// counts from the fire time regardless).
    pub fn poll(&mut self, now_ms: u64, transition_complete: bool) -> Option<CueExecution> {
        self.stack.next_index()?;
        let advance = self.live_advance?;
        let due = match advance {
            AdvanceMode::Manual => false,
            AdvanceMode::Timed { after_ms } => now_ms.saturating_sub(self.fired_at_ms) >= after_ms,
            AdvanceMode::Follow => transition_complete,
        };
        if due {
            self.fire(now_ms, true)
        } else {
            None
        }
    }

    fn fire(&mut self, now_ms: u64, automatic: bool) -> Option<CueExecution> {
        let index = self.stack.next_index()?;
        let cue = self.stack.mark_fired(index)?.clone();
        self.live_cue_id = Some(cue.id.0.clone());
        self.fired_at_ms = now_ms;
        self.live_advance = Some(cue.advance);
        // The fired cue's transition (if any) is now in flight; the engine
        // reports completion via poll's argument.
        self.transition_complete = cue.video_transition.is_none();
        self.fired_count += 1;
        Some(CueExecution {
            index,
            cue_id: cue.id.0.clone(),
            cue,
            fired_at_ms: now_ms,
            automatic,
        })
    }

    /// Rewinds to before-first-GO (used when a show is reloaded or
    /// re-rehearsed). Does not touch cue definitions.
    pub fn rewind(&mut self) {
        self.stack.rewind();
        self.live_cue_id = None;
        self.live_advance = None;
        self.transition_complete = true;
        self.fired_at_ms = 0;
    }

    // ---- live-safe editing (spec 10) ------------------------------------

    /// Inserts a cue at `index`. Inserting at or before the live cue keeps
    /// the same cue live (the stack shifts around it). Returns the index
    /// the cue landed at.
    pub fn insert(&mut self, index: usize, cue: Cue) -> usize {
        self.stack.insert(index, cue)
    }

    /// Appends a cue.
    pub fn push(&mut self, cue: Cue) {
        self.stack.push(cue);
    }

    /// Removes a cue at `index`. Removing the live cue leaves advance state
    /// consistent: the runner treats the show as "no live cue" until the
    /// next GO.
    pub fn remove(&mut self, index: usize) -> Result<Cue, CueRunnerError> {
        if index >= self.stack.len() {
            return Err(CueRunnerError::IndexOutOfBounds {
                index,
                len: self.stack.len(),
            });
        }
        let removing_live = self
            .stack
            .cues
            .get(index)
            .map(|c| Some(&c.id.0) == self.live_cue_id.as_ref())
            .unwrap_or(false);
        let removed = self
            .stack
            .remove(index)
            .expect("index bounds checked above");
        if removing_live {
            self.live_cue_id = None;
            self.live_advance = None;
            self.transition_complete = true;
        }
        Ok(removed)
    }

    /// Replaces a cue at `index` with an edited version. Replacing the live
    /// cue does **not** restart or alter it — the edit applies the next time
    /// that cue fires.
    pub fn replace(&mut self, index: usize, cue: Cue) -> Result<Cue, CueRunnerError> {
        if index >= self.stack.len() {
            return Err(CueRunnerError::IndexOutOfBounds {
                index,
                len: self.stack.len(),
            });
        }
        self.stack
            .replace(index, cue)
            .ok_or(CueRunnerError::IndexOutOfBounds {
                index,
                len: self.stack.len(),
            })
    }

    /// Moves a cue (drag-and-drop reorder). The live cue stays live.
    pub fn move_cue(&mut self, from: usize, to: usize) -> Result<(), CueRunnerError> {
        if from >= self.stack.len() {
            return Err(CueRunnerError::IndexOutOfBounds {
                index: from,
                len: self.stack.len(),
            });
        }
        self.stack.move_cue(from, to);
        // Keep the runner's live id in sync with the (possibly moved) index.
        if let Some(id) = &self.live_cue_id {
            if let Some(pos) = self.stack.cues.iter().position(|c| &c.id.0 == id) {
                self.stack.current_index = Some(pos);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tpt_app_live_production_model::cue::{CueNumber, Transition};
    use tpt_app_live_production_model::ids::CueId;

    fn cue(n: u32, advance: AdvanceMode, fade_ms: Option<u64>) -> Cue {
        let mut c = Cue::new(n, format!("Cue {n}"));
        c.advance = advance;
        if let Some(ms) = fade_ms {
            c.video_transition = Some(Transition::Fade {
                duration: Duration::from_millis(ms),
            });
        }
        c
    }

    fn stack() -> CueStack {
        CueStack {
            cues: vec![
                cue(1, AdvanceMode::Manual, None),
                cue(2, AdvanceMode::Timed { after_ms: 500 }, None),
                cue(3, AdvanceMode::Follow, Some(200)),
                cue(4, AdvanceMode::Manual, None),
            ],
            current_index: None,
        }
    }

    fn runner() -> CueStackRunner {
        CueStackRunner::new(stack())
    }

    // ---- manual advance --------------------------------------------------

    #[test]
    fn manual_go_fires_cues_in_order() {
        let mut r = runner();
        let e1 = r.go(1000).unwrap();
        assert_eq!(e1.cue.number, CueNumber(1));
        assert!(!e1.automatic);
        let e2 = r.go(1500).unwrap();
        assert_eq!(e2.cue.number, CueNumber(2));
    }

    #[test]
    fn manual_advance_mode_never_auto_fires() {
        let mut r = runner();
        r.go(0).unwrap(); // cue 1: manual advance
        for t in 0..10_000 {
            assert!(r.poll(t, true).is_none(), "manual must never auto-advance");
        }
    }

    // ---- timed advance ---------------------------------------------------

    #[test]
    fn timed_advance_fires_after_delay_not_before() {
        let mut r = runner();
        r.go(0).unwrap(); // cue 1 manual
        r.go(1000).unwrap(); // cue 2 timed 500ms
                             // Not due at 1499.
        assert!(r.poll(1499, true).is_none());
        // Due at exactly 1500 (boundary: fired 1000 + 500).
        let e = r.poll(1500, true).unwrap();
        assert_eq!(e.cue.number, CueNumber(3));
        assert!(e.automatic);
    }

    #[test]
    fn timed_advance_counts_from_fire_time_even_if_transition_pending() {
        let mut r = runner();
        r.go(0).unwrap(); // cue 1 (manual, no transition)
                          // Fire cue 2 with a transition that is NOT complete:
                          // replace cue 2 with a fading variant.
        let mut fading = cue(2, AdvanceMode::Timed { after_ms: 100 }, Some(5000));
        fading.id = CueId::new("cue-2");
        r.replace(1, fading).unwrap();
        r.go(1000).unwrap();
        // Timed ignores transition completion.
        assert!(r.poll(1100, false).is_some());
    }

    // ---- follow advance --------------------------------------------------

    #[test]
    fn follow_waits_for_transition_completion() {
        let mut r = runner();
        r.go(0).unwrap(); // cue 1 manual
        r.go(100).unwrap(); // cue 2 timed
        let e3 = r.poll(2000, true).unwrap(); // cue 3 follow (fade 200ms)
        assert_eq!(e3.cue.number, CueNumber(3));
        // Cue 3 advances on Follow: once its transition completes, cue 4
        // fires automatically. While the transition runs it must not.
        assert!(
            r.poll(2100, false).is_none(),
            "follow must wait for transition"
        );
        let e4 = r
            .poll(3000, true)
            .expect("cue 4 fires when cue 3 completes");
        assert_eq!(e4.cue.number, CueNumber(4));
        assert!(e4.automatic);
        // End of stack.
        assert!(r.poll(9000, true).is_none());
    }

    #[test]
    fn follow_fires_when_transition_completes() {
        let mut r = CueStackRunner::new(CueStack {
            cues: vec![
                cue(1, AdvanceMode::Follow, Some(100)),
                cue(2, AdvanceMode::Manual, None),
            ],
            current_index: None,
        });
        r.go(0).unwrap();
        assert!(r.poll(50, false).is_none(), "transition still running");
        let e = r.poll(100, true).unwrap();
        assert_eq!(e.cue.number, CueNumber(2));
    }

    // ---- boundaries / malformed -----------------------------------------

    #[test]
    fn go_past_end_returns_none() {
        let mut r = CueStackRunner::new(CueStack {
            cues: vec![cue(1, AdvanceMode::Manual, None)],
            current_index: None,
        });
        assert!(r.go(0).is_some());
        assert!(r.go(1).is_none());
        assert!(r.go(2).is_none());
        assert!(r.is_finished());
    }

    #[test]
    fn empty_stack_go_returns_none() {
        let mut r = CueStackRunner::new(CueStack::default());
        assert!(r.go(0).is_none());
    }

    #[test]
    fn rewind_allows_replay() {
        let mut r = runner();
        r.go(0).unwrap();
        r.go(1).unwrap();
        r.rewind();
        let e = r.go(2).unwrap();
        assert_eq!(e.cue.number, CueNumber(1));
        assert_eq!(r.fired_count(), 3, "fired_count counts all firings");
    }

    #[test]
    fn out_of_range_edits_are_errors_not_panics() {
        let mut r = runner();
        assert!(matches!(
            r.remove(99),
            Err(CueRunnerError::IndexOutOfBounds { index: 99, len: 4 })
        ));
        assert!(r.replace(99, cue(9, AdvanceMode::Manual, None)).is_err());
        assert!(r.move_cue(99, 0).is_err());
    }

    // ---- live-safe editing ----------------------------------------------

    #[test]
    fn insert_before_live_cue_keeps_same_cue_live() {
        let mut r = runner();
        let first = r.go(0).unwrap(); // cue 1 live
        let live_id = first.cue_id.clone();

        let inserted = r.insert(0, cue(99, AdvanceMode::Manual, None));
        assert_eq!(inserted, 0);
        assert_eq!(r.live_cue_id(), Some(live_id.as_str()));
        // Next GO fires what is now at index 2 (old cue 2).
        let next = r.next_cue().unwrap();
        assert_eq!(next.number, CueNumber(2));
    }

    #[test]
    fn insert_after_live_cue_does_not_shift_next() {
        let mut r = runner();
        r.go(0).unwrap(); // cue 1 live
        r.insert(3, cue(99, AdvanceMode::Manual, None)); // after next (index 1)
        let next = r.next_cue().unwrap();
        assert_eq!(next.number, CueNumber(2));
    }

    #[test]
    fn removing_live_cue_resets_advance_state() {
        let mut r = runner();
        r.go(0).unwrap(); // cue 1 live, manual advance
        r.remove(0).unwrap();
        assert_eq!(r.live_cue_id(), None);
        // Next cue (old cue 2) can still fire manually.
        let e = r.go(10).unwrap();
        assert_eq!(e.cue.number, CueNumber(2));
    }

    #[test]
    fn removing_future_cue_keeps_live_state() {
        let mut r = runner();
        let first = r.go(0).unwrap();
        let live_id = first.cue_id.clone();
        r.remove(3).unwrap(); // remove cue 4 (after live)
        assert_eq!(r.live_cue_id(), Some(live_id.as_str()));
    }

    #[test]
    fn replacing_live_cue_does_not_interrupt_it() {
        let mut r = runner();
        let first = r.go(0).unwrap();
        let live_id = first.cue_id.clone();
        let mut edited = cue(1, AdvanceMode::Manual, None);
        edited.label = "Edited".into();
        r.replace(0, edited).unwrap();
        assert_eq!(r.live_cue_id(), Some(live_id.as_str()));
        assert_eq!(r.stack().current().unwrap().label, "Edited");
    }

    #[test]
    fn moving_cues_keeps_live_cue_live() {
        let mut r = runner();
        let first = r.go(0).unwrap();
        let live_id = first.cue_id.clone();
        // Move cue 4 to the front.
        r.move_cue(3, 0).unwrap();
        assert_eq!(r.live_cue_id(), Some(live_id.as_str()));
        // The stack's current pointer still refers to cue 1.
        assert_eq!(r.stack().current().unwrap().id.0, live_id);
    }

    #[test]
    fn timed_due_at_exact_boundary() {
        let mut r = CueStackRunner::new(CueStack {
            cues: vec![
                cue(1, AdvanceMode::Timed { after_ms: 0 }, None),
                cue(2, AdvanceMode::Manual, None),
            ],
            current_index: None,
        });
        r.go(500).unwrap();
        // after_ms = 0 is immediately due.
        assert!(r.poll(500, false).is_some());
    }
}
