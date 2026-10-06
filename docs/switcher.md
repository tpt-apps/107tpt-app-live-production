# Video Switcher

Classic PVW/PGM bus model (spec 7), implemented in
`tpt-app-live-production-switcher`.

## Invariants

1. **Selecting a new PVW source never affects PGM.** Pinned by
   `preview_selection_never_affects_program`.
2. **A transition commits PVW to PGM.** A CUT lands on the very tick it
   is taken; fades/wipes span exactly `ceil(duration × fps)` frames,
   computed in exact integer arithmetic (800 ms at 60 fps = 48 frames, no
   truncation drift).
3. **Frame-accurate timing.** All durations quantize through
   `FrameClock::frames_for` against the program frame rate.
4. **Last-operator-action-wins.** TAKE during an in-flight transition
   completes the old transition at its target, then starts the new one —
   the behaviour physical switchers have.
5. **`last_good_program`** always names the source last committed
   cleanly; the failsafe "freeze last good frame" policy (spec 14.1)
   holds this on air.

## Render plan, not pixels

`Switcher::tick_into` produces a `SwitchFrame` — which source program
shows, transition progress (`from`, `to`, `t`), and the overlay layer —
into a caller-owned buffer. The steady-state per-frame path is
allocation-free (`tests/rt_allocation.rs`); a GPU compositor (via
`tpt-kinetix`, spec 5.1) consumes these plans as backends come online.

Invalid sources are rejected on select/take rather than blacking program
output mid-show.
