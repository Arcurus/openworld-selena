//! Entity History Module
//! 
//! Handles entity history formatting for LLM context and provides helpers
//! for adding action results to entity histories.

use crate::WorldEntity;
use crate::world_data::WorldSettings;

/// Format entity history for LLM context
/// Shows fully_displayed entries in full, shortened entries in brief form
pub fn format_history_for_llm(
    entity: &WorldEntity,
    settings: &WorldSettings,
) -> String {
    let history = &entity.history;
    let total = history.len();
    
    if total == 0 {
        return format!("{} has no recorded history.", entity.name);
    }
    
    let fully_displayed = settings.history_entries_fully_displayed as usize;
    let shortened = settings.history_entries_shortened as usize;
    
    let mut output = format!("History of {} ({} total):\n", entity.name, total);
    
    // Walk the trailing window: oldest in [shortened] shown briefly, newest
    // [fully_displayed] shown in full. Anything older is truncated.
    // The old code skipped straight to `total - fully_displayed`, which made
    // the shortened entries invisible and let short histories fall into the
    // shortened branch even when the entry fit fully within the window.
    let window = fully_displayed.saturating_add(shortened);
    let start_idx = if total > window { total - window } else { 0 };
    
    for (i, entry) in history.iter().enumerate().skip(start_idx) {
        let fully_start = total.saturating_sub(fully_displayed);
        if i >= fully_start {
            // Show fully
            output.push_str(&format!(
                "  [{}] {}: {} (Result: {})\n",
                entry.timestamp.format("%Y-%m-%d"),
                entry.action,
                entry.details,
                entry.outcome
            ));
        } else {
            // Show shortened (just action and outcome)
            output.push_str(&format!(
                "  [{}] {}: {}\n",
                entry.timestamp.format("%Y-%m-%d"),
                entry.action,
                entry.outcome
            ));
        }
    }
    
    // Note about truncated entries
    if total > fully_displayed + shortened {
        let truncated = total - fully_displayed - shortened;
        output.push_str(&format!("  ... ({} older entries truncated)\n", truncated));
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
    fn test_format_history_for_llm_shortened_window() {
        // With fully_displayed=2 and shortened=2, 5 entries should yield:
        //   - 1 shortened entry (action+outcome only, no details)
        //   - 2 fully-displayed entries (with details)
        //   - 2 truncated (older) entries noted in the "..." line
        let entity = make_entity("Multi", 5);
        let mut settings = WorldSettings::default();
        settings.history_entries_fully_displayed = 2;
        settings.history_entries_shortened = 2;
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Multi (5 total)"));
        // With fully_displayed=2, shortened=2, total=5: 5 - 2 - 2 = 1
        // older truncated entry is reported.
        assert!(out.contains("1 older entries truncated"));
        // Shortened entry: act2 should appear WITHOUT details2.
        assert!(out.contains("act2"));
        assert!(!out.contains("details2"));
        // Fully-displayed entries (act3, act4) include details.
        assert!(out.contains("act3") && out.contains("details3"));
        assert!(out.contains("act4") && out.contains("details4"));
    }

    #[test]
    fn test_format_history_for_llm_shortened_zero_window() {
        // If both windows are 0, everything is truncated.
        let entity = make_entity("Z", 3);
        let mut settings = WorldSettings::default();
        settings.history_entries_fully_displayed = 0;
        settings.history_entries_shortened = 0;
        let out = format_history_for_llm(&entity, &settings);
        assert!(out.contains("History of Z (3 total)"));
        assert!(out.contains("3 older entries truncated"));
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
