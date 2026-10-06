//! Real-time audio mixing engine (spec 8).
//!
//! Design constraints (spec 3.1):
//! - **no unbounded allocation on the render path**: every buffer is
//!   allocated once in [`Mixer::new`] / [`Mixer::add_source`];
//!   [`Mixer::render_block`] allocates nothing (asserted by the RT-safety
//!   tests using a tracking allocator),
//! - **no blocking I/O on the live signal path**: `render_block` is pure
//!   computation over caller-provided sample blocks,
//! - **gain/mute/pan changes are ramped, not stepped** (spec 8): every
//!   change interpolates over its ramp duration; a zero-length ramp is only
//!   produced when the operator explicitly requests an instant cut.
//!
//! Sample layout is interleaved stereo (`[L, R, L, R, ...]`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::time::Duration;

use tpt_app_live_production_model::ids::{BusId, SourceId};

/// Mixer errors.
#[derive(Debug, thiserror::Error)]
pub enum MixerError {
    /// The referenced source has no channel strip.
    #[error("unknown source '{0}'")]
    UnknownSource(SourceId),
    /// The referenced bus does not exist.
    #[error("unknown bus '{0}'")]
    UnknownBus(BusId),
    /// A parameter value was outside its accepted range.
    #[error("invalid {parameter}: {value} ({reason})")]
    InvalidParameter {
        /// Which parameter.
        parameter: &'static str,
        /// The offending value.
        value: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A non-finite (NaN/infinite) gain or pan was supplied.
    #[error("non-finite {parameter}")]
    NonFinite {
        /// Which parameter.
        parameter: &'static str,
    },
}

/// A bus meter reading.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Meter {
    /// Peak absolute sample level in the last rendered block.
    pub peak: f32,
    /// RMS level over the last rendered block.
    pub rms: f32,
}

#[derive(Debug, Clone)]
struct Ramp {
    /// Amplitude at ramp start.
    from: f32,
    /// Amplitude at ramp end.
    to: f32,
    /// Frames remaining in the ramp.
    frames_left: u64,
    /// Total frames of the ramp.
    frames_total: u64,
}

impl Ramp {
    fn instant(target: f32) -> Self {
        Ramp {
            from: target,
            to: target,
            frames_left: 0,
            frames_total: 0,
        }
    }

    /// Advances by `frames` and returns the amplitude at the *end* of the
    /// stepped span (the value to apply across the whole block — ramp
    /// resolution is per block, which at 128 frames / 48 kHz is sub-ms).
    fn step(&mut self, frames: usize) -> f32 {
        if self.frames_left == 0 {
            return self.to;
        }
        let stepped = self.frames_left.saturating_sub(frames as u64);
        self.frames_left = stepped;
        let progress = if self.frames_total == 0 {
            1.0
        } else {
            1.0 - (stepped as f32 / self.frames_total as f32)
        };
        let g = self.from + (self.to - self.from) * progress.min(1.0);
        if stepped == 0 {
            self.from = self.to;
        }
        g
    }
}

#[derive(Debug, Clone)]
struct Strip {
    source: SourceId,
    /// Configured gain in dB.
    gain_db: f64,
    muted: bool,
    pan: f64,
    /// Effective linear amplitude ramp (gain incl. mute).
    ramp: Ramp,
    /// Pan ramp (left, right weights).
    pan_ramp: Option<(Ramp, Ramp)>,
}

impl Strip {
    fn effective_target(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            db_to_linear(self.gain_db)
        }
    }
}

#[derive(Debug, Clone)]
struct BusState {
    id: BusId,
    /// Accumulator, interleaved stereo, `block_frames * 2` samples.
    accum: Vec<f32>,
    meter: Meter,
    /// Indexes (into `Mixer::strips`) of strips feeding this bus.
    /// Setup-time state only — never mutated by `render_block`.
    strips: Vec<usize>,
}

/// Per-bus output handed back by [`Mixer::render_block`].
#[derive(Debug)]
pub struct BusOut<'a> {
    /// Bus id.
    pub id: &'a BusId,
    /// Interleaved stereo samples for this block.
    pub samples: &'a [f32],
    /// Meter state after this block.
    pub meter: Meter,
}

