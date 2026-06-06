//! Entity History Module
//!
//! Handles entity history formatting for LLM context and provides helpers
//! for adding action results to entity histories.

use crate::WorldEntity;
use crate::world_data::WorldSettings;

// Per Arcurus 2026-06-06 #openworld: character-budget-based history
// display. Show newest entries in full (action + details + outcome)
// up to FULL_CHAR_BUDGET characters, then continue with shortened
// entries (action + outcome only) up to SHORT_CHAR_BUDGET more
// characters, then note any remaining older entries. This replaces
// the previous fixed-entry-count window (`history_entries_fully_displayed`
// / `history_entries_shortened`) which silently dropped everything
// past a fixed count of entries regardless of how short each entry
// was. Character budgets scale gracefully with both entry count
// and entry length.
//
// Tuned by Arcurus 2026-06-06 #openworld: bumped both from 2500→10000
// after the Velora test showed 1 huge full entry was eating the
// entire 2500-char full budget on its own. At 10000+10000, the
// LLM sees ~20× more history per call — still bounded, still
// scaled by entry length, but no longer a one-giant-entry cap.
//
// `WorldSettings.history_entries_fully_displayed` and
// `WorldSettings.history_entries_shortened` are no longer consulted
// here (kept on the struct for save-file backwards compatibility).
const FULL_CHAR_BUDGET: usize = 10000;
const SHORT_CHAR_BUDGET: usize = 10000;

/// Format entity history for LLM context.
/// Walks the entity's history newest-first, showing recent entries in
/// full up to FULL_CHAR_BUDGET chars, then older entries in shortened
/// form (action + outcome) up to SHORT_CHAR_BUDGET more chars, then a
/// trailing `... (N even older entries omitted)` line. At least one
/// entry is always shown in full even if it exceeds the budget, so a
/// single huge entry doesn't get silently dropped.
pub fn format_history_for_llm(
    entity: &WorldEntity,
    _settings: &WorldSettings,
) -> String {
    let history = &entity.history;
    let total = history.len();

    if total == 0 {
        return format!("{} has no recorded history.", entity.name);
    }

    let mut output = format!("History of {} ({} total):\n", entity.name, total);
    let mut full_chars = 0usize;
    let mut short_chars = 0usize;
    let mut shown_full = 0usize;
    let mut shown_short = 0usize;
    let mut in_short_mode = false;

    // Walk newest -> oldest.  `iter().rev()` does that without reversing
    // the whole Vec, which matters for entities with 300+ entries.
    for entry in history.iter().rev() {
        if !in_short_mode {
            let line = format!(
                "  [{}] {}: {} (Result: {})\n",
                entry.timestamp.format("%Y-%m-%d"),
                entry.action,
                entry.details,
                entry.outcome
            );
            // Always show at least one entry in full, even if it
            // exceeds the budget — otherwise a single huge entry
            // would silently disappear.
            if shown_full > 0 && full_chars + line.len() > FULL_CHAR_BUDGET {
                in_short_mode = true;
                // Fall through to short mode for THIS entry so we
                // don't lose it entirely.
            } else {
                output.push_str(&line);
                full_chars += line.len();
                shown_full += 1;
                continue;
            }
        }
        // Short mode: action + outcome only.
        let line = format!(
            "  [{}] {}: {}\n",
            entry.timestamp.format("%Y-%m-%d"),
            entry.action,
            entry.outcome
        );
        if shown_short > 0 && short_chars + line.len() > SHORT_CHAR_BUDGET {
            // Both budgets exhausted — stop walking.
            break;
        }
        output.push_str(&line);
        short_chars += line.len();
        shown_short += 1;
    }

    let shown_total = shown_full + shown_short;
    if total > shown_total {
        let remaining = total - shown_total;
        output.push_str(&format!(
            "  ... ({} even older entries omitted — too long for this context)\n",
            remaining
        ));
    }

    output
}

/// Format multiple entities' history for LLM context
pub fn format_histories_for_entities(
    entities: &[&WorldEntity],
    settings: &WorldSettings,
) -> String {
    let mut output = String::new();
    
    for entity in entities {
        output.push_str(&format_history_for_llm(entity, settings));
        output.push('\n');
    }
    
    output
}

/// Add an action result to an entity's history
pub fn add_to_history(
    entity: &mut WorldEntity,
    action: &str,
    details: &str,
    outcome: &str,
) {
    entity.add_history(action, details, outcome);
}

