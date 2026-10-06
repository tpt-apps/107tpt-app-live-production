//! Control-surface layer (spec 11): configurable mapping from physical or
//! virtual surface inputs to application functions, visual feedback, and
//! graceful mid-show disconnect handling.
//!
//! The core abstraction is protocol-agnostic ([`SurfaceControl`] →
//! [`SurfaceAction`]); concrete transports plug in behind features — OSC
//! via `tpt-av-control` (feature `osc`) and an in-process
//! [`VirtualSurface`] used by the on-screen console and tests. The mapping
//! table is a plain serializable struct so the UI's mapping screen and
//! show templates can carry it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// One physical/virtual control on a surface.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceControl {
    /// OSC address, e.g. `"/go"`.
    Osc(String),
    /// MIDI note-on (channel folded into mapping; MVP uses one channel).
    MidiNote(u8),
    /// MIDI control change (fader/encoder).
    MidiCc(u8),
    /// A button on the virtual (on-screen) surface.
    VirtualButton(String),
}

impl SurfaceControl {
    /// Stable human-readable name for UI and logs.
    pub fn describe(&self) -> String {
        match self {
            SurfaceControl::Osc(addr) => format!("osc:{addr}"),
            SurfaceControl::MidiNote(n) => format!("midi:note{n}"),
            SurfaceControl::MidiCc(n) => format!("midi:cc{n}"),
            SurfaceControl::VirtualButton(name) => format!("virtual:{name}"),
        }
    }
}

/// What a mapped control does to the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SurfaceAction {
    /// Advance to the next cue (GO).
    Go,
    /// TAKE: commit preview to program with a cut.
    Cut,
    /// TAKE: commit preview to program with a fade of `duration_ms`.
    Fade {
        /// Fade duration in milliseconds.
        duration_ms: u64,
    },
    /// Select the preview source.
    SelectPreview {
        /// Source to preview.
        source: String,
    },
    /// Set a channel's gain.
    SetGain {
        /// Target source.
        source: String,
        /// Gain in dB.
        gain_db: f64,
    },
    /// Mute or unmute a channel.
    SetMute {
        /// Target source.
        source: String,
        /// New mute state.
        muted: bool,
    },
    /// Switch to rehearsal mode. Surfaces may only switch to Rehearsal;
    /// going Live requires the console (deliberate safety friction — an
    /// accidental surface tap must not go on air).
    ToRehearsal,
}

/// One mapping row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceMapping {
    /// The incoming control.
    pub control: SurfaceControl,
    /// What it does.
    pub action: SurfaceAction,
}

/// The configurable mapping table (spec 11.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MappingConfig {
    /// Rows, first match wins.
    #[serde(default)]
    pub mappings: Vec<SurfaceMapping>,
}

impl MappingConfig {
    /// Looks up the action for a control.
    pub fn action_for(&self, control: &SurfaceControl) -> Option<&SurfaceAction> {
        self.mappings
            .iter()
            .find(|m| &m.control == control)
            .map(|m| &m.action)
    }
}

/// A parsed value accompanying a control event (fader position etc.).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlValue(pub f32);

/// What the manager tells the host about a handled control.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceOutcome {
    /// The control was mapped and applied to the engine.
    Applied(SurfaceAction),
    /// The control arrived but has no mapping (ignored).
    Unmapped(SurfaceControl),
    /// A mapped control arrived while the surface is disconnected; it was
    /// dropped (a disconnected surface must not drive the show).
    DroppedDisconnected(SurfaceControl),
}

/// Visual feedback sink (spec 11.2): LEDs, scribbles, motorized faders —
/// whatever the surface supports. Unsupported sinks no-op.
pub trait FeedbackSink: Send {
    /// Lights (or extinguishes) the control's indicator.
    fn set_indicator(&mut self, control: &SurfaceControl, on: bool);
}

/// Records indicator changes (tests + the on-screen console).
#[derive(Default)]
pub struct RecordingFeedback {
    /// (control, on) pairs in arrival order.
    pub states: Vec<(String, bool)>,
}

impl FeedbackSink for RecordingFeedback {
    fn set_indicator(&mut self, control: &SurfaceControl, on: bool) {
        self.states.push((control.describe(), on));
    }
}

