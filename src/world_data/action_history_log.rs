//! Action History Log (durable JSONL append-only log)
//!
//! Per Arcurus 2026-06-03 (#openworld):
//!   "create new file saving the history of the world actions for a given
//!    entity and display it in the open world selena web interface if
//!    you open the entity"
//!
//! Why a separate log file in addition to entity.history (which lives in
//! the save.owbl blob)?
//!
//!   * Independent of save.owbl — survives a corrupted or replaced save.
//!   * Append-only — never lost to in-place edits.
//!   * Cheap to grep / parse / export without loading the whole world.
//!   * Future-friendly — easy to add a secondary index (entity_id, day) or
//!     ship to a log aggregator without touching save.owbl.
//!
//! File format: world_data/action_history.jsonl
//!   one JSON object per line, schema:
//!     { "entity_id": "<uuid>",
//!       "entity_name": "<name>",
//!       "timestamp": "2026-06-03T19:43:01Z",
//!       "action": "<action name>",
//!       "outcome": "<1-3 sentence outcome>",
//!       "details": "<narrative prose>",
//!       "effects": { ... },
//!       "warnings": [...] }
//!
//! Read API: `load_history_for(entity_id, limit)` returns the last N
//! entries (most recent first) for the given entity, ignoring other rows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const HISTORY_FILENAME: &str = "action_history.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionHistoryEntry {
    pub entity_id: String,
    pub entity_name: String,
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub outcome: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub effects: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Default on-disk location: `<cwd>/world_data/action_history.jsonl`.
pub fn history_path() -> PathBuf {
    PathBuf::from("world_data").join(HISTORY_FILENAME)
}

/// Path variant for tests and callers that need a different base
/// directory (e.g. a `tempdir`). The filename is always
/// `action_history.jsonl`.
pub fn history_path_at(base_dir: &Path) -> PathBuf {
    base_dir.join("world_data").join(HISTORY_FILENAME)
}

/// Append one entry to the durable history log. Best-effort: a write
/// failure logs to stderr but does not block the action response.
///
/// Writes are atomic per line (single-line write + flush).
pub fn append_entry(entry: &ActionHistoryEntry) -> std::io::Result<()> {
    append_entry_at(entry, &history_path())
}

/// Path-parameterized variant of [`append_entry`]. Public so tests
/// (and any future non-default storage) can target a different file.
pub fn append_entry_at(entry: &ActionHistoryEntry, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(f, "{}", line)?;
    f.flush()?;
    Ok(())
}

/// Read the most recent N entries for the given entity_id (most recent
/// first).  Reads the whole file but streams it line-by-line so it scales
/// to large logs without loading everything into memory at once.
pub fn load_for_entity(entity_id: &str, limit: usize) -> Vec<ActionHistoryEntry> {
    load_for_entity_at(entity_id, limit, &history_path())
}

/// Path-parameterized variant of [`load_for_entity`].
pub fn load_for_entity_at(
    entity_id: &str,
    limit: usize,
    path: &Path,
) -> Vec<ActionHistoryEntry> {
    if !path.exists() {
        return Vec::new();
    }
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut matching: Vec<ActionHistoryEntry> = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: ActionHistoryEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.entity_id == entity_id {
            matching.push(entry);
        }
    }
    // The file is chronological, so the last N are most recent.  We
    // reverse for "most recent first" order.
    matching.reverse();
    if matching.len() > limit {
        matching.truncate(limit);
    }
    matching
}

/// Total entries for a given entity (cheap, for stats / debug).
pub fn count_for_entity(entity_id: &str) -> usize {
    count_for_entity_at(entity_id, &history_path())
}