/// Input sample blocks for one render pass, per source.
///
/// All slices should be exactly `block_frames * 2` interleaved samples; a
/// short slice is padded with silence (a malformed input must never panic
/// or glitch the program bus — spec 20).
pub struct RenderInputs<'a> {
    blocks: Vec<(&'a SourceId, &'a [f32])>,
}

impl<'a> RenderInputs<'a> {
    /// Creates an empty input set (sources without inputs render silence).
    pub fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    /// Adds the input block for a source.
    pub fn push(&mut self, source: &'a SourceId, samples: &'a [f32]) {
        self.blocks.push((source, samples));
    }

    fn get(&self, source: &SourceId) -> Option<&'a [f32]> {
        self.blocks
            .iter()
            .find(|(id, _)| *id == source)
            .map(|(_, samples)| *samples)
    }
}

impl Default for RenderInputs<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads an interleaved sample, padding with silence for malformed short
/// blocks.
fn input_sample(block: Option<&[f32]>, index: usize) -> f32 {
    block.and_then(|b| b.get(index)).copied().unwrap_or(0.0)
}

/// The real-time audio mixer.
#[derive(Debug, Clone)]
pub struct Mixer {
    sample_rate: u32,
    block_frames: usize,
    default_ramp: Duration,
    strips: Vec<Strip>,
    strip_index: HashMap<SourceId, usize>,
    buses: Vec<BusState>,
    bus_index: HashMap<BusId, usize>,
    /// Per-strip gain/pan scratch written during the ramp pass of
    /// [`Mixer::render_block`]. Preallocated at setup; never reallocated on
    /// the render path.
    gain_scratch: Vec<f32>,
    pan_scratch: Vec<(f32, f32)>,
}

