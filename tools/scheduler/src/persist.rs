//! Overlay persistence: the operator's local state written to a versioned
//! JSON file, atomically (temp + rename), so a mid-event crash or restart
//! resumes the board, flags, tombstones, durations, pending writes, and unread
//! notices instead of a blank screen.
//!
//! The on-disk shape is an [`OverlayDoc`] DTO: it reuses the internal overlay
//! types where they serialize cleanly and flattens the tuple/enum-keyed maps
//! (which JSON can't express as object keys) to vectors of pairs. A corrupt or
//! version-mismatched file is renamed `.bak` and treated as a fresh start —
//! never a startup failure. A single-instance flock guards the file so two
//! schedulers can't halve the rate budget or last-writer-wins the state.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    app::{Notice, PendingWrite},
    conflict::{ConflictKey, PlayerFlags, SetupBoard, Tombstones, UnixMillis},
    duration::DurationModel,
    model::{BracketId, SetKey},
};

/// Bumped when the on-disk shape changes incompatibly; an older file then
/// recovers to `.bak` rather than mis-parsing.
pub const OVERLAY_VERSION: u32 = 1;

/// The persisted overlay. Maps with non-string keys become vectors of pairs so
/// the document is plain JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayDoc {
    pub version: u32,
    pub board: SetupBoard,
    pub flags: PlayerFlags,
    pub tombstones: Tombstones,
    pub snoozes: Vec<(BracketId, SetKey, UnixMillis)>,
    pub last_completed: Vec<(ConflictKey, UnixMillis)>,
    pub callable_since: Vec<(SetKey, UnixMillis)>,
    pub called_at: Vec<(BracketId, SetKey, UnixMillis)>,
    pub called_ints: Vec<i32>,
    pub in_progress_ints: Vec<i32>,
    pub soft_busy: Vec<(BracketId, SetKey)>,
    pub durations: DurationModel,
    pub pending_writes: Vec<PendingWrite>,
    /// Unacked, correctness-relevant notices only (see `AppState::to_overlay`).
    pub notices: Vec<Notice>,
    pub no_show_alerted: Vec<(BracketId, SetKey)>,
}

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("failed to serialize overlay: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to write overlay {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read overlay {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Outcome of loading the overlay at startup.
pub enum OverlayLoad {
    /// No file yet (first run).
    None,
    Loaded(Box<OverlayDoc>),
    /// The file was corrupt or a mismatched version; it was renamed here and a
    /// fresh session begins.
    Recovered(PathBuf),
}

/// Serializes and writes the overlay atomically: full contents to a sibling
/// temp file, fsync, then rename over the target (atomic on the same
/// filesystem), so a crash mid-write never truncates the live file.
pub fn save_overlay(path: &Path, doc: &OverlayDoc) -> Result<(), PersistError> {
    let json = serde_json::to_vec_pretty(doc).map_err(PersistError::Serialize)?;
    let tmp = temp_path(path);
    write_then_rename(&tmp, path, &json).map_err(|source| PersistError::Write {
        path: path.to_owned(),
        source,
    })
}

fn write_then_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    {
        let mut file = File::create(tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(tmp, path)
}

/// Loads the overlay, recovering a corrupt/version-mismatched file to `.bak`.
pub fn load_overlay(path: &Path) -> Result<OverlayLoad, PersistError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(OverlayLoad::None),
        Err(source) => {
            return Err(PersistError::Read {
                path: path.to_owned(),
                source,
            })
        }
    };

    match serde_json::from_str::<OverlayDoc>(&raw) {
        Ok(doc) if doc.version == OVERLAY_VERSION => Ok(OverlayLoad::Loaded(Box::new(doc))),
        // Parsed but from an incompatible version, or failed to parse at all:
        // back it up and start fresh rather than bricking startup.
        _ => {
            let backup = backup_path(path);
            fs::rename(path, &backup).map_err(|source| PersistError::Write {
                path: backup.clone(),
                source,
            })?;
            Ok(OverlayLoad::Recovered(backup))
        }
    }
}

fn temp_path(path: &Path) -> PathBuf {
    with_suffix(path, "tmp")
}

fn backup_path(path: &Path) -> PathBuf {
    with_suffix(path, "bak")
}