/// Get recent history entries (most recent first)
pub fn get_recent_entries(
    entity: &WorldEntity,
    count: usize,
) -> Vec<&crate::WorldEntity> {
    // NOTE: This is a stub. The signature returns `Vec<&WorldEntity>` but the
    // name and `count` parameter suggest it should return `Vec<&HistoryEntry>`.
    // The function is currently unused in the codebase (verified 2026-06-04).
    // Tests below document the current behavior so refactors don't break
    // silently; rewrite when the function is wired up.
    let _ = count; // count is currently ignored
    if entity.history.is_empty() {
        Vec::new()
    } else {
        // Return the entity itself as a reference
        vec![entity]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_data::{HistoryEntry, WorldSettings};

    fn make_entity(name: &str, n: usize) -> WorldEntity {
        // Build a fresh entity and push n history entries with predictable
        // distinct action strings so we can assert which entries appear
        // in the formatter output. (Timestamps are auto-set to "now".)
        let mut e = WorldEntity::new("character", name, 0.0, 0.0);
        for i in 0..n {
            e.history.push(HistoryEntry::new(
                &format!("act{}", i),
                &format!("details{}", i),
                &format!("outcome{}", i),
            ));
        }
        e
    }

    #[test]
    fn test_format_history_for_llm_empty() {
        let entity = WorldEntity::new("location", "Empty", 0.0, 0.0);
        let settings = WorldSettings::default();
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("Empty"));
        assert!(out.contains("no recorded history"));
    }

    #[test]
    fn test_format_history_for_llm_single_entry() {
        let entity = make_entity("Solo", 1);
        let settings = WorldSettings::default();
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Solo (1 total)"));
        // Fully-displayed entries show action + details + outcome.
        assert!(out.contains("act0"));
        assert!(out.contains("details0"));
        assert!(out.contains("outcome0"));
    }

    #[test]
    fn test_format_history_for_llm_short_history_fits_fully() {
        // 5 small entries: each is ~50 chars, 5 * 50 = 250 chars total,
        // well under the 2500-char full budget. All 5 should be shown
        // in full (with details), no shortening, no omission line.
        let entity = make_entity("Multi", 5);
        let settings = WorldSettings::default();
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Multi (5 total)"));
        for i in 0..5 {
            assert!(out.contains(&format!("act{i}")), "missing act{i} in:\n{out}");
            assert!(out.contains(&format!("details{i}")), "missing details{i} in:\n{out}");
        }
        assert!(!out.contains("older entries omitted"));
    }

    #[test]
    fn test_format_history_for_llm_huge_newest_entry_always_shown() {
        // The newest entry is huge (>2500 chars), and we push 20
        // normal entries AFTER it (so they become older). The "at
        // least one full entry" guarantee must surface the huge one
        // even though it alone exceeds FULL_CHAR_BUDGET, and the
        // remaining 20 entries should be split between full and
        // short modes.  None should be omitted (the test entity has
        // 21 entries total, well within combined budget capacity).
        let mut entity = WorldEntity::new("character", "Burst", 0.0, 0.0);
        // 20 short entries first (so the huge one is the newest).
        for i in 0..20 {
            entity.history.push(HistoryEntry::new(
                &format!("act{i}"),
                &format!("details{i}"),
                &format!("outcome{i}"),
            ));
        }
        // Newest = huge (>2500 chars).
        let huge = "x".repeat(3000);
        entity.history.push(HistoryEntry::new(
            "huge_action",
            &huge,
            "huge_outcome",
        ));
        let settings = WorldSettings::default();
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Burst (21 total)"));
        // The huge newest entry is the FIRST one processed (iter().rev())
        // and must end up in the output — "always show at least one
        // full" guarantee.
        assert!(out.contains("huge_action"));
        assert!(out.contains(&huge));
        // All 20 short entries should be visible (combined budget
        // is 5000 chars, plenty for 20 small entries even in short
        // mode).
        let full_count = (0..20).filter(|i| out.contains(&format!("details{i}"))).count();
        let short_count = (0..20).filter(|i| out.contains(&format!("act{i}")) && !out.contains(&format!("details{i}"))).count();
        assert_eq!(full_count + short_count, 20, "all 20 short entries should appear");
        assert!(full_count > 0 || short_count > 0);
        // Nothing omitted — we showed all 21.
        assert!(!out.contains("older entries omitted"));
    }

    #[test]
    fn test_format_history_for_llm_long_entries_get_truncated() {
        // 200 entries with ~500-char details.  Each full line is
        // ~540 chars, so only ~18 fit in FULL_CHAR_BUDGET=10000.  The
        // remaining ~182 entries walk in short mode (~30 chars each),
        // of which ~333 fit in SHORT_CHAR_BUDGET=10000 — but we only
        // have 182 left, so they all fit.  Total shown 200, omitted 0.
        // We need a much bigger entity to actually trigger omission
        // with the new budgets.  (See the next test for that.)
        let mut entity = WorldEntity::new("character", "Many", 0.0, 0.0);
        let long = "lorem ipsum dolor sit amet ".repeat(20); // ~500 chars
        for i in 0..200 {
            entity.history.push(HistoryEntry::new(
                &format!("act{i}"),
                &long,
                &format!("outcome{i}"),
            ));
        }
        let settings = WorldSettings::default();
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Many (200 total)"));
        // Newest (act199) must be visible.
        assert!(out.contains("act199"));
        // No omission line — everything fits in the 20K-char combined budget.
        assert!(!out.contains("older entries omitted"));
    }

    #[test]
    fn test_format_history_for_llm_very_long_history_omits_oldest() {
        // With the 10000+10000 budgets, a realistic 500-entry entity
        // with 500-char details still overflows: each full line is
        // ~540 chars, so ~18 fit in full mode, the remaining ~482
        // walk in short mode (~30 chars each), of which ~333 fit
        // in short mode ⇒ ~351 total shown, ~149 omitted (the
        // OLDEST 149 entries).  Newest (act499) visible, oldest
        // (act0, act1, ...) dropped.
        let mut entity = WorldEntity::new("character", "Crowd", 0.0, 0.0);
        let long = "lorem ipsum dolor sit amet ".repeat(20); // ~500 chars
        for i in 0..500 {
            entity.history.push(HistoryEntry::new(
                &format!("act{i}"),
                &long,
                &format!("outcome{i}"),
            ));
        }
        let settings = WorldSettings::default();
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Crowd (500 total)"));
        // Newest (act499) must be visible.
        assert!(out.contains("act499"));
        // Some entries got omitted (oldest ones, since we walk newest-first).
        assert!(out.contains("older entries omitted"), "expected omission line in:\n{out}");
        // The OLDEST entries (act0, act1, ...) should be the ones
        // that got dropped, not the newest.
        assert!(!out.contains("act0: "), "act0 should have been omitted, but is in output:\n{out}");
        assert!(!out.contains("act1: "), "act1 should have been omitted, but is in output:\n{out}");
    }

    #[test]
    fn test_format_history_for_llm_per_world_settings_ignored() {
        // The per-world history_entries_fully_displayed/_shortened
        // fields are no longer consulted (kept on the struct for
        // save-file backwards compatibility).  Setting them to 0
        // should NOT cause the formatter to drop everything — only
        // an entity that genuinely overflows both 2500-char budgets
        // triggers the omission line.
        let entity = make_entity("Z", 3);
        let mut settings = WorldSettings::default();
        settings.history_entries_fully_displayed = 0;
        settings.history_entries_shortened = 0;
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Z (3 total)"));
        // 3 small entries easily fit in 2500 chars, so all 3 are
        // shown in full.
        assert!(out.contains("act0") && out.contains("details0"));
        assert!(out.contains("act1") && out.contains("details1"));
        assert!(out.contains("act2") && out.contains("details2"));
        assert!(!out.contains("older entries omitted"));
    }

    #[test]
    fn test_format_histories_for_entities_joins_outputs() {
        let a = make_entity("A", 1);
        let b = make_entity("B", 2);
        let settings = WorldSettings::default();
        let out = format_histories_for_entities(&[&a, &b], &settings);
        assert!(out.contains("History of A"));
        assert!(out.contains("History of B"));
        // Entries are joined with a blank line between them.
        assert!(out.contains("\n\n"));
    }

    #[test]
    fn test_add_to_history_appends_entry() {
        let mut entity = WorldEntity::new("character", "Hero", 0.0, 0.0);
        assert_eq!(entity.history.len(), 0);
        add_to_history(&mut entity, "fight", "slays dragon", "wins");
        assert_eq!(entity.history.len(), 1);
        let h = &entity.history[0];
        assert_eq!(h.action, "fight");
        assert_eq!(h.details, "slays dragon");
        assert_eq!(h.outcome, "wins");
    }

    #[test]
    fn test_get_recent_entries_empty_returns_empty_vec() {
        // Documents the stub: empty history -> empty vec.
        let entity = WorldEntity::new("character", "Fresh", 0.0, 0.0);
        let out = get_recent_entries(&entity, 5);
        assert!(out.is_empty());
    }

    #[test]
    fn test_get_recent_entries_nonempty_returns_entity() {
        // Documents the stub: non-empty history -> vec containing the entity.
        // (Signature should probably be Vec<&HistoryEntry>; tracked as
        // dead/stub code to rewrite when wired up.)
        let entity = make_entity("Some", 1);
        let out = get_recent_entries(&entity, 3);
        assert_eq!(out.len(), 1);
        assert!(std::ptr::eq(out[0], &entity));
    }
}
