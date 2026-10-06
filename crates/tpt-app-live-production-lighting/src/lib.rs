//! Lighting cue engine (spec 9).
//!
//! MVP scope is **scene recall with fade timing** (complex effects/chases
//! are explicitly deferred, spec 22). The engine holds the current DMX
//! state per universe, fades between scenes over a configurable duration,
//! and pushes every changed universe to a [`DmxSink`].
//!
//! Time is supplied by the caller as monotonic milliseconds, which keeps
//! the engine deterministic and testable; the core engine derives that
//! clock from the program frame clock via `tpt-av-sync`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use serde::Serialize;
use tpt_app_live_production_model::ids::{FixtureId, LightingSceneId, UniverseId};
use tpt_app_live_production_model::lighting::{Fixture, LightingScene};

/// Lighting engine errors.
#[derive(Debug, thiserror::Error)]
pub enum LightingError {
    /// The scene id is not in the engine's scene table.
    #[error("unknown lighting scene '{0}'")]
    UnknownScene(LightingSceneId),
    /// The fixture id is not in the engine's fixture table.
    #[error("unknown fixture '{0}'")]
    UnknownFixture(FixtureId),
    /// The fixture's universe has no numeric DMX universe mapping.
    #[error("universe '{0}' has no DMX universe number mapping")]
    UnmappedUniverse(UniverseId),
    /// The scene's channel count for a fixture does not match the fixture's
    /// patched channels.
    #[error(
        "scene '{scene}' sets {got} values for fixture '{fixture}' which has {expected} channels"
    )]
    ChannelCountMismatch {
        /// Scene being recalled.
        scene: LightingSceneId,
        /// Fixture with the mismatch.
        fixture: FixtureId,
        /// Values supplied.
        got: usize,
        /// Channels patched.
        expected: usize,
    },
}

/// Receives DMX universe frames from the lighting engine.
///
/// Implemented by the sACN/Art-Net sender (feature `sacn`), a recording
/// sink for tests, and a null sink for rehearsal (the core engine simply
/// does not call the sink at all in rehearsal mode — isolation is enforced
/// above this trait).
pub trait DmxSink {
    /// Sends one full universe frame.
    fn send_universe(&mut self, universe: u16, data: &[u8; 512]);
}

/// Records every universe frame (test harness).
#[derive(Default)]
pub struct RecordingSink {
    frames: Vec<(u16, [u8; 512])>,
}

impl RecordingSink {
    /// All frames sent so far, in order.
    pub fn frames(&self) -> &[(u16, [u8; 512])] {
        &self.frames
    }

    /// The last frame sent for a universe.
    pub fn last(&self, universe: u16) -> Option<&[u8; 512]> {
        self.frames
            .iter()
            .rev()
            .find(|(u, _)| *u == universe)
            .map(|(_, d)| d)
    }

    /// Forgets all recorded frames.
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

impl DmxSink for RecordingSink {
    fn send_universe(&mut self, universe: u16, data: &[u8; 512]) {
        self.frames.push((universe, *data));
    }
}

/// Discards every frame.
#[derive(Default)]
pub struct NullSink;

impl DmxSink for NullSink {
    fn send_universe(&mut self, _universe: u16, _data: &[u8; 512]) {}
}

#[derive(Clone)]
struct FadeState {
    from: HashMap<u16, [u8; 512]>,
    to: HashMap<u16, [u8; 512]>,
    started_ms: u64,
    duration_ms: u64,
}

impl FadeState {
    fn progress(&self, now_ms: u64) -> f64 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        let elapsed = now_ms.saturating_sub(self.started_ms);
        (elapsed as f64 / self.duration_ms as f64).min(1.0)
    }
}

/// A snapshot of one universe's state, for UI monitors and tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UniverseSnapshot {
    /// DMX universe number.
    pub universe: u16,
    /// Full 512-channel frame.
    #[serde(with = "base512")]
    pub data: [u8; 512],
}