/// The engine operations a surface action can drive. Implemented by the
/// host (and by a test double) so this crate stays engine-independent.
pub trait EngineOps {
    /// Fires the next cue.
    fn go(&mut self);
    /// Cut preview to program.
    fn cut(&mut self);
    /// Fade preview to program over `duration`.
    fn fade(&mut self, duration: Duration);
    /// Select the preview source.
    fn select_preview(&mut self, source: &str);
    /// Set a channel's gain.
    fn set_gain(&mut self, source: &str, gain_db: f64);
    /// Mute/unmute a channel.
    fn set_mute(&mut self, source: &str, muted: bool);
    /// Switch to rehearsal mode.
    fn to_rehearsal(&mut self);
}

/// The protocol-agnostic surface manager: mapping table + connected state
/// + feedback.
///
/// The host feeds it raw controls and pushes engine state back into it
/// for indicator updates.
pub struct SurfaceManager {
    name: String,
    config: MappingConfig,
    connected: bool,
    feedback: Box<dyn FeedbackSink>,
}

impl SurfaceManager {
    /// Creates a manager for a named surface.
    pub fn new(
        name: impl Into<String>,
        config: MappingConfig,
        feedback: Box<dyn FeedbackSink>,
    ) -> Self {
        Self {
            name: name.into(),
            config,
            connected: true,
            feedback,
        }
    }

    /// Surface name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The mapping table.
    pub fn config(&self) -> &MappingConfig {
        &self.config
    }

    /// Whether the surface is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Feeds one control event with an optional analog value.
    pub fn handle(
        &mut self,
        control: SurfaceControl,
        value: Option<ControlValue>,
        engine: &mut dyn EngineOps,
    ) -> SurfaceOutcome {
        if !self.connected {
            // Mid-show disconnect (spec 11.3): surface input is dropped;
            // the show continues via on-screen controls.
            return SurfaceOutcome::DroppedDisconnected(control);
        }
        let Some(action) = self.config.action_for(&control).cloned() else {
            return SurfaceOutcome::Unmapped(control);
        };
        apply_action(&action, value, engine);
        SurfaceOutcome::Applied(action)
    }

    /// Marks the surface disconnected mid-show. Operation continues via
    /// on-screen controls (spec 11.3).
    pub fn disconnect(&mut self) {
        if self.connected {
            self.connected = false;
            let controls: Vec<SurfaceControl> = self
                .config
                .mappings
                .iter()
                .map(|m| m.control.clone())
                .collect();
            for c in controls {
                self.feedback.set_indicator(&c, false);
            }
            log::warn!(
                "control surface '{}' disconnected; continuing via on-screen controls",
                self.name
            );
        }
    }

    /// Marks the surface (re)connected.
    pub fn reconnect(&mut self) {
        self.connected = true;
    }

    /// Pushes current engine state into the surface's indicators (spec
    /// 11.2): e.g. the button mapped to `SelectPreview(cam1)` lights while
    /// cam1 is on program or preview.
    pub fn update_feedback(&mut self, program_source: &str, preview_source: &str) {
        if !self.connected {
            return;
        }
        let updates: Vec<(SurfaceControl, bool)> = self
            .config
            .mappings
            .iter()
            .map(|m| {
                let on = match &m.action {
                    SurfaceAction::SelectPreview { source } => {
                        source == program_source || source == preview_source
                    }
                    _ => false,
                };
                (m.control.clone(), on)
            })
            .collect();
        for (c, on) in updates {
            self.feedback.set_indicator(&c, on);
        }
    }

    /// Access to the feedback sink (host may push custom state).
    pub fn feedback(&mut self) -> &mut dyn FeedbackSink {
        &mut *self.feedback
    }
}

fn apply_action(action: &SurfaceAction, value: Option<ControlValue>, engine: &mut dyn EngineOps) {
    match action {
        SurfaceAction::Go => engine.go(),
        SurfaceAction::Cut => engine.cut(),
        SurfaceAction::Fade { duration_ms } => engine.fade(Duration::from_millis(*duration_ms)),
        SurfaceAction::SelectPreview { source } => engine.select_preview(source),
        SurfaceAction::SetGain { source, gain_db } => {
            // A fader value (0..1) scales the configured gain when present.
            let gain = match value {
                Some(ControlValue(v)) => gain_db.clamp(-60.0, 12.0) * f64::from(v.clamp(0.0, 1.0)),
                None => *gain_db,
            };
            engine.set_gain(source, gain);
        }
        SurfaceAction::SetMute { source, muted } => engine.set_mute(source, *muted),
        SurfaceAction::ToRehearsal => engine.to_rehearsal(),
    }
}

