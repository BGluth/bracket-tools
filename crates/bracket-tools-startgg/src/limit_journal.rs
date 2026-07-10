//! Cross-process rate-limit memory.
//!
//! start.gg enforces its per-minute quota per *token*, server-side, with no
//! way to query the window's current state. The in-process governor limiter
//! resets on every restart, so a relaunch inside a hot window replays the
//! whole budget into an already-spent quota and eats 429s. The journal fixes
//! that: every process sharing a token also shares an on-disk sliding window
//! of recent request timestamps, plus a hold stamp written whenever the
//! server actually rejects with 429.
//!
//! Best-effort by design: any I/O or parse trouble disables gating for that
//! call rather than failing (or delaying) the query. Concurrent processes
//! read-modify-write without a file lock — a lost timestamp under-counts the
//! window slightly, which the capacity margin absorbs.

use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Kept below the configured per-minute rate so clock skew between processes
/// (and the lockless read-modify-write) can't nudge the count past the
/// server's own window.
pub(crate) const JOURNAL_CAPACITY_MARGIN: u32 = 4;

#[derive(Debug, Default, Serialize, Deserialize)]
struct JournalState {
    /// Set when the server answered 429: no process may send until this
    /// passes (unix millis).
    blocked_until_ms: Option<i64>,
    /// Unix millis of recent sends, pruned to the window on every touch.
    sent_ms: Vec<i64>,
}

/// The on-disk sliding request window shared by every process using the same
/// token. See the module docs for the contract.
#[derive(Debug)]
pub(crate) struct LimitJournal {
    path: PathBuf,
    window: Duration,
    capacity: usize,
    /// Serializes read-modify-write within this process; cross-process races
    /// stay best-effort.
    lock: Mutex<()>,
}

impl LimitJournal {
    pub(crate) fn new(path: PathBuf, window: Duration, capacity: usize) -> Self {
        Self {
            path,
            window,
            capacity: capacity.max(1),
            lock: Mutex::new(()),
        }
    }

    /// Whether sending now would exceed the shared window. `Some(wait)` means
    /// the caller should not send for roughly that long; `None` clears it.
    pub(crate) fn gate(&self, now: SystemTime) -> Option<Duration> {
        let _guard = self.lock.lock();
        let state = self.load();
        let now_ms = to_millis(now);

        if let Some(blocked) = state.blocked_until_ms {
            if blocked > now_ms {
                return Some(Duration::from_millis((blocked - now_ms) as u64));
            }
        }

        let floor = now_ms - self.window.as_millis() as i64;
        let mut recent: Vec<i64> = state.sent_ms.into_iter().filter(|&t| t > floor).collect();
        if recent.len() < self.capacity {
            return None;
        }
        recent.sort_unstable();
        let oldest = recent[recent.len() - self.capacity];
        let clear_ms = oldest + self.window.as_millis() as i64 - now_ms;
        (clear_ms > 0).then(|| Duration::from_millis(clear_ms as u64))
    }

    /// Records a request about to go out.
    pub(crate) fn record_sent(&self, now: SystemTime) {
        let _guard = self.lock.lock();
        let mut state = self.load();
        let now_ms = to_millis(now);
        let floor = now_ms - self.window.as_millis() as i64;
        state.sent_ms.retain(|&t| t > floor);
        state.sent_ms.push(now_ms);
        if state.blocked_until_ms.is_some_and(|b| b <= now_ms) {
            state.blocked_until_ms = None;
        }
        self.save(&state);
    }

    /// Records a server-side 429: hold every sharing process for `hold`.
    pub(crate) fn record_rejected(&self, now: SystemTime, hold: Duration) {
        let _guard = self.lock.lock();
        let mut state = self.load();
        let until = to_millis(now) + hold.as_millis() as i64;
        state.blocked_until_ms = Some(state.blocked_until_ms.map_or(until, |b| b.max(until)));
        self.save(&state);
    }

    fn load(&self) -> JournalState {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, state: &JournalState) {
        let Ok(bytes) = serde_json::to_vec(state) else { return };
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("json.tmp");
        if fs::write(&tmp, bytes).is_ok() {
            let _ = fs::rename(&tmp, &self.path);
        }
    }
}

fn to_millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    use super::LimitJournal;

    const WINDOW: Duration = Duration::from_secs(60);

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bt-journal-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("window.json")
    }

    #[test]
    fn empty_journal_admits_and_fills_to_capacity() {
        let journal = LimitJournal::new(scratch("fill"), WINDOW, 3);
        let now = SystemTime::now();

        for _ in 0..3 {
            assert_eq!(journal.gate(now), None);
            journal.record_sent(now);
        }
        let wait = journal.gate(now).expect("at capacity");
        assert!(wait <= WINDOW, "{wait:?}");
    }

    #[test]
    fn window_frees_as_old_sends_expire() {
        let journal = LimitJournal::new(scratch("expire"), WINDOW, 2);
        let start = SystemTime::now();
        journal.record_sent(start);
        journal.record_sent(start + Duration::from_secs(30));

        assert!(journal.gate(start + Duration::from_secs(31)).is_some());
        assert_eq!(journal.gate(start + Duration::from_secs(61)), None, "first send expired");
    }

    #[test]
    fn a_second_journal_on_the_same_file_sees_the_window() {
        let path = scratch("shared");
        let first = LimitJournal::new(path.clone(), WINDOW, 2);
        let now = SystemTime::now();
        first.record_sent(now);
        first.record_sent(now);

        // The "restarted process": fresh instance, same file.
        let second = LimitJournal::new(path, WINDOW, 2);
        assert!(second.gate(now + Duration::from_secs(1)).is_some());
    }

    #[test]
    fn rejection_holds_until_the_stamp_passes() {
        let journal = LimitJournal::new(scratch("hold"), WINDOW, 100);
        let now = SystemTime::now();
        journal.record_rejected(now, Duration::from_secs(65));

        let wait = journal.gate(now + Duration::from_secs(1)).expect("held");
        assert!(wait > Duration::from_secs(60), "{wait:?}");
        assert_eq!(journal.gate(now + Duration::from_secs(66)), None);
    }

    #[test]
    fn corrupt_file_degrades_to_admitting() {
        let path = scratch("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json at all").unwrap();

        let journal = LimitJournal::new(path, WINDOW, 1);
        assert_eq!(journal.gate(SystemTime::now()), None);
        journal.record_sent(SystemTime::now());
        assert!(journal.gate(SystemTime::now()).is_some(), "recovered file counts again");
    }
}
