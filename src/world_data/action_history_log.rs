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
    /// World tick at the moment this action was processed.
    /// Tied to `World::action_count` (a monotonic counter
    /// that increments on every world action).  Used by the
    /// "unprocessed world actions from other entities" LLM
    /// feature to track, per-entity, which actions have been
    /// folded into that entity's history_summary.  See
    /// `docs/world-mechanics.md` for the full contract.
    ///
    /// `#[serde(default)]` so old entries (pre-2026-06-07)
    /// load with `tick = 0` and get backfilled on world
    /// load by the load path in `main.rs`.
    #[serde(default)]
    pub tick: i64,
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

/// Load the most recent N world actions across ALL entities,
/// optionally filtering to entries strictly after `after_ts`.
///
/// This is the cross-entity feed used by the LLM action prompt so
/// the actor can see what else has been happening in the world.
/// Per Arcurus 2026-06-07 (#openworld): "add to the world action
/// llm call an insertion of not yet processed world actions".
///
/// "Not yet processed" is modelled as: every entry in the
/// `action_history.jsonl` log with `timestamp > actor.last_action_at`
/// (or, if the actor has never acted, every entry). The caller is
/// expected to do any final filtering (e.g. drop the actor's own
/// most recent action that triggered the current LLM call, drop
/// system-entity actions) before rendering the prompt block — this
/// function is deliberately raw.
///
/// Returns the entries in most-recent-first order, capped at `limit`.
pub fn load_recent_world_actions(limit: usize, after_ts: Option<DateTime<Utc>>) -> Vec<ActionHistoryEntry> {
    load_recent_world_actions_at(limit, after_ts, &history_path())
}

/// Path-parameterized variant of [`load_recent_world_actions`].
/// Public so tests and any future non-default storage can target a
/// different file.
pub fn load_recent_world_actions_at(
    limit: usize,
    after_ts: Option<DateTime<Utc>>,
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
    // The file is chronological (oldest first). We collect all
    // matching entries that satisfy the after_ts filter, then
    // truncate to the most recent `limit` from the tail (cheaper
    // than sorting the whole thing if the log is large).
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
        if let Some(cutoff) = after_ts {
            if entry.timestamp <= cutoff {
                continue;
            }
        }
        matching.push(entry);
    }
    // Truncate from the head to keep only the most recent `limit`
    // (the tail of `matching` is the newest because the log is
    // chronological).
    if matching.len() > limit {
        let drop_n = matching.len() - limit;
        matching.drain(0..drop_n);
    }
    // Reverse to most-recent-first.
    matching.reverse();
    matching
}

/// Read ALL entries from the durable history log, in append
/// (oldest-first) order.  Used by the "unprocessed world
/// actions from other entities" LLM context feature to scan
/// the whole file and filter to entries that touch a
/// specific entity.  Per Arcurus 2026-06-07 (#openworld).
///
/// Why oldest-first here (not most-recent-first like
/// `load_recent_world_actions`): the renderer for the
/// unprocessed list needs to know the count of *omitted*
/// older actions so it can flag the operator (and because
/// the 10K char cap means we may need to drop the oldest
/// entries from the visible list).  Working in
/// chronological order makes the drop-oldest logic
/// trivial.
///
/// The 10K char cap is applied inside the renderer (so the
/// renderer is pure and unit-testable).  The I/O is done
/// here so the renderer doesn't need to read the file.
///
/// Cheap on a typical world (5400 entries, sub-millisecond
/// to parse).  If the log grows to 100K+ entries this
/// would benefit from an index; not needed at current
/// scale.
pub fn load_all_world_actions() -> Vec<ActionHistoryEntry> {
    load_all_world_actions_at(&history_path())
}

/// Path-parameterized variant of [`load_all_world_actions`].
pub fn load_all_world_actions_at(path: &Path) -> Vec<ActionHistoryEntry> {
    if !path.exists() {
        return Vec::new();
    }
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut entries: Vec<ActionHistoryEntry> = Vec::new();
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
        entries.push(entry);
    }
    entries
}

/// Best-effort helper that reads the durable history log,
/// assigns a sequential tick (1, 2, 3, ...) to any entry with
/// `tick == 0` (pre-2026-06-07 entries; the `tick` field has
/// `#[serde(default)]` so old data loads as 0), and rewrites
/// the file in place.  Returns the count of entries that
/// were backfilled (0 means "everything already had ticks").
///
/// Per Arcurus 2026-06-07 #openworld: "we need also to add
/// the time tick in the world action history when the action
/// happened.  if its not set yes, we need also to be able
/// to set a date until which dates other entities actions
/// where processed".  The backfill is what closes the "if
/// its not set yes" gap for the existing 5400+ entries on
/// disk.
///
/// Why this is safe to call on every world load: it's
/// idempotent (only modifies entries with tick=0; once
/// every entry has a real tick, it's a no-op).  And it
/// never touches entries that already have a tick.
///
/// Why we rewrite the whole file (instead of in-place
/// patching each line): the JSONL log is append-only and
/// lines aren't a fixed length, so a partial-write
/// approach would need to read the whole file into memory
/// anyway.  An atomic full-file rewrite (write to
/// `action_history.jsonl.tmp`, then rename) is the
/// standard pattern.
pub fn backfill_ticks() -> usize {
    backfill_ticks_at(&history_path())
}

