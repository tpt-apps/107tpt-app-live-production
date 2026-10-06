# Reliability & Failsafe

A live show cannot tolerate a silent failure (spec 14).

## Input-loss handling (spec 14.1)

Per-input health tracking distinguishes "configured but never flowing"
(not lost) from "was flowing and stopped" (lost). On loss, the
configured policy applies:

| Policy | Behaviour |
|---|---|
| `FreezeLastFrame` (default) | program holds the last cleanly committed source |
| `CutToBackup { backup }` | hard cut to the designated backup source |
| `Slate { slate }` | hard cut to a "signal lost" slate |

Audio defaults to **mute-on-error** with the show's ramp — corrupted
audio never reaches program. A lost input that returns raises
`InputRestored`; the operator decides whether to unmute (the engine
never auto-unmutes mid-show).

## Watchdog supervision (spec 14.2)

The engine bumps a `Heartbeat` every poll. A supervisor thread fires a
recovery handler if the heartbeat stalls. The handler's job is to alert
and to rebuild engine state from the last good show file (exercised by
`chaos::watchdog_detects_stall_and_recovery_rebuilds_the_engine`).
Full *process*-level supervision belongs to the OS service wrapper /
desktop shell relaunching the engine process — the in-process watchdog
plus rebuild covers the same operator-visible guarantee without a
second process in v1.

## Graceful degradation (spec 14.3)

`DegradationController` escalates one tier per 3 sustained high-pressure
samples, strictly in order: **preview quality → non-program effects →
program (emergency, operator-alerted)**, with a hysteresis dead band and
step-down on recovery. Transient spikes never degrade the show. The
engine exposes `preview_scale()` / `effects_enabled()` to the render
host; program output renders full quality until the final tier.

## Failure isolation

- Malformed show files fail at load or validation — never mid-show.
- Cue steps that reference broken things log and skip the step; one bad
  cue cannot stop the stack.
- Event subscribers that fall behind get drops, never back-pressure
  into the engine.
- Session log write failures alert (`SessionLogWriteFailed`) and
  continue.