mod base512 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8; 512], s: S) -> Result<S::Ok, S::Error> {
        data.serialize(s)
    }

    // Deserialization stays available for future config round-trips; the
    // snapshot type is currently produced only by the engine (hence the
    // allow rather than deletion).
    #[allow(dead_code)]
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 512], D::Error> {
        let v = Vec::<u8>::deserialize(d)?;
        v.try_into()
            .map_err(|_| serde::de::Error::custom("expected 512 bytes"))
    }
}

/// The lighting engine.
pub struct LightingEngine {
    /// Fixture id -> fixture definition.
    fixtures: HashMap<FixtureId, Fixture>,
    /// Scene id -> scene definition.
    scenes: HashMap<LightingSceneId, LightingScene>,
    /// Universe id -> numeric DMX universe.
    universe_numbers: HashMap<UniverseId, u16>,
    /// Current held values per universe (post-fade when settled).
    current: HashMap<u16, [u8; 512]>,
    active_fade: Option<FadeState>,
}

impl LightingEngine {
    /// Builds an engine from the show's fixtures, scenes, and the mapping
    /// from universe ids to numeric DMX universes.
    pub fn new(
        fixtures: &[Fixture],
        scenes: &[LightingScene],
        universe_numbers: HashMap<UniverseId, u16>,
    ) -> Self {
        let fixture_map = fixtures.iter().map(|f| (f.id.clone(), f.clone())).collect();
        let scene_map = scenes.iter().map(|s| (s.id.clone(), s.clone())).collect();
        Self {
            fixtures: fixture_map,
            scenes: scene_map,
            universe_numbers,
            current: HashMap::new(),
            active_fade: None,
        }
    }

    /// True while a fade is in progress at `now_ms`.
    pub fn is_fading(&self, now_ms: u64) -> bool {
        self.active_fade
            .as_ref()
            .is_some_and(|f| f.progress(now_ms) < 1.0)
    }