/// Path-parameterized variant of [`backfill_ticks`].
pub fn backfill_ticks_at(path: &Path) -> usize {
    if !path.exists() {
        return 0;
    }
    // Read all entries.
    let Ok(raw) = std::fs::read_to_string(path) else {
        return 0;
    };
    let mut entries: Vec<ActionHistoryEntry> = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ActionHistoryEntry>(line) {
            Ok(e) => entries.push(e),
            // Skip malformed lines (defense: a corrupt
            // line shouldn't break the whole backfill).
            Err(_) => continue,
        }
    }
    if entries.is_empty() {
        return 0;
    }
    // Assign sequential ticks: 1, 2, 3, ... to any entry
    // with tick==0.  Skip entries that already have a
    // tick (they were stamped when written; we don't
    // overwrite).  The starting point is the max existing
    // tick + 1, so the backfill seamlessly continues the
    // monotonic sequence even if the file was partially
    // backfilled in a prior run.
    let mut backfilled_count = 0usize;
    let mut next_tick: i64 = entries
        .iter()
        .map(|e| e.tick)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    for e in entries.iter_mut() {
        if e.tick == 0 {
            e.tick = next_tick;
            next_tick = next_tick.saturating_add(1);
            backfilled_count += 1;
        }
    }
    if backfilled_count == 0 {
        return 0;
    }
    // Atomic rewrite: write to tmp, then rename.  Avoids
    // a half-written file if the process is killed mid-write.
    let tmp_path = path.with_extension("jsonl.tmp");
    let write_result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(&tmp_path)?;
        for e in &entries {
            let line = serde_json::to_string(e)
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
            writeln!(f, "{}", line)?;
        }
        f.flush()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        eprintln!("[backfill_ticks] failed to write tmp file: {}", e);
        return 0;
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        eprintln!("[backfill_ticks] failed to rename tmp to {}: {}", path.display(), e);
        let _ = std::fs::remove_file(&tmp_path);
        return 0;
    }
    backfilled_count
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
            tick: 0,
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

    // -- load_recent_world_actions tests ------------------------------
    // Per Arcurus 2026-06-07 (#openworld): cross-entity feed used to
    // surface "not yet processed" world actions to the LLM prompt.
    // Helper makes one entry per call so test bodies stay readable.
    fn make_entry_full(
        entity_id: &str,
        entity_name: &str,
        action: &str,
        ts_secs: i64,
    ) -> ActionHistoryEntry {
        ActionHistoryEntry {
            entity_id: entity_id.to_string(),
            entity_name: entity_name.to_string(),
            timestamp: DateTime::<Utc>::from_timestamp(ts_secs, 0).unwrap(),
            action: action.to_string(),
            outcome: format!("outcome for {}", action),
            details: String::new(),
            effects: serde_json::Map::new(),
            warnings: Vec::new(),
            tick: 0,
        }
    }

    #[test]
    fn load_recent_returns_all_when_no_after_ts() {
        let path = fresh_log_path();
        // 3 entries across 2 entities, oldest -> newest.
        append_entry_at(&make_entry_full("e1", "A", "a1", 1000), &path).unwrap();
        append_entry_at(&make_entry_full("e2", "B", "b1", 2000), &path).unwrap();
        append_entry_at(&make_entry_full("e1", "A", "a2", 3000), &path).unwrap();
        let v = load_recent_world_actions_at(10, None, &path);
        assert_eq!(v.len(), 3);
        // Most-recent-first order
        assert_eq!(v[0].action, "a2");
        assert_eq!(v[1].action, "b1");
        assert_eq!(v[2].action, "a1");
    }

    #[test]
    fn load_recent_respects_limit() {
        let path = fresh_log_path();
        for i in 0..5 {
            append_entry_at(
                &make_entry_full("e1", "A", &format!("a{}", i), 1000 + i),
                &path,
            )
            .unwrap();
        }
        let v = load_recent_world_actions_at(3, None, &path);
        assert_eq!(v.len(), 3);
        // The last 3 written are a2, a3, a4 (most-recent-first)
        assert_eq!(v[0].action, "a4");
        assert_eq!(v[1].action, "a3");
        assert_eq!(v[2].action, "a2");
    }

    #[test]
    fn load_recent_filters_by_after_ts() {
        let path = fresh_log_path();
        append_entry_at(&make_entry_full("e1", "A", "old_a", 1000), &path).unwrap();
        append_entry_at(&make_entry_full("e2", "B", "old_b", 1500), &path).unwrap();
        append_entry_at(&make_entry_full("e1", "A", "new_a", 2000), &path).unwrap();
        append_entry_at(&make_entry_full("e2", "B", "new_b", 2500), &path).unwrap();
        // after_ts = 1500 strictly: drops entries at t=1000 and t=1500,
        // keeps t=2000 and t=2500.
        let cutoff = DateTime::<Utc>::from_timestamp(1500, 0).unwrap();
        let v = load_recent_world_actions_at(10, Some(cutoff), &path);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].action, "new_b");
        assert_eq!(v[1].action, "new_a");
    }

    #[test]
    fn load_recent_empty_when_file_missing() {
        let mut dir = env::temp_dir();
        dir.push(format!("ow-missing-recent-{}", Uuid::new_v4()));
        let path = history_path_at(&dir);
        assert!(load_recent_world_actions_at(10, None, &path).is_empty());
        // Even with a cutoff
        let cutoff = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        assert!(load_recent_world_actions_at(10, Some(cutoff), &path).is_empty());
    }

    #[test]
    fn load_recent_skips_garbled_lines() {
        // Mix a malformed line in with good entries; the loader must
        // skip it without panicking.
        let path = fresh_log_path();
        append_entry_at(&make_entry_full("e1", "A", "good_a", 1000), &path).unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{ this is not valid json").unwrap();
        }
        append_entry_at(&make_entry_full("e2", "B", "good_b", 2000), &path).unwrap();
        let v = load_recent_world_actions_at(10, None, &path);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].action, "good_b");
        assert_eq!(v[1].action, "good_a");
    }

    // ========================================================================
    // backfill_ticks tests
    // ========================================================================

    #[test]
    fn backfill_ticks_assigns_sequential_ticks_to_zero_tick_entries() {
        // Pre-2026-06-07 entries have tick=0.  Backfill
        // should assign them 1, 2, 3 in append order.
        let path = fresh_log_path();
        let mut e1 = make_entry_full("e1", "A", "first", 1000);
        let mut e2 = make_entry_full("e2", "B", "second", 2000);
        let mut e3 = make_entry_full("e3", "C", "third", 3000);
        e1.tick = 0;
        e2.tick = 0;
        e3.tick = 0;
        append_entry_at(&e1, &path).unwrap();
        append_entry_at(&e2, &path).unwrap();
        append_entry_at(&e3, &path).unwrap();

        let backfilled = backfill_ticks_at(&path);
        assert_eq!(backfilled, 3);

        // Reload and verify the ticks.
        let v = load_for_entity_at("e1", 100, &path);
        assert_eq!(v[0].tick, 1);
        let v = load_for_entity_at("e2", 100, &path);
        assert_eq!(v[0].tick, 2);
        let v = load_for_entity_at("e3", 100, &path);
        assert_eq!(v[0].tick, 3);
    }

    #[test]
    fn backfill_ticks_is_idempotent() {
        // Run backfill twice; the second call must be a no-op
        // (0 entries backfilled).
        let path = fresh_log_path();
        let mut e1 = make_entry_full("e1", "A", "first", 1000);
        e1.tick = 0;
        append_entry_at(&e1, &path).unwrap();
        let first = backfill_ticks_at(&path);
        assert_eq!(first, 1);
        let second = backfill_ticks_at(&path);
        assert_eq!(second, 0);
    }

    #[test]
    fn backfill_ticks_preserves_existing_ticks() {
        // Pre-2026-06-07 entries have tick=0; one entry was
        // already stamped at tick=42 by the new code.  The
        // backfill should NOT overwrite tick=42, and should
        // assign tick=1 to the un-ticked one (since the max
        // existing tick is 42, the next sequential is 43,
        // but the implementation actually starts the
        // sequence at max+1 for the first un-ticked one;
        // verify the actual behavior below).
        let path = fresh_log_path();
        let mut e1 = make_entry_full("e1", "A", "first", 1000);
        let mut e2 = make_entry_full("e2", "B", "second", 2000);
        e1.tick = 42;
        e2.tick = 0;
        append_entry_at(&e1, &path).unwrap();
        append_entry_at(&e2, &path).unwrap();
        let backfilled = backfill_ticks_at(&path);
        assert_eq!(backfilled, 1);
        let v1 = load_for_entity_at("e1", 100, &path);
        assert_eq!(v1[0].tick, 42, "existing tick must be preserved");
        let v2 = load_for_entity_at("e2", 100, &path);
        assert!(v2[0].tick > 42, "new tick must be > existing max");
    }

    #[test]
    fn backfill_ticks_on_missing_file_is_noop() {
        let path = fresh_log_path();  // doesn't exist
        let backfilled = backfill_ticks_at(&path);
        assert_eq!(backfilled, 0);
    }

    #[test]
    fn backfill_ticks_skips_garbled_lines() {
        // Mix a malformed line in; backfill must skip it and
        // backfill the rest without panicking.
        let path = fresh_log_path();
        let mut e1 = make_entry_full("e1", "A", "good_a", 1000);
        e1.tick = 0;
        append_entry_at(&e1, &path).unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{ this is not valid json").unwrap();
        }
        let mut e2 = make_entry_full("e2", "B", "good_b", 2000);
        e2.tick = 0;
        append_entry_at(&e2, &path).unwrap();
        let backfilled = backfill_ticks_at(&path);
        assert_eq!(backfilled, 2);
    }

    // Bring Uuid into scope for the test helpers above
    use uuid::Uuid;
}