/// Linear gain from dB.
pub fn db_to_linear(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// Constant-power pan weights for `pan` in -1.0..=1.0.
pub fn pan_weights(pan: f64) -> (f32, f32) {
    let theta = ((pan + 1.0) * 0.25) * std::f64::consts::PI; // 0..=PI/2
    (theta.cos() as f32, theta.sin() as f32)
}

fn ramp_frames(ramp: Duration, sample_rate: u32) -> u64 {
    (ramp.as_secs_f64() * f64::from(sample_rate)).ceil() as u64
}

impl Mixer {
    /// Maximum gain magnitude the mixer accepts on any path.
    pub const MAX_GAIN_DB: f64 = 12.0;
    /// Minimum gain the mixer accepts (effectively silence).
    pub const MIN_GAIN_DB: f64 = -120.0;

    /// Creates a mixer. `block_frames` is the fixed render block size.
    pub fn new(
        sample_rate: u32,
        block_frames: usize,
        default_ramp: Duration,
    ) -> Result<Self, MixerError> {
        if sample_rate == 0 {
            return Err(MixerError::InvalidParameter {
                parameter: "sample_rate",
                value: "0".to_string(),
                reason: "must be greater than zero",
            });
        }
        if block_frames == 0 || block_frames > 8192 {
            return Err(MixerError::InvalidParameter {
                parameter: "block_frames",
                value: block_frames.to_string(),
                reason: "must be in 1..=8192",
            });
        }
        Ok(Self {
            sample_rate,
            block_frames,
            default_ramp,
            strips: Vec::new(),
            strip_index: HashMap::new(),
            buses: Vec::new(),
            bus_index: HashMap::new(),
            gain_scratch: Vec::new(),
            pan_scratch: Vec::new(),
        })
    }

    /// Sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Block size in frames.
    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    /// Default ramp applied when a caller passes the show default.
    pub fn default_ramp(&self) -> Duration {
        self.default_ramp
    }

    /// Adds a bus (program first, then aux — spec 8).
    pub fn add_bus(&mut self, id: BusId) -> Result<(), MixerError> {
        if self.bus_index.contains_key(&id) {
            return Err(MixerError::InvalidParameter {
                parameter: "bus",
                value: id.0.clone(),
                reason: "already exists",
            });
        }
        self.bus_index.insert(id.clone(), self.buses.len());
        self.buses.push(BusState {
            id,
            accum: vec![0.0; self.block_frames * 2],
            meter: Meter::default(),
            strips: Vec::new(),
        });
        Ok(())
    }

    /// Number of buses.
    pub fn bus_count(&self) -> usize {
        self.buses.len()
    }

    /// Adds a channel strip for a source (silent, unpatched, 0 dB).
    pub fn add_source(&mut self, source: SourceId) -> Result<(), MixerError> {
        if self.strip_index.contains_key(&source) {
            return Err(MixerError::InvalidParameter {
                parameter: "source",
                value: source.0.clone(),
                reason: "already exists",
            });
        }
        let index = self.strips.len();
        self.gain_scratch.push(0.0);
        self.pan_scratch.push((1.0, 1.0));
        self.strip_index.insert(source.clone(), index);
        self.strips.push(Strip {
            source,
            gain_db: 0.0,
            muted: false,
            pan: 0.0,
            ramp: Ramp::instant(1.0),
            pan_ramp: None,
        });
        Ok(())
    }

    /// Removes a channel strip and its bus patches.
    pub fn remove_source(&mut self, source: &SourceId) -> Result<(), MixerError> {
        let index = self.take_strip_index(source)?;
        self.strips.remove(index);
        self.gain_scratch.remove(index);
        self.pan_scratch.remove(index);
        self.strip_index.clear();
        for (i, s) in self.strips.iter().enumerate() {
            self.strip_index.insert(s.source.clone(), i);
        }
        for bus in &mut self.buses {
            bus.strips.retain(|&i| i != index);
            for i in &mut bus.strips {
                if *i > index {
                    *i -= 1;
                }
            }
        }
        Ok(())
    }

    fn take_strip_index(&self, source: &SourceId) -> Result<usize, MixerError> {
        self.strip_index
            .get(source)
            .copied()
            .ok_or_else(|| MixerError::UnknownSource(source.clone()))
    }

    fn strip_mut(&mut self, source: &SourceId) -> Result<&mut Strip, MixerError> {
        let index = self.take_strip_index(source)?;
        Ok(&mut self.strips[index])
    }

    /// Begins an amplitude ramp on `strip` from wherever the previous ramp
    /// currently is (mid-ramp changes never jump).
    fn begin_ramp(strip: &mut Strip, target: f32, ramp: Duration, sample_rate: u32) {
        let current = if strip.ramp.frames_left == 0 || strip.ramp.frames_total == 0 {
            strip.ramp.to
        } else {
            let progress = 1.0 - (strip.ramp.frames_left as f32 / strip.ramp.frames_total as f32);
            strip.ramp.from + (strip.ramp.to - strip.ramp.from) * progress
        };
        let frames = ramp_frames(ramp, sample_rate);
        strip.ramp = if frames == 0 {
            Ramp::instant(target)
        } else {
            Ramp {
                from: current,
                to: target,
                frames_left: frames,
                frames_total: frames,
            }
        };
    }

    /// Applies a ramped gain change (spec 8). Rejects out-of-range and
    /// non-finite gains.
    pub fn set_gain(
        &mut self,
        source: &SourceId,
        gain_db: f64,
        ramp: Duration,
    ) -> Result<(), MixerError> {
        if !gain_db.is_finite() {
            return Err(MixerError::NonFinite { parameter: "gain" });
        }
        if !(Self::MIN_GAIN_DB..=Self::MAX_GAIN_DB).contains(&gain_db) {
            return Err(MixerError::InvalidParameter {
                parameter: "gain_db",
                value: gain_db.to_string(),
                reason: "out of range",
            });
        }
        let sample_rate = self.sample_rate;
        let strip = self.strip_mut(source)?;
        strip.gain_db = gain_db;
        let target = strip.effective_target();
        Self::begin_ramp(strip, target, ramp, sample_rate);
        Ok(())
    }

    /// Applies a ramped mute/unmute (spec 8). Muting ramps to silence;
    /// unmuting ramps back to the strip's configured gain.
    pub fn set_mute(
        &mut self,
        source: &SourceId,
        muted: bool,
        ramp: Duration,
    ) -> Result<(), MixerError> {
        let sample_rate = self.sample_rate;
        let strip = self.strip_mut(source)?;
        strip.muted = muted;
        let target = strip.effective_target();
        Self::begin_ramp(strip, target, ramp, sample_rate);
        Ok(())
    }

    /// Applies a ramped pan change (constant-power law, -1.0..=1.0).
    pub fn set_pan(
        &mut self,
        source: &SourceId,
        pan: f64,
        ramp: Duration,
    ) -> Result<(), MixerError> {
        if !pan.is_finite() {
            return Err(MixerError::NonFinite { parameter: "pan" });
        }
        if !(-1.0..=1.0).contains(&pan) {
            return Err(MixerError::InvalidParameter {
                parameter: "pan",
                value: pan.to_string(),
                reason: "must be within -1.0..=1.0",
            });
        }
        let sample_rate = self.sample_rate;
        let strip = self.strip_mut(source)?;
        strip.pan = pan;
        let (l, r) = pan_weights(pan);
        let current = match &strip.pan_ramp {
            Some((lp, rp)) => {
                if lp.frames_left == 0 || lp.frames_total == 0 {
                    (lp.to, rp.to)
                } else {
                    let progress = 1.0 - (lp.frames_left as f32 / lp.frames_total as f32);
                    (
                        lp.from + (lp.to - lp.from) * progress,
                        rp.from + (rp.to - rp.from) * progress,
                    )
                }
            }
            None => pan_weights(strip.pan),
        };
        let frames = ramp_frames(ramp, sample_rate);
        strip.pan_ramp = Some((
            Ramp {
                from: current.0,
                to: l,
                frames_left: frames,
                frames_total: frames,
            },
            Ramp {
                from: current.1,
                to: r,
                frames_left: frames,
                frames_total: frames,
            },
        ));
        Ok(())
    }

    /// Patches a source into a bus.
    pub fn patch(&mut self, source: &SourceId, bus: &BusId) -> Result<(), MixerError> {
        let bus_idx = *self
            .bus_index
            .get(bus)
            .ok_or_else(|| MixerError::UnknownBus(bus.clone()))?;
        let strip_idx = self.take_strip_index(source)?;
        let membership = &mut self.buses[bus_idx].strips;
        if !membership.contains(&strip_idx) {
            membership.push(strip_idx);
        }
        Ok(())
    }

    /// Removes a source from every bus.
    pub fn unpatch_all(&mut self, source: &SourceId) -> Result<(), MixerError> {
        let strip_idx = self.take_strip_index(source)?;
        for bus in &mut self.buses {
            bus.strips.retain(|&i| i != strip_idx);
        }
        Ok(())
    }

    /// The buses a source currently feeds, in bus-creation order.
    pub fn patches(&self, source: &SourceId) -> Result<Vec<BusId>, MixerError> {
        let strip_idx = self.take_strip_index(source)?;
        let mut out = Vec::new();
        for bus in &self.buses {
            if bus.strips.contains(&strip_idx) {
                out.push(bus.id.clone());
            }
        }
        Ok(out)
    }

    /// Current strip state: `(gain_db, muted, pan)`.
    pub fn strip_state(&self, source: &SourceId) -> Result<(f64, bool, f64), MixerError> {
        let index = self.take_strip_index(source)?;
        let s = &self.strips[index];
        Ok((s.gain_db, s.muted, s.pan))
    }

    /// Last meter reading for a bus.
    pub fn meter(&self, bus: &BusId) -> Result<Meter, MixerError> {
        let index = self
            .bus_index
            .get(bus)
            .copied()
            .ok_or_else(|| MixerError::UnknownBus(bus.clone()))?;
        Ok(self.buses[index].meter)
    }

    /// Renders one block.
    ///
    /// `inputs` provides the interleaved stereo block per source; missing
    /// sources render silence. `outputs` receives one interleaved stereo
    /// slice per bus, in bus-creation order; short slices are truncated to
    /// what is available (never panics on host buffer mistakes).
    ///
    /// Allocates nothing.
    pub fn render_block(&mut self, inputs: &RenderInputs<'_>, outputs: &mut [&mut [f32]]) {
        debug_assert_eq!(outputs.len(), self.buses.len(), "one output slice per bus");
        let frames = self.block_frames;
        let len = frames * 2;

        // Pass 1: advance every strip's ramps, snapshotting per-strip
        // gain/pan into preallocated scratch.
        for i in 0..self.strips.len() {
            let strip = &mut self.strips[i];
            self.gain_scratch[i] = strip.ramp.step(frames);
            if let Some((l, r)) = strip.pan_ramp.as_mut() {
                let (gl, gr) = (l.step(frames), r.step(frames));
                self.pan_scratch[i] = (gl, gr);
            }
        }

        // Pass 2: mix strips into their buses.
        let strips = &self.strips;
        let gains = &self.gain_scratch;
        let pans = &self.pan_scratch;
        for bus in &mut self.buses {
            for sample in bus.accum.iter_mut() {
                *sample = 0.0;
            }
            for &strip_idx in &bus.strips {
                let strip = &strips[strip_idx];
                let g = gains[strip_idx];
                let (pl, pr) = pans[strip_idx];
                let block = inputs.get(&strip.source);
                for f in 0..frames {
                    bus.accum[f * 2] += input_sample(block, f * 2) * g * pl;
                    bus.accum[f * 2 + 1] += input_sample(block, f * 2 + 1) * g * pr;
                }
            }
        }

        // Copy out + meter.
        for (bi, bus) in self.buses.iter_mut().enumerate() {
            if let Some(out) = outputs.get_mut(bi) {
                let n = out.len().min(len);
                let slice = &mut out[..n];
                let mut peak = 0.0f32;
                let mut sum_squares = 0.0f64;
                for (o, a) in slice.iter_mut().zip(bus.accum.iter()) {
                    *o = *a;
                    let abs = a.abs();
                    if abs > peak {
                        peak = abs;
                    }
                    sum_squares += (*a as f64) * (*a as f64);
                }
                bus.meter.peak = peak;
                bus.meter.rms = (sum_squares / n.max(1) as f64).sqrt() as f32;
            }
        }
    }

    /// True when every strip's ramps have fully settled (tests and the
    /// engine use this to know ramp work can be skipped).
    pub fn all_ramps_settled(&self) -> bool {
        self.strips.iter().all(|s| {
            s.ramp.frames_left == 0
                && s.pan_ramp
                    .as_ref()
                    .is_none_or(|(l, r)| l.frames_left == 0 && r.frames_left == 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(name: &str) -> SourceId {
        SourceId::new(name)
    }

    fn bid(name: &str) -> BusId {
        BusId::new(name)
    }

    fn mixer() -> Mixer {
        let mut m = Mixer::new(48_000, 128, Duration::from_millis(25)).unwrap();
        m.add_bus(bid("program")).unwrap();
        m.add_bus(bid("aux")).unwrap();
        m.add_source(sid("mic1")).unwrap();
        m.patch(&sid("mic1"), &bid("program")).unwrap();
        m
    }

    fn sine(freq: f32, frames: usize, sample_rate: u32) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = (2.0 * std::f32::consts::PI * freq * n as f32 / sample_rate as f32).sin() * 0.5;
            v.push(s);
            v.push(s);
        }
        v
    }

    // ---- valid behaviour -----------------------------------------------

    #[test]
    fn renders_signal_to_program_bus_with_metering() {
        let mut m = mixer();
        let input = sine(1000.0, 128, 48_000);
        let mut inputs = RenderInputs::new();
        let mic1 = sid("mic1");
        inputs.push(&mic1, &input);
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        let meter = m.meter(&bid("program")).unwrap();
        assert!(
            meter.peak > 0.4,
            "peak should track the 0.5 sine, got {}",
            meter.peak
        );
        assert!(
            meter.rms > 0.2 && meter.rms < 0.5,
            "rms sane, got {}",
            meter.rms
        );
        // Aux receives nothing (mic1 is only patched to program).
        assert_eq!(m.meter(&bid("aux")).unwrap().peak, 0.0);
    }

    #[test]
    fn ramped_gain_change_is_click_free() {
        let mut m = mixer();
        let input = sine(1000.0, 512, 48_000); // four blocks
        let mic1 = sid("mic1");
        // 4 blocks x 128 frames x 2 channels.
        let mut program = vec![0.0f32; 1024];
        let mut aux = vec![0.0f32; 1024];

        // Steady state at 0 dB, then a big ramped drop. Each block consumes
        // its own quarter of the input so the source signal itself is
        // continuous across the boundary.
        m.set_gain(&mic1, 0.0, Duration::ZERO).unwrap();
        for (i, chunk) in input.chunks(256).enumerate() {
            let mut inputs = RenderInputs::new();
            inputs.push(&mic1, chunk);
            if i == 2 {
                // Drop lands as block 3 begins, ramped over ~10 blocks.
                m.set_gain(&mic1, -12.0, Duration::from_millis(25)).unwrap();
            }
            // One block is 128 frames = 256 interleaved samples.
            let mut outs: [&mut [f32]; 2] = [
                &mut program[i * 256..(i + 1) * 256],
                &mut aux[i * 256..(i + 1) * 256],
            ];
            m.render_block(&inputs, &mut outs);
        }

        let max_delta = program
            .windows(2)
            .map(|p| (p[0] - p[1]).abs())
            .fold(0.0f32, f32::max);
        // The uninterrupted sine at unity gain has a per-sample slope of
        // ~0.065; staying near that means the ramp introduced no click.
        assert!(
            max_delta < 0.15,
            "inter-sample jump {max_delta} exceeds click threshold"
        );
    }

    #[test]
    fn instant_mute_is_silent_then_unmute_restores() {
        let mut m = mixer();
        let input = sine(440.0, 128, 48_000);
        let mut inputs = RenderInputs::new();
        let mic1 = sid("mic1");
        inputs.push(&mic1, &input);
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];

        m.set_mute(&sid("mic1"), true, Duration::ZERO).unwrap();
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        assert_eq!(
            m.meter(&bid("program")).unwrap().peak,
            0.0,
            "instant mute is silent"
        );

        m.set_mute(&sid("mic1"), false, Duration::ZERO).unwrap();
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        assert!(
            m.meter(&bid("program")).unwrap().peak > 0.3,
            "unmute restores signal"
        );
    }

    #[test]
    fn ramped_mute_attenuates_progressively() {
        let mut m = mixer();
        m.set_gain(&sid("mic1"), 6.0, Duration::ZERO).unwrap();
        let input = sine(440.0, 128, 48_000);
        let mut inputs = RenderInputs::new();
        let mic1 = sid("mic1");
        inputs.push(&mic1, &input);
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);

        // 25 ms = 1200 frames ≈ 10 blocks at 128 frames.
        m.set_mute(&sid("mic1"), true, Duration::from_millis(25))
            .unwrap();
        assert!(!m.all_ramps_settled());
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        let mid = m.meter(&bid("program")).unwrap().peak;
        assert!(mid > 0.0 && mid < 1.5, "mid-ramp attenuation, got {mid}");

        for _ in 0..12 {
            let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
            m.render_block(&inputs, &mut outs);
        }
        assert!(m.all_ramps_settled());
        assert_eq!(m.meter(&bid("program")).unwrap().peak, 0.0);
    }

    #[test]
    fn pan_moves_energy_between_channels() {
        let mut m = mixer();
        m.set_pan(&sid("mic1"), 1.0, Duration::ZERO).unwrap(); // hard right
        let input = sine(440.0, 128, 48_000);
        let mut inputs = RenderInputs::new();
        let mic1 = sid("mic1");
        inputs.push(&mic1, &input);
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        let peak_r = program.chunks(2).map(|c| c[1].abs()).fold(0.0f32, f32::max);
        let peak_l = program.chunks(2).map(|c| c[0].abs()).fold(0.0f32, f32::max);
        assert!(peak_r > 0.3, "right channel loud, got {peak_r}");
        assert!(peak_l < 0.01, "left channel silent, got {peak_l}");
    }

    #[test]
    fn bus_assignment_changes_routing() {
        let mut m = mixer();
        m.patch(&sid("mic1"), &bid("aux")).unwrap();
        assert_eq!(
            m.patches(&sid("mic1")).unwrap(),
            vec![bid("program"), bid("aux")]
        );
        m.unpatch_all(&sid("mic1")).unwrap();
        assert!(m.patches(&sid("mic1")).unwrap().is_empty());
        let input = sine(440.0, 128, 48_000);
        let mut inputs = RenderInputs::new();
        let mic1 = sid("mic1");
        inputs.push(&mic1, &input);
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        assert_eq!(m.meter(&bid("program")).unwrap().peak, 0.0);
    }

    #[test]
    fn missing_input_renders_silence_not_panic() {
        let mut m = mixer();
        let inputs = RenderInputs::new();
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        assert_eq!(m.meter(&bid("program")).unwrap().peak, 0.0);
    }

    #[test]
    fn short_input_block_pads_with_silence() {
        let mut m = mixer();
        let mut inputs = RenderInputs::new();
        let mic1 = sid("mic1");
        inputs.push(&mic1, &[0.5, 0.5]); // malformed: 1 frame, not 128
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs); // must not panic
        assert!(m.meter(&bid("program")).unwrap().peak <= 0.5);
    }

    // ---- invalid input ---------------------------------------------------

    #[test]
    fn unknown_source_and_bus_are_rejected() {
        let mut m = mixer();
        assert!(matches!(
            m.set_gain(&sid("ghost"), 0.0, Duration::ZERO),
            Err(MixerError::UnknownSource(_))
        ));
        assert!(matches!(
            m.set_mute(&sid("ghost"), true, Duration::ZERO),
            Err(MixerError::UnknownSource(_))
        ));
        assert!(matches!(
            m.patch(&sid("mic1"), &bid("ghost")),
            Err(MixerError::UnknownBus(_))
        ));
        assert!(matches!(
            m.meter(&bid("ghost")),
            Err(MixerError::UnknownBus(_))
        ));
    }

    #[test]
    fn non_finite_and_out_of_range_params_are_rejected() {
        let mut m = mixer();
        assert!(matches!(
            m.set_gain(&sid("mic1"), f64::NAN, Duration::ZERO),
            Err(MixerError::NonFinite { .. })
        ));
        assert!(matches!(
            m.set_gain(&sid("mic1"), f64::INFINITY, Duration::ZERO),
            Err(MixerError::NonFinite { .. })
        ));
        assert!(matches!(
            m.set_gain(&sid("mic1"), 50.0, Duration::ZERO),
            Err(MixerError::InvalidParameter { .. })
        ));
        assert!(matches!(
            m.set_pan(&sid("mic1"), 1.5, Duration::ZERO),
            Err(MixerError::InvalidParameter { .. })
        ));
        assert!(matches!(
            m.set_pan(&sid("mic1"), f64::NAN, Duration::ZERO),
            Err(MixerError::NonFinite { .. })
        ));
        assert_eq!(m.strip_state(&sid("mic1")).unwrap(), (0.0, false, 0.0));
    }

    #[test]
    fn boundary_gains_accepted() {
        let mut m = mixer();
        m.set_gain(&sid("mic1"), Mixer::MAX_GAIN_DB, Duration::ZERO)
            .unwrap();
        assert_eq!(m.strip_state(&sid("mic1")).unwrap().0, Mixer::MAX_GAIN_DB);
        m.set_gain(&sid("mic1"), Mixer::MIN_GAIN_DB, Duration::ZERO)
            .unwrap();
        assert_eq!(m.strip_state(&sid("mic1")).unwrap().0, Mixer::MIN_GAIN_DB);
    }

    #[test]
    fn pan_boundaries_accepted() {
        let mut m = mixer();
        m.set_pan(&sid("mic1"), -1.0, Duration::ZERO).unwrap();
        m.set_pan(&sid("mic1"), 1.0, Duration::ZERO).unwrap();
        m.set_pan(&sid("mic1"), 0.0, Duration::ZERO).unwrap();
    }

    #[test]
    fn duplicate_adds_are_rejected() {
        let mut m = mixer();
        assert!(m.add_source(sid("mic1")).is_err());
        assert!(m.add_bus(bid("program")).is_err());
    }

    #[test]
    fn zero_sample_rate_and_bad_block_size_rejected() {
        assert!(Mixer::new(0, 128, Duration::from_millis(5)).is_err());
        assert!(Mixer::new(48_000, 0, Duration::from_millis(5)).is_err());
        assert!(Mixer::new(48_000, 100_000, Duration::from_millis(5)).is_err());
    }

    #[test]
    fn remove_source_keeps_mixer_consistent() {
        let mut m = mixer();
        m.add_source(sid("cam2_audio")).unwrap();
        m.patch(&sid("cam2_audio"), &bid("aux")).unwrap();
        m.remove_source(&sid("cam2_audio")).unwrap();
        assert!(matches!(
            m.strip_state(&sid("cam2_audio")),
            Err(MixerError::UnknownSource(_))
        ));
        let input = sine(440.0, 128, 48_000);
        let mut inputs = RenderInputs::new();
        let mic1 = sid("mic1");
        inputs.push(&mic1, &input);
        let mut program = vec![0.0f32; 256];
        let mut aux = vec![0.0f32; 256];
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        m.render_block(&inputs, &mut outs);
        assert!(m.meter(&bid("program")).unwrap().peak > 0.3);
    }
}