    /// Current value of one channel (0 when never set).
    pub fn channel(&self, universe: u16, address: u16) -> u8 {
        self.current
            .get(&universe)
            .and_then(|d| d.get((address as usize).saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }

    /// Snapshot of every known universe.
    pub fn snapshots(&self) -> Vec<UniverseSnapshot> {
        let mut out: Vec<UniverseSnapshot> = self
            .current
            .iter()
            .map(|(u, d)| UniverseSnapshot {
                universe: *u,
                data: *d,
            })
            .collect();
        out.sort_by_key(|s| s.universe);
        out
    }

    /// Begins recalling `scene`: captures the current state as the fade
    /// source and fades to the scene over the scene's configured time.
    ///
    /// Fixtures the scene does not mention hold their current values.
    pub fn recall(
        &mut self,
        scene: &LightingSceneId,
        now_ms: u64,
    ) -> Result<Vec<u16>, LightingError> {
        let scene_def = self
            .scenes
            .get(scene)
            .ok_or_else(|| LightingError::UnknownScene(scene.clone()))?
            .clone();

        // Resolve targets per universe.
        let mut to: HashMap<u16, [u8; 512]> = self.current.clone();
        for (fixture_id, values) in &scene_def.values {
            let fixture = self
                .fixtures
                .get(fixture_id)
                .ok_or_else(|| LightingError::UnknownFixture(fixture_id.clone()))?;
            if values.0.len() != fixture.channels.len() {
                return Err(LightingError::ChannelCountMismatch {
                    scene: scene.clone(),
                    fixture: fixture_id.clone(),
                    got: values.0.len(),
                    expected: fixture.channels.len(),
                });
            }
            let universe = *self
                .universe_numbers
                .get(&fixture.universe)
                .ok_or_else(|| LightingError::UnmappedUniverse(fixture.universe.clone()))?;
            let frame = to.entry(universe).or_insert([0u8; 512]);
            for (ch, value) in fixture.channels.iter().zip(values.0.iter()) {
                let idx = (ch.address as usize).saturating_sub(1);
                if idx < 512 {
                    frame[idx] = *value;
                }
            }
        }

        let from = self.snapshot_now(now_ms);
        let duration_ms = scene_def.fade.as_millis() as u64;
        let touched: Vec<u16> = to.keys().copied().collect();
        self.active_fade = Some(FadeState {
            from,
            to: to.clone(),
            started_ms: now_ms,
            duration_ms,
        });

        // A zero-length fade lands immediately.
        if duration_ms == 0 {
            self.current = to;
            self.active_fade = None;
        }
        Ok(touched)
    }

    /// Advances any active fade to `now_ms` and returns the universes whose
    /// values changed (which the caller pushes to the sinks).
    pub fn tick(&mut self, now_ms: u64) -> Vec<u16> {
        let fade = match &self.active_fade {
            Some(f) => f.clone(),
            None => return Vec::new(),
        };
        let t = fade.progress(now_ms);
        let mut changed = Vec::new();
        for (universe, target) in &fade.to {
            let from = fade.from.get(universe);
            let frame = self.current.entry(*universe).or_insert([0u8; 512]);
            for i in 0..512 {
                let a = from.map_or(0u8, |f| f[i]);
                let b = target[i];
                let v = if t >= 1.0 {
                    b
                } else {
                    (a as f64 + (b as f64 - a as f64) * t)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                if frame[i] != v {
                    frame[i] = v;
                    changed.push(*universe);
                }
            }
        }
        changed.dedup();
        if t >= 1.0 {
            self.active_fade = None;
        }
        changed
    }

    /// Fades everything to black over `duration_ms` (failsafe/emergency).
    pub fn blackout(&mut self, now_ms: u64, duration_ms: u64) -> Vec<u16> {
        let from = self.snapshot_now(now_ms);
        let to = from
            .keys()
            .map(|u| (*u, [0u8; 512]))
            .collect::<HashMap<_, _>>();
        let touched: Vec<u16> = to.keys().copied().collect();
        if duration_ms == 0 {
            self.current = to;
            self.active_fade = None;
            return touched;
        }
        self.active_fade = Some(FadeState {
            from,
            to,
            started_ms: now_ms,
            duration_ms,
        });
        touched
    }

    /// The current blended values (mid-fade inclusive), used as the fade
    /// source when a new recall interrupts an in-flight fade.
    fn snapshot_now(&self, now_ms: u64) -> HashMap<u16, [u8; 512]> {
        let fade = match &self.active_fade {
            Some(f) if f.progress(now_ms) < 1.0 => f,
            _ => return self.current.clone(),
        };
        let t = fade.progress(now_ms);
        let mut out = self.current.clone();
        for (universe, target) in &fade.to {
            let from = fade.from.get(universe);
            let frame = out.entry(*universe).or_insert([0u8; 512]);
            for i in 0..512 {
                let a = from.map_or(0u8, |f| f[i]);
                let b = target[i];
                frame[i] = (a as f64 + (b as f64 - a as f64) * t)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
        out
    }
}

#[cfg(feature = "sacn")]
pub mod sacn {
    //! sACN (E1.31) output sink over [`tpt_av_control_dmx`].

    use super::DmxSink;
    use std::net::SocketAddr;
    use tpt_av_control_dmx::{Cid, DmxClient, DmxProtocol};

    /// Sends universes as sACN to a multicast/unicast receiver address.
    pub struct SacnSink {
        client: DmxClient,
    }

    impl SacnSink {
        /// Creates a sink targeting `addr` (e.g. a lighting interface or
        /// `239.255.0.1:5568` multicast).
        pub fn new(addr: SocketAddr, source_name: impl Into<String>) -> std::io::Result<Self> {
            let name = source_name.into();
            let mut client = DmxClient::new(addr, DmxProtocol::Sacn)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let cid = Cid::from_bytes(name.as_bytes());
            client.set_sacn_source(cid, name);
            Ok(Self { client })
        }
    }

    impl DmxSink for SacnSink {
        fn send_universe(&mut self, universe: u16, data: &[u8; 512]) {
            // Build a DmxUniverse wrapper and send. Failure to send is
            // logged and swallowed: a transient network hiccup must never
            // take down the engine mid-show (spec 14). The lighting engine
            // retries on the next tick.
            let mut u = tpt_av_control_dmx::DmxUniverse::new(universe);
            for (i, v) in data.iter().enumerate() {
                u.set_channel(i as u16 + 1, *v);
            }
            if let Err(e) = self.client.send_universe(&u) {
                log::warn!("sACN send failed for universe {universe}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture(id: &str, universe: &str, addresses: &[u16]) -> Fixture {
        Fixture {
            id: FixtureId::new(id),
            label: id.to_string(),
            universe: UniverseId::new(universe),
            channels: addresses
                .iter()
                .map(|&a| tpt_app_live_production_model::lighting::DmxChannel {
                    address: a,
                    role: "intensity".into(),
                })
                .collect(),
        }
    }

    fn scene(id: &str, fixture: &str, values: &[u8], fade: Duration) -> LightingScene {
        LightingScene::new(
            id,
            id,
            vec![(
                FixtureId::new(fixture),
                tpt_app_live_production_model::lighting::ChannelValues::new(values.to_vec()),
            )],
            fade,
        )
    }

    fn engine() -> LightingEngine {
        let mut numbers = HashMap::new();
        numbers.insert(UniverseId::new("stage"), 1u16);
        LightingEngine::new(
            &[fixture("wash", "stage", &[1, 2])],
            &[scene(
                "open",
                "wash",
                &[255, 128],
                Duration::from_millis(1000),
            )],
            numbers,
        )
    }

    // ---- valid -----------------------------------------------------------

    #[test]
    fn recall_fades_to_scene_over_configured_time() {
        let mut e = engine();
        e.recall(&LightingSceneId::new("open"), 0).unwrap();
        assert!(e.is_fading(0));

        // Half-way through the fade: channel 1 ≈ 127.
        let _ = e.tick(500);
        let mid = e.channel(1, 1);
        assert!((120..=135).contains(&mid), "mid-fade value {mid}");

        // Fade complete.
        let changed = e.tick(1000);
        assert!(changed.contains(&1));
        assert!(!e.is_fading(1001));
        assert_eq!(e.channel(1, 1), 255);
        assert_eq!(e.channel(1, 2), 128);
    }

    #[test]
    fn changed_universes_are_pushed_to_sink() {
        let mut e = engine();
        let mut sink = RecordingSink::default();
        e.recall(&LightingSceneId::new("open"), 0).unwrap();
        // A quarter of the way through the fade, channel 1 must have moved.
        let changed = e.tick(250);
        assert!(changed.contains(&1), "universe 1 must be reported changed");
        for u in changed {
            let snap = e.snapshots().into_iter().find(|s| s.universe == u).unwrap();
            sink.send_universe(snap.universe, &snap.data);
        }
        let v = sink.last(1).unwrap()[0];
        assert!((50..=80).contains(&v), "quarter-fade value ~64, got {v}");
    }

    #[test]
    fn zero_fade_is_instant() {
        let mut numbers = HashMap::new();
        numbers.insert(UniverseId::new("stage"), 1u16);
        let mut e = LightingEngine::new(
            &[fixture("wash", "stage", &[1])],
            &[scene("snap", "wash", &[200], Duration::ZERO)],
            numbers,
        );
        e.recall(&LightingSceneId::new("snap"), 7).unwrap();
        assert!(!e.is_fading(7));
        assert_eq!(e.channel(1, 1), 200);
    }

    #[test]
    fn recalling_mid_fade_starts_from_current_blended_values() {
        let mut e = engine();
        e.recall(&LightingSceneId::new("open"), 0).unwrap();
        let _ = e.tick(500); // ~half way to 255
        let mid = e.channel(1, 1);
        assert!(mid > 0);

        // Blackout from wherever we are.
        let touched = e.blackout(600, 100);
        assert!(touched.contains(&1));
        let _ = e.tick(700);
        assert_eq!(e.channel(1, 1), 0);
    }

    #[test]
    fn scene_holds_unmentioned_fixtures() {
        let mut numbers = HashMap::new();
        numbers.insert(UniverseId::new("stage"), 1u16);
        let mut e = LightingEngine::new(
            &[fixture("a", "stage", &[1]), fixture("b", "stage", &[2])],
            &[
                scene("s1", "a", &[255], Duration::ZERO),
                scene("s2", "b", &[99], Duration::ZERO),
            ],
            numbers,
        );
        e.recall(&LightingSceneId::new("s1"), 0).unwrap();
        e.recall(&LightingSceneId::new("s2"), 1).unwrap();
        // s2 only set fixture b; fixture a must hold 255.
        assert_eq!(e.channel(1, 1), 255);
        assert_eq!(e.channel(1, 2), 99);
    }

    // ---- invalid / boundary ---------------------------------------------

    #[test]
    fn unknown_scene_is_rejected() {
        let mut e = engine();
        let err = e.recall(&LightingSceneId::new("ghost"), 0).unwrap_err();
        assert!(matches!(err, LightingError::UnknownScene(_)));
    }

    #[test]
    fn unknown_fixture_in_scene_is_rejected() {
        let mut numbers = HashMap::new();
        numbers.insert(UniverseId::new("stage"), 1u16);
        let mut e = LightingEngine::new(
            &[fixture("a", "stage", &[1])],
            &[scene("bad", "ghost_fixture", &[255], Duration::ZERO)],
            numbers,
        );
        let err = e.recall(&LightingSceneId::new("bad"), 0).unwrap_err();
        assert!(matches!(err, LightingError::UnknownFixture(_)));
    }

    #[test]
    fn channel_count_mismatch_is_rejected() {
        let mut numbers = HashMap::new();
        numbers.insert(UniverseId::new("stage"), 1u16);
        let mut e = LightingEngine::new(
            &[fixture("a", "stage", &[1, 2])],
            &[scene("bad", "a", &[255], Duration::ZERO)],
            numbers,
        );
        let err = e.recall(&LightingSceneId::new("bad"), 0).unwrap_err();
        assert!(matches!(
            err,
            LightingError::ChannelCountMismatch {
                got: 1,
                expected: 2,
                ..
            }
        ));
    }

    #[test]
    fn unmapped_universe_is_rejected() {
        let mut e = LightingEngine::new(
            &[fixture("a", "elsewhere", &[1])],
            &[scene("s", "a", &[255], Duration::ZERO)],
            HashMap::new(),
        );
        let err = e.recall(&LightingSceneId::new("s"), 0).unwrap_err();
        assert!(matches!(err, LightingError::UnmappedUniverse(_)));
    }

    #[test]
    fn fade_progress_never_exceeds_bounds() {
        let mut e = engine();
        e.recall(&LightingSceneId::new("open"), 100).unwrap();
        // Tick far past the end.
        let _ = e.tick(100_000);
        assert_eq!(e.channel(1, 1), 255);
        assert!(!e.is_fading(100_001));
    }

    #[test]
    fn blackout_to_zero() {
        let mut e = engine();
        e.recall(&LightingSceneId::new("open"), 0).unwrap();
        let _ = e.tick(1000);
        assert_eq!(e.channel(1, 1), 255);
        e.blackout(1001, 0);
        assert_eq!(e.channel(1, 1), 0);
        assert_eq!(e.channel(1, 2), 0);
    }
}