/// Path-parameterized variant of [`count_for_entity`].
pub fn count_for_entity_at(entity_id: &str, path: &Path) -> usize {
    if !path.exists() {
        return 0;
    }
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let reader = BufReader::new(file);
    let mut n = 0usize;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<ActionHistoryEntry>(&line) {
            if entry.entity_id == entity_id {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Per-test tempdir. We deliberately use a real OS tempdir (not
    /// a single shared scratch path) so parallel test execution and
    /// re-runs don't collide with the live `world_data/` log.
    fn fresh_log_path() -> PathBuf {
        let mut dir = env::temp_dir();
        dir.push(format!("ow-action-history-{}", Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        history_path_at(&dir)
    }

    fn make_entry(entity_id: &str, action: &str, ts_secs: i64) -> ActionHistoryEntry {
        ActionHistoryEntry {
            entity_id: entity_id.to_string(),
            entity_name: format!("Entity {}", entity_id),
            timestamp: DateTime::<Utc>::from_timestamp(ts_secs, 0).unwrap(),
            action: action.to_string(),
            outcome: format!("outcome for {}", action),
            details: String::new(),
            effects: serde_json::Map::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn append_and_load_basic() {
        let path = fresh_log_path();
        append_entry_at(&make_entry("e1", "act_a", 1000), &path).unwrap();
        append_entry_at(&make_entry("e1", "act_b", 2000), &path).unwrap();
        let entries = load_for_entity_at("e1", 10, &path);
        assert_eq!(entries.len(), 2);
        // Most recent first
        assert_eq!(entries[0].action, "act_b");
        assert_eq!(entries[1].action, "act_a");
    }

    #[test]
    fn load_respects_limit() {
        let path = fresh_log_path();
        for i in 0..5 {
            append_entry_at(&make_entry("e1", &format!("act_{}", i), 1000 + i), &path).unwrap();
        }
        let entries = load_for_entity_at("e1", 3, &path);
        assert_eq!(entries.len(), 3);
        // The last 3 written are 2, 3, 4 (most-recent first)
        assert_eq!(entries[0].action, "act_4");
        assert_eq!(entries[1].action, "act_3");
        assert_eq!(entries[2].action, "act_2");
    }

    #[test]
    fn load_filters_by_entity_id() {
        let path = fresh_log_path();
        append_entry_at(&make_entry("e1", "a", 1000), &path).unwrap();
        append_entry_at(&make_entry("e2", "b", 1500), &path).unwrap();
        append_entry_at(&make_entry("e1", "c", 2000), &path).unwrap();
        append_entry_at(&make_entry("e3", "d", 2500), &path).unwrap();
        let e1 = load_for_entity_at("e1", 10, &path);
        assert_eq!(e1.len(), 2);
        assert!(e1.iter().all(|e| e.entity_id == "e1"));
        assert_eq!(e1[0].action, "c");
        assert_eq!(e1[1].action, "a");
        assert_eq!(load_for_entity_at("e2", 10, &path).len(), 1);
        assert_eq!(load_for_entity_at("e_missing", 10, &path).len(), 0);
    }

    #[test]
    fn count_for_entity_basic() {
        let path = fresh_log_path();
        append_entry_at(&make_entry("e1", "a", 1000), &path).unwrap();
        append_entry_at(&make_entry("e2", "b", 1500), &path).unwrap();
        append_entry_at(&make_entry("e1", "c", 2000), &path).unwrap();
        append_entry_at(&make_entry("e1", "d", 2500), &path).unwrap();
        assert_eq!(count_for_entity_at("e1", &path), 3);
        assert_eq!(count_for_entity_at("e2", &path), 1);
        assert_eq!(count_for_entity_at("nope", &path), 0);
    }

    #[test]
    fn append_creates_parent_directory() {
        // Use a path whose parent doesn't exist yet
        let mut dir = env::temp_dir();
        dir.push(format!("ow-deep-{}-{}", Uuid::new_v4(), Uuid::new_v4()));
        let path = history_path_at(&dir); // .../world_data/action_history.jsonl
        assert!(!path.parent().unwrap().exists());
        append_entry_at(&make_entry("e1", "first", 1), &path).unwrap();
        assert!(path.exists());
        let entries = load_for_entity_at("e1", 10, &path);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn load_returns_empty_when_file_missing() {
        let mut dir = env::temp_dir();
        dir.push(format!("ow-missing-{}", Uuid::new_v4()));
        let path = history_path_at(&dir);
        // No append — file does not exist yet
        assert!(load_for_entity_at("e1", 10, &path).is_empty());
        assert_eq!(count_for_entity_at("e1", &path), 0);
    }

    // Bring Uuid into scope for the test helpers above
    use uuid::Uuid;
}