/// `foo.json` → `foo.json.<suffix>` (keeps the original name intact so the
/// pairing is obvious on disk).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// An advisory exclusive lock on a sidecar file, held for the process lifetime
/// to enforce single-instance operation. Released (and the file removed) on
/// drop.
pub struct Lockfile {
    file: File,
    path: PathBuf,
}

#[derive(Debug, Error)]
pub enum LockError {
    #[error("another scheduler instance is already running{}", pid.as_ref().map(|p| format!(" (pid {p})")).unwrap_or_default())]
    Held { path: PathBuf, pid: Option<String> },
    #[error("failed to acquire lock {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl Lockfile {
    /// Acquires the lock, writing our pid into the file. Fails with
    /// [`LockError::Held`] (carrying the holder's pid when readable) if another
    /// instance holds it.
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|source| LockError::Io {
                path: path.to_owned(),
                source,
            })?;

        if let Err(source) = file.try_lock_exclusive() {
            if source.kind() == io::ErrorKind::WouldBlock {
                return Err(LockError::Held {
                    path: path.to_owned(),
                    pid: read_pid(path),
                });
            }
            return Err(LockError::Io {
                path: path.to_owned(),
                source,
            });
        }

        let mut file = file;
        // Best-effort pid record for the "already running" message.
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        let _ = file.sync_all();
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }
}

impl Drop for Lockfile {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        let _ = fs::remove_file(&self.path);
    }
}

fn read_pid(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    let trimmed = buf.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{load_overlay, save_overlay, Lockfile, OverlayDoc, OverlayLoad, OVERLAY_VERSION};
    use crate::{conflict::SetupBoard, config::SetupId, duration::DurationModel};

    fn scratch(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("bt_persist_test_{}_{}", std::process::id(), name));
        dir
    }

    fn sample_doc() -> OverlayDoc {
        OverlayDoc {
            version: OVERLAY_VERSION,
            board: SetupBoard::new(&[SetupId(1), SetupId(2)]),
            flags: Default::default(),
            tombstones: Default::default(),
            snoozes: Vec::new(),
            last_completed: Vec::new(),
            callable_since: Vec::new(),
            called_at: Vec::new(),
            called_ints: vec![6],
            in_progress_ints: vec![2],
            soft_busy: Vec::new(),
            durations: DurationModel::new(),
            pending_writes: Vec::new(),
            notices: Vec::new(),
            no_show_alerted: Vec::new(),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let path = scratch("roundtrip.json");
        let _ = std::fs::remove_file(&path);
        let doc = sample_doc();
        save_overlay(&path, &doc).unwrap();

        let OverlayLoad::Loaded(loaded) = load_overlay(&path).unwrap() else {
            panic!("expected a loaded overlay");
        };
        assert_eq!(loaded.called_ints, vec![6]);
        assert_eq!(loaded.in_progress_ints, vec![2]);
        assert_eq!(loaded.board.setups().len(), 2);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_none_not_error() {
        let path = scratch("absent.json");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(load_overlay(&path).unwrap(), OverlayLoad::None));
    }

    #[test]
    fn corrupt_file_recovers_to_bak() {
        let path = scratch("corrupt.json");
        std::fs::write(&path, b"{ this is not valid json").unwrap();
        let OverlayLoad::Recovered(backup) = load_overlay(&path).unwrap() else {
            panic!("expected recovery");
        };
        assert!(backup.exists(), "corrupt file backed up");
        assert!(!path.exists(), "corrupt file moved aside");
        std::fs::remove_file(&backup).unwrap();
    }

    #[test]
    fn version_mismatch_recovers_to_bak() {
        let path = scratch("oldversion.json");
        let mut doc = sample_doc();
        doc.version = OVERLAY_VERSION + 1;
        save_overlay(&path, &doc).unwrap();
        assert!(matches!(load_overlay(&path).unwrap(), OverlayLoad::Recovered(_)));
        let backup = super::backup_path(&path);
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn second_lock_is_rejected() {
        let path = scratch("lock");
        let _ = std::fs::remove_file(&path);
        let first = Lockfile::acquire(&path).expect("first lock");
        let second = Lockfile::acquire(&path);
        assert!(matches!(second, Err(super::LockError::Held { .. })), "second instance blocked");
        drop(first);
        // Once released, the lock is available again.
        Lockfile::acquire(&path).expect("lock free after drop");
    }
}
