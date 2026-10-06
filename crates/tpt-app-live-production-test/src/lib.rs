//! Shared helpers for the TPT Live Production test suite. The actual test
//! binaries live in `tests/` (golden, chaos, latency, fuzz, rt_allocation).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use tpt_app_live_production_core::engine::{EngineConfig, LiveEngine};
use tpt_app_live_production_model::showfile::ShowFile;
use tpt_app_live_production_model::Show;

/// Repo root (this crate lives at `<root>/crates/<name>`).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate is two levels below the repo root")
        .to_path_buf()
}

/// Loads a show file from the repo's `shows/` directory.
pub fn load_show(rel: &str) -> Show {
    let path = repo_root().join(rel);
    let file = ShowFile::load(&path)
        .unwrap_or_else(|e| panic!("cannot load show {}: {e}", path.display()));
    Show::try_from(file).unwrap_or_else(|e| panic!("cannot build show {}: {e}", path.display()))
}

/// Builds a live engine from a show in the repo, in the given mode.
pub fn build_engine(show: Show) -> Arc<Mutex<LiveEngine>> {
    let engine = LiveEngine::build(show, EngineConfig::default()).expect("engine builds");
    Arc::new(Mutex::new(engine))
}

/// A tiny deterministic PRNG (xorshift64*) so fuzz corpora are reproducible
/// across platforms and runs (spec 21.5 — fuzzing, deterministic mode).
pub struct Rng(pub u64);

impl Rng {
    /// Creates a generator from a seed.
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    /// Next pseudo-random u64.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Next pseudo-random byte vector of `len` bytes.
    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| (self.next_u64() & 0xFF) as u8).collect()
    }

    /// A random mutation of `input` (flip/copy/insert/truncate).
    pub fn mutate(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = input.to_vec();
        for _ in 0..(self.next_u64() % 4 + 1) {
            if out.is_empty() {
                out.push((self.next_u64() & 0xFF) as u8);
                continue;
            }
            match self.next_u64() % 4 {
                0 => {
                    let i = (self.next_u64() as usize) % out.len();
                    out[i] = (self.next_u64() & 0xFF) as u8;
                }
                1 => {
                    let i = (self.next_u64() as usize) % out.len();
                    out.truncate(i);
                }
                2 => {
                    let i = (self.next_u64() as usize) % out.len();
                    out.insert(i, (self.next_u64() & 0xFF) as u8);
                }
                _ => {
                    let i = (self.next_u64() as usize) % out.len();
                    let j = (self.next_u64() as usize) % out.len();
                    out.swap(i, j);
                }
            }
        }
        out
    }
}
