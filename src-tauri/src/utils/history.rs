//! A small on-disk record of what Windle has removed.
//!
//! The dashboard shows "last clean" and "space reclaimed so far", which means
//! something has to outlive the process. This keeps a short JSON log next to
//! the app's other support files; losing it costs nothing but the history, so
//! every failure here is swallowed rather than surfaced.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::utils::format;
use crate::utils::permissions;

/// Older entries are dropped: this is a recent-activity list, not an audit log.
const MAX_ENTRIES: usize = 50;

/// What kind of operation freed the space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Operation {
    Clean,
    EmptyTrash,
    Uninstall,
    Purge,
    Installers,
    Agent,
    Docker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub operation: Operation,
    pub at: u64,
    pub freed_bytes: u64,
    pub item_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct History {
    pub total_freed_bytes: u64,
    /// Oldest first, so the most recent operation is last.
    pub entries: Vec<HistoryEntry>,
}

impl History {
    /// When Windle last removed anything.
    pub fn last_clean_at(&self) -> Option<u64> {
        self.entries.last().map(|entry| entry.at)
    }
}

/// Where the log lives.
pub fn path() -> PathBuf {
    permissions::home_dir().join("Library/Application Support/Windle/history.json")
}

/// Read the log, treating any problem as "no history yet".
pub fn load() -> History {
    load_from(&path())
}

fn load_from(file: &std::path::Path) -> History {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Append one operation. Does nothing when it freed no space, so a no-op clean
/// does not clutter the list.
pub fn record(operation: Operation, freed_bytes: u64, item_count: usize) {
    record_in(&path(), operation, freed_bytes, item_count);
}

fn record_in(file: &std::path::Path, operation: Operation, freed_bytes: u64, item_count: usize) {
    if freed_bytes == 0 && item_count == 0 {
        return;
    }

    let Some(at) = format::epoch_millis(std::time::SystemTime::now()) else {
        return;
    };

    let mut history = load_from(file);
    history.total_freed_bytes = history.total_freed_bytes.saturating_add(freed_bytes);
    history.entries.push(HistoryEntry {
        operation,
        at,
        freed_bytes,
        item_count,
    });

    // Keep only the most recent window.
    if history.entries.len() > MAX_ENTRIES {
        let excess = history.entries.len() - MAX_ENTRIES;
        history.entries.drain(..excess);
    }

    save(file, &history);
}

fn save(file: &std::path::Path, history: &History) {
    let Some(parent) = file.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    if let Ok(contents) = serde_json::to_string_pretty(history) {
        let _ = std::fs::write(file, contents);
    }
}

/// Note the outcome of a delete operation.
pub fn record_outcome(operation: Operation, outcome: &crate::commands::CleanOutcome) {
    record(operation, outcome.freed_bytes, outcome.removed_paths.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private log file per test, so parallel runs cannot collide and the
    /// user's real history is never touched.
    fn scratch(name: &str) -> PathBuf {
        let file = PathBuf::from("/tmp").join(format!(
            "windle-history-{name}-{}.json",
            std::process::id()
        ));
        std::fs::remove_file(&file).ok();
        file
    }

    #[test]
    fn an_empty_log_reads_as_no_history() {
        let history = History::default();

        assert_eq!(history.last_clean_at(), None);
        assert_eq!(history.total_freed_bytes, 0);
    }

    #[test]
    fn the_newest_entry_is_the_last_clean() {
        let history = History {
            total_freed_bytes: 300,
            entries: vec![
                HistoryEntry {
                    operation: Operation::Clean,
                    at: 100,
                    freed_bytes: 100,
                    item_count: 1,
                },
                HistoryEntry {
                    operation: Operation::Purge,
                    at: 200,
                    freed_bytes: 200,
                    item_count: 2,
                },
            ],
        };

        assert_eq!(history.last_clean_at(), Some(200));
    }

    #[test]
    fn the_log_round_trips_and_stays_bounded() {
        let file = scratch("bounded");

        for _ in 0..MAX_ENTRIES + 10 {
            record_in(&file, Operation::Clean, 1, 1);
        }

        let history = load_from(&file);

        // Every operation counts towards the total, even the ones whose entry
        // has since aged out.
        assert_eq!(history.total_freed_bytes, (MAX_ENTRIES + 10) as u64);
        assert_eq!(history.entries.len(), MAX_ENTRIES);

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn recording_survives_a_reload() {
        let file = scratch("reload");

        record_in(&file, Operation::Purge, 4_096, 2);
        record_in(&file, Operation::EmptyTrash, 1_024, 1);

        let history = load_from(&file);

        assert_eq!(history.total_freed_bytes, 5_120);
        assert_eq!(history.entries.len(), 2);
        assert_eq!(history.entries[1].operation, Operation::EmptyTrash);
        assert!(history.last_clean_at().is_some());

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn an_operation_that_freed_nothing_is_not_recorded() {
        let file = scratch("empty");

        record_in(&file, Operation::Clean, 0, 0);

        assert!(!file.exists(), "nothing to report means nothing to write");
        assert_eq!(load_from(&file).entries.len(), 0);
    }

    #[test]
    fn a_corrupt_log_reads_as_no_history() {
        let file = scratch("corrupt");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{ not json").unwrap();

        assert_eq!(load_from(&file).entries.len(), 0);

        // A later write repairs it rather than failing.
        record_in(&file, Operation::Clean, 10, 1);
        assert_eq!(load_from(&file).total_freed_bytes, 10);

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn a_missing_log_file_is_not_an_error() {
        // `load` reads the real location, which may or may not exist yet;
        // either way it must return something usable.
        let history = load();
        assert!(history.entries.len() <= MAX_ENTRIES);
    }
}