/// In-process surface used by the on-screen console and tests: virtual
/// buttons share exactly one code path with physical surfaces.
pub struct VirtualSurface {
    /// The underlying manager.
    pub manager: SurfaceManager,
}

impl VirtualSurface {
    /// Creates a virtual surface with the given mappings.
    pub fn new(config: MappingConfig, feedback: Box<dyn FeedbackSink>) -> Self {
        Self {
            manager: SurfaceManager::new("virtual", config, feedback),
        }
    }
}

#[cfg(feature = "osc")]
pub mod osc {
    //! OSC transport over `tpt-av-control-osc` (feature `osc`).
    //!
    //! Runs the crate's synchronous UDP server on a dedicated thread and
    //! forwards parsed messages as [`SurfaceControl::Osc`] events through a
    //! bounded channel — the engine never blocks on the network (spec 3.1).

    use super::{ControlValue, SurfaceControl};
    use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
    use std::time::Duration;
    use tpt_av_control_osc::OscServer;

    /// A running OSC listener.
    pub struct OscSurface {
        receiver: Receiver<(SurfaceControl, Option<ControlValue>)>,
        local_addr: std::net::SocketAddr,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl OscSurface {
        /// Binds an OSC listener on `port` (standard OSC port is 8000).
        ///
        /// Each received message becomes a
        /// [`SurfaceControl::Osc(address)`]; the first float/int argument,
        /// when present, becomes the [`ControlValue`]. Malformed datagrams
        /// are logged and dropped (spec 20: malformed input never crashes
        /// the app).
        pub fn bind(port: u16) -> std::io::Result<Self> {
            let (tx, rx) = std::sync::mpsc::sync_channel(256);
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_flag = stop.clone();
            let server = OscServer::new(port).map_err(|e| std::io::Error::other(e.to_string()))?;
            let local_addr = server
                .local_addr()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            std::thread::Builder::new()
                .name("tpt-lp-osc".to_string())
                .spawn(move || {
                    run_osc_loop(server, tx, stop_flag);
                })?;
            Ok(Self {
                receiver: rx,
                local_addr,
                stop,
            })
        }

        /// The bound address.
        pub fn local_addr(&self) -> std::net::SocketAddr {
            self.local_addr
        }

        /// Drains queued controls (non-blocking).
        pub fn drain(&self) -> Vec<(SurfaceControl, Option<ControlValue>)> {
            let mut out = Vec::new();
            while let Ok(event) = self.receiver.try_recv() {
                out.push(event);
            }
            out
        }

        /// Stops the listener thread.
        pub fn shutdown(&self) {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    impl Drop for OscSurface {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    fn flatten_packet(
        packet: tpt_av_control_osc::OscPacket,
    ) -> Vec<tpt_av_control_osc::OscMessage> {
        use tpt_av_control_osc::OscPacket;
        let mut out = Vec::new();
        let mut stack = vec![packet];
        while let Some(p) = stack.pop() {
            match p {
                OscPacket::Message(m) => out.push(m),
                OscPacket::Bundle(b) => {
                    for element in b.elements {
                        stack.push(element);
                    }
                }
            }
        }
        out
    }

    fn run_osc_loop(
        mut server: OscServer,
        tx: SyncSender<(SurfaceControl, Option<ControlValue>)>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let _ = server.set_nonblocking(true);
        while !stop.load(std::sync::atomic::Ordering::Acquire) {
            match server.recv_packet() {
                Ok((packet, _src)) => {
                    for msg in flatten_packet(packet) {
                        let value = msg.arguments.iter().find_map(|arg| match arg {
                            tpt_av_control_osc::OscArg::Float(f) => Some(ControlValue(*f)),
                            tpt_av_control_osc::OscArg::Int(i) => Some(ControlValue(*i as f32)),
                            _ => None,
                        });
                        let control = SurfaceControl::Osc(msg.address);
                        match tx.try_send((control, value)) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {
                                log::warn!("OSC event queue full; dropping control");
                            }
                            Err(TrySendError::Disconnected(_)) => return,
                        }
                    }
                }
                Err(tpt_av_control_osc::ControlError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    log::debug!("OSC receive error: {e}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_app_live_production_model::OperationMode;

    struct FakeEngine {
        calls: Vec<String>,
        gain: f64,
    }

    impl FakeEngine {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                gain: 0.0,
            }
        }
    }

    impl EngineOps for FakeEngine {
        fn go(&mut self) {
            self.calls.push("go".into());
        }
        fn cut(&mut self) {
            self.calls.push("cut".into());
        }
        fn fade(&mut self, duration: Duration) {
            self.calls.push(format!("fade({:?})", duration));
        }
        fn select_preview(&mut self, source: &str) {
            self.calls.push(format!("preview({source})"));
        }
        fn set_gain(&mut self, source: &str, gain_db: f64) {
            self.gain = gain_db;
            self.calls.push(format!("gain({source},{gain_db})"));
        }
        fn set_mute(&mut self, source: &str, muted: bool) {
            self.calls.push(format!("mute({source},{muted})"));
        }
        fn to_rehearsal(&mut self) {
            self.calls.push("to_rehearsal".into());
        }
    }

    fn mapping() -> MappingConfig {
        MappingConfig {
            mappings: vec![
                SurfaceMapping {
                    control: SurfaceControl::VirtualButton("go".into()),
                    action: SurfaceAction::Go,
                },
                SurfaceMapping {
                    control: SurfaceControl::VirtualButton("cut".into()),
                    action: SurfaceAction::Cut,
                },
                SurfaceMapping {
                    control: SurfaceControl::Osc("/cam/1".into()),
                    action: SurfaceAction::SelectPreview {
                        source: "cam1".into(),
                    },
                },
                SurfaceMapping {
                    control: SurfaceControl::MidiCc(7),
                    action: SurfaceAction::SetGain {
                        source: "mic1".into(),
                        gain_db: 6.0,
                    },
                },
                SurfaceMapping {
                    control: SurfaceControl::Osc("/mode/rehearsal".into()),
                    action: SurfaceAction::ToRehearsal,
                },
            ],
        }
    }

    #[test]
    fn mapped_controls_drive_engine_actions() {
        let mut engine = FakeEngine::new();
        let mut surface =
            SurfaceManager::new("test", mapping(), Box::new(RecordingFeedback::default()));

        let out = surface.handle(
            SurfaceControl::VirtualButton("go".into()),
            None,
            &mut engine,
        );
        assert_eq!(out, SurfaceOutcome::Applied(SurfaceAction::Go));
        assert_eq!(engine.calls, vec!["go"]);

        surface.handle(
            SurfaceControl::VirtualButton("cut".into()),
            None,
            &mut engine,
        );
        surface.handle(SurfaceControl::Osc("/cam/1".into()), None, &mut engine);
        assert!(engine.calls.contains(&"cut".to_string()));
        assert!(engine.calls.contains(&"preview(cam1)".to_string()));
    }

    #[test]
    fn unmapped_controls_are_ignored_not_errors() {
        let mut engine = FakeEngine::new();
        let mut surface =
            SurfaceManager::new("test", mapping(), Box::new(RecordingFeedback::default()));
        let out = surface.handle(
            SurfaceControl::VirtualButton("does_not_exist".into()),
            None,
            &mut engine,
        );
        assert_eq!(
            out,
            SurfaceOutcome::Unmapped(SurfaceControl::VirtualButton("does_not_exist".into()))
        );
        assert!(engine.calls.is_empty());
    }

    #[test]
    fn fader_value_scales_gain() {
        let mut engine = FakeEngine::new();
        let mut surface =
            SurfaceManager::new("test", mapping(), Box::new(RecordingFeedback::default()));
        surface.handle(
            SurfaceControl::MidiCc(7),
            Some(ControlValue(0.5)),
            &mut engine,
        );
        assert!(
            (engine.gain - 3.0).abs() < 1e-6,
            "half fader on +6 dB = 3 dB, got {}",
            engine.gain
        );
    }

    #[test]
    fn disconnected_surface_drops_input_and_show_continues() {
        let mut engine = FakeEngine::new();
        let mut surface =
            SurfaceManager::new("test", mapping(), Box::new(RecordingFeedback::default()));
        surface.disconnect();
        assert!(!surface.is_connected());
        let out = surface.handle(
            SurfaceControl::VirtualButton("go".into()),
            None,
            &mut engine,
        );
        assert!(matches!(out, SurfaceOutcome::DroppedDisconnected(_)));
        assert!(
            engine.calls.is_empty(),
            "no surface input reaches the engine after disconnect"
        );
        // On-screen controls (direct engine calls) still work:
        engine.go();
        assert_eq!(engine.calls, vec!["go"]);
        // Reconnect restores surface control.
        surface.reconnect();
        let out = surface.handle(
            SurfaceControl::VirtualButton("go".into()),
            None,
            &mut engine,
        );
        assert!(matches!(out, SurfaceOutcome::Applied(_)));
    }

    #[test]
    fn disconnect_turns_off_all_indicators() {
        let feedback = RecordingFeedback::default();
        let mut surface = SurfaceManager::new("test", mapping(), Box::new(feedback));
        let mut feedback = RecordingFeedback::default();
        let _ = &mut feedback; // (second instance unused; first records)
        surface.disconnect();
        // The manager's internal sink recorded the off states; verify via a
        // fresh manager with a shared recorder:
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>>);
        impl FeedbackSink for SharedSink {
            fn set_indicator(&mut self, control: &SurfaceControl, on: bool) {
                self.0.lock().unwrap().push((control.describe(), on));
            }
        }
        let mut surface2 =
            SurfaceManager::new("t2", mapping(), Box::new(SharedSink(shared.clone())));
        surface2.disconnect();
        let states = shared.lock().unwrap();
        assert!(
            states.iter().all(|(_, on)| !on),
            "all indicators off: {states:?}"
        );
        assert_eq!(states.len(), 5);
    }

    #[test]
    fn feedback_lights_mapped_source_buttons() {
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>>);
        impl FeedbackSink for Sink {
            fn set_indicator(&mut self, control: &SurfaceControl, on: bool) {
                self.0.lock().unwrap().push((control.describe(), on));
            }
        }
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut surface = SurfaceManager::new("t", mapping(), Box::new(Sink(shared.clone())));
        surface.update_feedback("cam1", "cam2");
        let states = shared.lock().unwrap();
        let cam1 = states.iter().find(|(c, _)| c == "osc:/cam/1").unwrap();
        assert!(cam1.1, "cam1 on program lights its button");
    }

    #[test]
    fn mapping_config_serializes() {
        let cfg = mapping();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: MappingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn surfaces_cannot_go_live_deliberately() {
        // The action set deliberately has no ToLive: going live requires
        // the console (safety friction). Compile-time proof by absence; at
        // runtime the ToRehearsal action works.
        let mut engine = FakeEngine::new();
        let mut surface =
            SurfaceManager::new("test", mapping(), Box::new(RecordingFeedback::default()));
        let out = surface.handle(
            SurfaceControl::Osc("/mode/rehearsal".into()),
            None,
            &mut engine,
        );
        assert_eq!(out, SurfaceOutcome::Applied(SurfaceAction::ToRehearsal));
        assert!(engine.calls.contains(&"to_rehearsal".to_string()));
        // Mode enum still has Live for the console to use.
        assert!(OperationMode::Live.is_live());
    }

    #[cfg(feature = "osc")]
    #[test]
    fn osc_surface_parses_malformed_datagrams_without_crashing() {
        // parse path: well-formed OSC round-trips, garbage is rejected.
        use tpt_av_control_osc::OscServer;
        use tpt_av_control_osc::{OscArg, OscMessage};
        let msg = OscMessage::new("/go", &[OscArg::Float(1.0)]).unwrap();
        let bytes = msg.encode();
        let parsed = OscServer::parse_bytes(&bytes).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].address, "/go");
        // Garbage never panics (returns an error).
        assert!(OscServer::parse_bytes(&[0u8; 16]).is_err());
        assert!(OscServer::parse_bytes(&[]).is_err());
    }
}
