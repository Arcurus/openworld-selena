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
    
    // Show most recent entries that are fully displayed
    let start_idx = if total > fully_displayed { total - fully_displayed } else { 0 };
    
    for (i, entry) in history.iter().enumerate().skip(start_idx) {
        if i < total - shortened {
            // These entries are beyond the shortened range, just count them
            continue;
        }
        
        if i >= total - fully_displayed {
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
    // This would return recent history entries
    // For now, just return the entity if it has any history
    if entity.history.is_empty() {
        Vec::new()
    } else {
        // Return the entity itself as a reference
        vec![entity]
    }
}
