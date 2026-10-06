//! Engine event bus: everything the UI, the local API, and the session log
//! need to observe, published from one place.

use serde::Serialize;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Mutex;
use tpt_app_live_production_model::ids::SourceId;
use tpt_app_live_production_model::OperationMode;

/// Why a failsafe fired (spec 14.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailsafeReason {
    /// Video input signal lost.
    VideoInputLost,
    /// Audio input signal lost.
    AudioInputLost,
}

/// Operator-visible engine events.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    /// Operation mode changed (always surfaced with the text label — the UI
    /// adds colour on top; never colour alone, spec 15.1).
    ModeChanged {
        /// The new mode.
        mode: OperationMode,
    },
    /// A cue fired.
    CueAdvanced {
        /// Cue number.
        number: u32,
        /// Cue label.
        label: String,
        /// Manual GO or automatic (timed/follow).
        automatic: bool,
    },
    /// A transition committed to program.
    TransitionStarted {
        /// Source program is leaving.
        from: SourceId,
        /// Source program is going to.
        to: SourceId,
        /// Duration in program frames.
        frames: u64,
    },
    /// Preview selection changed.
    PreviewChanged {
        /// The new preview source.
        source: SourceId,
    },
    /// A failsafe policy fired (spec 14.1) — always operator-visible.
    FailsafeTriggered {
        /// What was lost.
        reason: FailsafeReason,
        /// Which input.
        source: SourceId,
        /// What the engine did about it.
        action: String,
    },
    /// A lost input came back; the operator decides whether to unmute.
    InputRestored {
        /// Which input.
        source: SourceId,
    },
    /// A control surface disconnected mid-show (spec 11): operation
    /// continues via on-screen controls.
    SurfaceDisconnected {
        /// Surface name.
        name: String,
    },
    /// A control surface (re)connected.
    SurfaceConnected {
        /// Surface name.
        name: String,
    },
    /// Graceful degradation level changed (spec 14.3).
    DegradationChanged {
        /// The new level.
        level: crate::degrade::DegradationLevel,
    },
    /// Watchdog detected a stalled heartbeat and recovery ran.
    WatchdogRecovery {
        /// Milliseconds since the last heartbeat.
        stalled_for_ms: u64,
    },
    /// The session log could not be written (operator-visible: history is
    /// being lost).
    SessionLogWriteFailed,
}

/// Broadcast bus with bounded per-subscriber queues.
///
/// Publishing never blocks the engine: a subscriber that falls behind has
/// events **dropped** (with a warning log), never back-pressured into the
/// render path (spec 3.1: no blocking on the live path).
#[derive(Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<Subscriber>>,
}

#[derive(Clone)]
pub(crate) struct Subscriber {
    name: String,
    sender: SyncSender<EngineEvent>,
}

/// A queued stream of engine events.
pub struct Subscription(Receiver<EngineEvent>);

impl Subscription {
    /// Non-blocking receive: returns `None` when nothing is queued.
    pub fn try_recv(&self) -> Option<EngineEvent> {
        self.0.try_recv().ok()
    }

    /// Blocking receive with timeout.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<EngineEvent> {
        Receiver::recv_timeout(&self.0, timeout).ok()
    }
}

impl EventBus {
    /// Creates an empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes with a bounded queue of `capacity` events.
    pub fn subscribe(&self, name: impl Into<String>, capacity: usize) -> Subscription {
        let (tx, rx) = std::sync::mpsc::sync_channel(capacity.max(1));
        self.subscribers
            .lock()
            .expect("event bus lock poisoned")
            .push(Subscriber {
                name: name.into(),
                sender: tx,
            });
        Subscription(rx)
    }

    /// Publishes an event to every subscriber. Never blocks; slow or dead
    /// subscribers silently drop events.
    pub fn publish(&self, event: EngineEvent) {
        let mut dead = Vec::new();
        {
            let subs = self.subscribers.lock().expect("event bus lock poisoned");
            for (i, sub) in subs.iter().enumerate() {
                match sub.sender.try_send(event.clone()) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        log::warn!(
                            "event queue for subscriber '{}' is full; dropping event",
                            sub.name
                        );
                    }
                    Err(TrySendError::Disconnected(_)) => dead.push(i),
                }
            }
        }
        if !dead.is_empty() {
            let mut subs = self.subscribers.lock().expect("event bus lock poisoned");
            for i in dead.into_iter().rev() {
                subs.swap_remove(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribers_receive_published_events() {
        let bus = EventBus::new();
        let sub = bus.subscribe("ui", 16);
        bus.publish(EngineEvent::PreviewChanged {
            source: SourceId::new("cam2"),
        });
        match sub.recv_timeout(std::time::Duration::from_millis(100)) {
            Some(EngineEvent::PreviewChanged { source }) => assert_eq!(source.0, "cam2"),
            other => panic!("expected preview event, got {other:?}"),
        }
    }

    #[test]
    fn slow_subscriber_never_blocks_publisher() {
        let bus = EventBus::new();
        let _sub = bus.subscribe("slow", 1);
        for i in 0..100u32 {
            bus.publish(EngineEvent::PreviewChanged {
                source: SourceId::new(format!("cam{i}")),
            });
        }
        // Publisher completed without blocking; subscriber keeps at least
        // the first event.
    }

    #[test]
    fn publish_with_no_subscribers_is_fine() {
        let bus = EventBus::new();
        bus.publish(EngineEvent::ModeChanged {
            mode: OperationMode::Live,
        });
    }
}
