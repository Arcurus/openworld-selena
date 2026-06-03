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
use std::path::PathBuf;

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

fn history_path() -> PathBuf {
    PathBuf::from("world_data").join(HISTORY_FILENAME)
}

/// Append one entry to the durable history log. Best-effort: a write
/// failure logs to stderr but does not block the action response.
///
/// Writes are atomic per line (single-line write + flush).
pub fn append_entry(entry: &ActionHistoryEntry) -> std::io::Result<()> {
    if let Some(parent) = history_path().parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path())?;
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
    let path = history_path();
    if !path.exists() {
        return Vec::new();
    }
    let file = match std::fs::File::open(&path) {
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
    let path = history_path();
    if !path.exists() {
        return 0;
    }
    let file = match std::fs::File::open(&path) {
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
