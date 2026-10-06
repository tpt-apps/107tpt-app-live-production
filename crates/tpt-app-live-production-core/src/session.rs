//! Embedded session log store (spec 18).
//!
//! The show *file* is the primary artefact; the session log is the
//! post-show review record: session start/stop, cue-advance timestamps,
//! failsafe events. The spec asks for "SQLite (or an embedded store)" —
//! this is an append-only JSONL store: crash-safe (each line is a complete
//! record), human-readable, diffable, and trivially importable into SQLite
//! later if query depth is ever needed. Reads skip malformed lines rather
//! than failing (a partially-written tail after a crash is expected).

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// One session-log record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Wall-clock ms since UNIX epoch.
    pub ts_ms: u64,
    /// Session id (same for all events of one session).
    pub session: String,
    /// Event kind discriminator.
    pub kind: SessionEventKind,
}

/// Kinds of session events (spec 18).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventKind {
    /// A session started (show opened).
    SessionStart {
        /// Show id.
        show: String,
        /// Show name.
        name: String,
        /// Mode the session started in.
        mode: String,
    },
    /// The session ended.
    SessionEnd {
        /// Reason: normal close, crash recovery, etc.
        reason: String,
    },
    /// Mode changed mid-session.
    ModeChange {
        /// New mode.
        mode: String,
    },
    /// A cue fired.
    CueAdvance {
        /// Cue number.
        number: u32,
        /// Cue label.
        label: String,
        /// Automatic (timed/follow) vs manual.
        automatic: bool,
    },
    /// A failsafe policy fired.
    Failsafe {
        /// What was lost.
        reason: String,
        /// Which input.
        source: String,
        /// Action taken.
        action: String,
    },
    /// A custom note (operator or engine annotation).
    Note {
        /// Free-form text.
        text: String,
    },
}

/// Errors from the session log.
#[derive(Debug, thiserror::Error)]
pub enum SessionLogError {
    /// The log directory could not be created / opened.
    #[error("cannot open session log: {0}")]
    Io(#[from] std::io::Error),
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Monotonic counter so two sessions opened in the same millisecond still
/// get distinct ids (tests open several back-to-back).
static SESSION_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_session_id() -> String {
    use std::sync::atomic::Ordering;
    let n = SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("s-{}-{}", now_ms(), n)
}

/// Append-only JSONL session log. Safe to share across threads.
pub struct SessionLog {
    file: Mutex<File>,
    path: PathBuf,
    session_id: String,
}

impl SessionLog {
    /// Opens (creating if needed) a new session log file in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, SessionLogError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let session_id = next_session_id();
        let path = dir.join(format!("{session_id}.jsonl"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            file: Mutex::new(file),
            path,
            session_id,
        })
    }

    /// Opens a session log against an explicit file path.
    pub fn open_at(
        path: impl AsRef<Path>,
        session_id: impl Into<String>,
    ) -> Result<Self, SessionLogError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            file: Mutex::new(file),
            path,
            session_id: session_id.into(),
        })
    }

    /// This session's id.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends an event. Write failures are logged and reported to the
    /// caller but never panic mid-show.
    pub fn record(&self, kind: SessionEventKind) -> Result<(), SessionLogError> {
        let event = SessionEvent {
            ts_ms: now_ms(),
            session: self.session_id.clone(),
            kind,
        };
        let line = serde_json::to_string(&event)
            .map_err(|e| SessionLogError::Io(std::io::Error::other(e.to_string())))?;
        let mut file = self.file.lock().expect("session log lock poisoned");
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    /// Reads all well-formed events from a log file; malformed lines
    /// (e.g. a torn write after a crash) are skipped.
    pub fn read_events(path: impl AsRef<Path>) -> Vec<SessionEvent> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_events_through_a_file() {
        let dir = std::env::temp_dir().join(format!("tpt-lp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = SessionLog::open(&dir).expect("open");
        log.record(SessionEventKind::SessionStart {
            show: "sunday".into(),
            name: "Sunday Service".into(),
            mode: "rehearsal".into(),
        })
        .unwrap();
        log.record(SessionEventKind::CueAdvance {
            number: 3,
            label: "Sponsor break".into(),
            automatic: false,
        })
        .unwrap();
        log.record(SessionEventKind::Failsafe {
            reason: "video_input_lost".into(),
            source: "cam2".into(),
            action: "cut_to_backup".into(),
        })
        .unwrap();

        let events = SessionLog::read_events(log.path());
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[1].kind,
            SessionEventKind::CueAdvance { number: 3, .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn torn_tail_lines_are_skipped() {
        let dir = std::env::temp_dir().join(format!("tpt-lp-torn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("torn.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"ts_ms\":1,\"session\":\"s\",\"kind\":{\"type\":\"note\",\"text\":\"ok\"}}\n",
                "{\"ts_ms\":2,\"session\":\"s\",\"kind\":{\"type\":\"not", // torn write
            ),
        )
        .unwrap();
        let events = SessionLog::read_events(&path);
        assert_eq!(events.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_ids_are_distinct_per_open() {
        let dir = std::env::temp_dir().join(format!("tpt-lp-ids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = SessionLog::open(&dir).unwrap();
        let b = SessionLog::open(&dir).unwrap();
        assert_ne!(a.session_id(), b.session_id());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
