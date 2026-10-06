//! Watchdog supervision of the engine (spec 14.2).
//!
//! The engine touches a [`Heartbeat`] after every poll; a supervisor thread
//! watches it and invokes a recovery handler if the engine stalls. The
//! handler decides what "recovery" means for the host: at minimum alert the
//! operator; the desktop shell additionally rebuilds the engine from the
//! last good show file. Full *process*-level supervision (restart on exit)
//! belongs to the OS service wrapper — see `docs/reliability.md`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Monotonic heartbeat the engine bumps, in milliseconds.
#[derive(Debug, Default)]
pub struct Heartbeat(AtomicU64);

impl Heartbeat {
    /// Creates a heartbeat starting at the current wall clock (ms).
    pub fn starting_now() -> Arc<Self> {
        let hb = Self(AtomicU64::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        ));
        Arc::new(hb)
    }

    /// Marks the engine alive.
    pub fn beat(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.0.store(now, Ordering::Release);
    }

    /// Last beat, in wall-clock ms.
    pub fn last_beat_ms(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

/// Watchdog configuration.
#[derive(Debug, Clone, Copy)]
pub struct WatchdogConfig {
    /// How long the engine may go without a heartbeat before recovery runs.
    pub stall_timeout: Duration,
    /// How often the watchdog checks.
    pub poll_interval: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            stall_timeout: Duration::from_millis(2000),
            poll_interval: Duration::from_millis(250),
        }
    }
}

/// A running watchdog supervisor.
pub struct Watchdog {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    /// Spawns the supervisor thread.
    ///
    /// `on_stall` is invoked once per detected stall (never re-entrantly)
    /// with the stall duration. It must be fast and non-blocking: alerting
    /// and state recovery, not media work.
    pub fn spawn(
        heartbeat: Arc<Heartbeat>,
        config: WatchdogConfig,
        on_stall: Arc<dyn Fn(Duration) + Send + Sync>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("tpt-lp-watchdog".to_string())
            .spawn(move || {
                while !stop_flag.load(Ordering::Acquire) {
                    std::thread::sleep(config.poll_interval);
                    if stop_flag.load(Ordering::Acquire) {
                        break;
                    }
                    let last = heartbeat.last_beat_ms();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let stalled_for = now.saturating_sub(last);
                    if stalled_for > config.stall_timeout.as_millis() as u64 {
                        log::error!(
                            "engine heartbeat stalled for {stalled_for}ms; invoking recovery"
                        );
                        on_stall(Duration::from_millis(stalled_for));
                        // Re-arm: only the heartbeat resuming (or recovery
                        // beating on the engine's behalf) prevents repeat
                        // firings at the poll cadence.
                        heartbeat.beat();
                    }
                }
            })
            .expect("watchdog thread spawn");
        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Stops the supervisor thread.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Wall-clock deadline helper for tests and the engine loop: returns a
/// closure that reports elapsed time since construction.
pub fn stopwatch() -> impl Fn() -> Duration {
    let start = Instant::now();
    move || start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn watchdog_fires_when_heartbeat_stalls() {
        let heartbeat = Heartbeat::starting_now();
        // Do not beat the heartbeat ever again.
        let firings = Arc::new(Mutex::new(Vec::new()));
        let firings2 = firings.clone();
        let mut wd = Watchdog::spawn(
            heartbeat,
            WatchdogConfig {
                stall_timeout: Duration::from_millis(50),
                poll_interval: Duration::from_millis(10),
            },
            Arc::new(move |d| firings2.lock().unwrap().push(d)),
        );
        std::thread::sleep(Duration::from_millis(200));
        wd.shutdown();
        assert!(
            !firings.lock().unwrap().is_empty(),
            "watchdog must fire on a stalled heartbeat"
        );
    }

    #[test]
    fn watchdog_stays_quiet_while_heartbeat_runs() {
        let heartbeat = Heartbeat::starting_now();
        let firings = Arc::new(Mutex::new(0usize));
        let firings2 = firings.clone();
        let beat_flag = Arc::new(AtomicBool::new(true));
        let beat_flag2 = beat_flag.clone();
        let hb2 = heartbeat.clone();
        let beater = std::thread::spawn(move || {
            while beat_flag2.load(Ordering::Acquire) {
                hb2.beat();
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mut wd = Watchdog::spawn(
            heartbeat,
            WatchdogConfig {
                stall_timeout: Duration::from_millis(80),
                poll_interval: Duration::from_millis(10),
            },
            Arc::new(move |_| {
                *firings2.lock().unwrap() += 1;
            }),
        );
        std::thread::sleep(Duration::from_millis(250));
        wd.shutdown();
        beat_flag.store(false, Ordering::Release);
        let _ = beater.join();
        assert_eq!(
            *firings.lock().unwrap(),
            0,
            "healthy heartbeat must not fire the watchdog"
        );
    }

    #[test]
    fn shutdown_is_idempotent() {
        let heartbeat = Heartbeat::starting_now();
        let mut wd = Watchdog::spawn(heartbeat, WatchdogConfig::default(), Arc::new(|_| {}));
        wd.shutdown();
        wd.shutdown();
    }
}
