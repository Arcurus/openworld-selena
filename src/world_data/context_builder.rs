//! Builds the per-entity context blocks used by the LLM action prompt.
//!
//! Both `action_context_handler` and `entity_action` in main.rs need the
//! same five context strings (property, history, nearby entities, power
//! tier, world events) and the same template rendering. This module
//! extracts that duplicated work so the two handlers stay in sync.
//!
//! Extracted during the 23:15 worker run on 2026-06-03 as part of
//! todo 5143f67c (DRY refactor for entity_action). See /docs/.

use crate::world_data::entity_history::format_history_for_llm;
use crate::world_data::world::{EntityTypeStats, World};
use crate::world_data::world_entity::WorldEntity;

/// All the context strings an LLM action prompt needs. Built once per
/// request by `build_action_context`, then handed to `build_action_prompt`.
#[derive(Debug, Clone, Default)]
pub struct ActionContext {
    pub prop_context: String,
    pub entity_history_str: String,
    pub nearby_entities_str: String,
    pub power_tier_str: String,
    pub world_events_str: String,
    /// Current history_summary for the entity (or a placeholder
    /// when none exists yet). Goes into `{history_summary}`.
    pub history_summary_str: String,
    /// Max characters the LLM may use for the summary it returns.
    /// Resolved effective cap: per-world override if non-zero,
    /// otherwise the global default from
    /// `settings.json → llm.default_max_history_summary_chars`.
    /// Goes into `{max_history_summary_chars}`.
    pub max_history_summary_chars: u32,
    /// Length in chars of the *currently stored* `entity.history_summary`
    /// (NOT the truncated version — what the LLM sent last turn that
    /// is now sitting in storage). Used to compute "used / free" in
    /// `{history_summary_header}` so the LLM knows how much budget it
    /// has left for the next `history_summary_replace` edit. 0 if no
    /// summary has ever been written for this entity.
    pub history_summary_chars_used: u64,
}

/// Resolve the effective per-entity history-summary char cap.
/// Per-world `WorldSettings.max_history_summary_chars` overrides
/// the global default if non-zero; 0 means "use the global default".
pub fn resolve_max_history_summary_chars(world: &World, global_default: u32) -> u32 {
    if world.settings.max_history_summary_chars > 0 {
        world.settings.max_history_summary_chars
    } else {
        global_default
    }
}

/// Same as `resolve_max_history_summary_chars` but also returns the
/// *source* of the cap ("world" vs "global") so callers (e.g. the
/// API layer) can tell the client which knob controls the value.
/// Per Arcurus 2026-06-04: expose the cap on the entity API so the
/// web client's History Summary card can show the real number.
pub fn resolve_max_history_summary_chars_with_source(
    world: &World,
    global_default: u32,
) -> (u32, &'static str) {
    if world.settings.max_history_summary_chars > 0 {
        (world.settings.max_history_summary_chars, "world")
    } else {
        (global_default, "global")
    }
}

/// Build the full action context for an entity. Used by both
/// `action_context_handler` and `entity_action` to keep their
/// prompt construction in sync.
pub fn build_action_context(
    world: &World,
    entity: &WorldEntity,
    global_default_max_history_summary_chars: u32,
) -> ActionContext {
    let stats = world.calculate_stats();
    let type_stats = stats.by_type.get(&entity.entity_type);

    ActionContext {
        prop_context: build_property_context(entity, type_stats),
        entity_history_str: format_history_for_llm(entity, &world.settings),
        nearby_entities_str: build_nearby_entities_str(world, entity),
        power_tier_str: build_power_tier_str(entity),
        world_events_str: build_world_events_str(world),
        history_summary_str: entity.history_summary.clone()
            .unwrap_or_else(|| "(no history summary yet)".to_string()),
        max_history_summary_chars: resolve_max_history_summary_chars(
            world,
            global_default_max_history_summary_chars,
        ),
        history_summary_chars_used: entity
            .history_summary
            .as_ref()
            .map(|s| s.chars().count() as u64)
            .unwrap_or(0),
    }
}

/// Render the "Current History Summary" header line for the LLM
/// prompt. Surfaces the cap, the current length, and the free budget
/// so the LLM can plan the size of its next `history_summary_replace`
/// edit. The full summary body itself is rendered separately via
/// `{history_summary}`.
fn build_history_summary_header(ctx: &ActionContext) -> String {
    let cap = ctx.max_history_summary_chars as u64;
    let used = ctx.history_summary_chars_used;
    if used == 0 {
        format!("Current History Summary (cap {} chars, none yet — first edit sets it):", cap)
    } else if used > cap {
        format!(
            "Current History Summary (cap {} chars, used {}, OVER by {} — please trim with a surgical edit or !ALL! rewrite):",
            cap, used, used - cap
        )
    } else {
        let free = cap - used;
        format!(
            "Current History Summary (cap {} chars, used {}, {} free):",
            cap, used, free
        )
    }
}

/// Render the EntityAction.md template with all placeholders filled.
pub fn build_action_prompt(
    world_name: &str,
    entity: &WorldEntity,
    ctx: &ActionContext,
    template: &str,
) -> String {
    template
        .replace("{world_name}", world_name)
        .replace("{entity_name}", &entity.name)
        .replace("{entity_type}", &entity.entity_type)
        .replace("{description}", &entity.description)
        .replace("{tags}", &entity.tags.join(", "))
        .replace("{x}", &format!("{:.1}", entity.x))
        .replace("{y}", &format!("{:.1}", entity.y))
        .replace("{property_context}", &ctx.prop_context)
        .replace("{power_tier}", &ctx.power_tier_str)
        .replace("{entity_history}", &ctx.entity_history_str)
        .replace("{nearby_entities}", &ctx.nearby_entities_str)
        .replace("{world_events}", &ctx.world_events_str)
        .replace("{history_summary}", &ctx.history_summary_str)
        .replace("{max_history_summary_chars}", &ctx.max_history_summary_chars.to_string())
        .replace("{history_summary_header}", &build_history_summary_header(ctx))
}

fn build_property_context(entity: &WorldEntity, type_stats: Option<&EntityTypeStats>) -> String {
    let mut prop_context = String::new();
    for (key, value) in &entity.properties_int {
        let relative = if let Some(ts) = type_stats {
            if let Some(stat) = ts.properties_int.get(key) {
                World::get_relative_value(*value as f64, stat.min, stat.max, stat.avg)
            } else {
                "unknown"
            }
        } else {
            "unknown"
        };
        prop_context.push_str(&format!("  - {}: {} ({})\n", key, value, relative));
    }
    for (key, value) in &entity.properties_float {
        prop_context.push_str(&format!("  - {}: {:.2}\n", key, value));
    }
    prop_context
}

fn build_nearby_entities_str(world: &World, entity: &WorldEntity) -> String {
    // No distance cutoff: consider every entity in the world, then
    // let the per-section algorithm pick the top N.  Per Arcurus
    // 2026-06-07 #openworld: the previous 150-unit radius was a
    // hidden limit that hid legitimate faraway-but-significant
    // entities (e.g. a high-power legend in the next kingdom).
    // The cap is now per-section (top 5), not per-radius.
    //
    // System entities (world_clock, anything tagged 'meta') are
    // still filtered out — they're bookkeeping entities, not
    // narrative actors, and the LLM doesn't need them in the
    // nearby list.  This matches the `include_system=false` filter
    // on the public /api/entities endpoint.
    let nearby: Vec<&WorldEntity> = world.entities.values()
        .filter(|e| e.id != entity.id && !e.is_system_entity())
        .collect();
    if nearby.is_empty() {
        // Defensive fallback; in practice the world always has more
        // than one entity, but keep the message for the single-entity
        // edge case.
        return String::from("No other entities nearby.");
    }

    // Per Arcurus 2026-06-06 (#openworld): split nearby entities
    // into three groups:
    //   - "Locations"  (entity_type == "location", top MAX_NEARBY_LOCATIONS
    //                   by influence score)
    //   - "Factions"   (entity_type == "faction", top MAX_NEARBY_FACTIONS
    //                   nearest by distance)
    //   - "Characters" (everything else, top MAX_NEARBY_CHARACTERS by
    //                   influence score)
    //
    // Locations and Characters are sorted by influence score so the
    // most relevant neighbours surface first:
    //
    //     score = max(1, power + visibility) / distance
    //             × (sleeping multiplier, 0.01 if "sleeping" in tags else 1.0)
    //
    // Factions are sorted by DISTANCE ASCENDING (nearest first)
    // and capped at MAX_NEARBY_FACTIONS, so the LLM gets a tight
    // "here are the closest organised groups" picture without
    // context-bloat.  Arcurus 2026-06-07 #openworld.
    //
    // The `max(1, ...)` floor keeps distance from dominating the
    // sort (otherwise a low-power bystander right next to the
    // subject would outrank a powerful legend in the next village).
    // `power` and `visibility` are both signed i64 properties, so
    // visibility can be negative (entity in hiding / suppressed) and
    // the floor still kicks in if power + visibility < 1.
    //
    // The sleeping tag multiplier is the same 0.01× value the
    // action-selector uses (DEPRIO_TAG_MULTIPLIERS in
    // scheduled_actions.py), so a sleeping legend that happens to
    // be near is still listed (so the LLM knows they exist) but
    // sorts to the BOTTOM of the nearby block — it doesn't get
    // prioritized over awake, present neighbours just because of
    // its title.  Arcurus 2026-06-06 #openworld.
    let mut locations: Vec<(&WorldEntity, f64, f64)> = Vec::new();
    let mut factions: Vec<(&WorldEntity, f64, f64)> = Vec::new();
    let mut characters: Vec<(&WorldEntity, f64, f64)> = Vec::new();
    for other in &nearby {
        let dist = ((other.x - entity.x).powi(2) + (other.y - entity.y).powi(2)).sqrt();
        if dist < 0.001 {
            // Zero-distance means same position; skip to avoid
            // divide-by-zero (rare: only happens if the world
            // places two entities at the exact same coords).
            continue;
        }
        let power = other.properties_int.get("power").copied().unwrap_or(0);
        let visibility = other.properties_int.get("visibility").copied().unwrap_or(0);
        let numerator = std::cmp::max(1, power.saturating_add(visibility));
        let sleeping_mult: f64 = if other.tags.iter().any(|t| t == "sleeping") {
            0.01
        } else {
            1.0
        };
        let score = (numerator as f64 / dist) * sleeping_mult;
        match other.entity_type.as_str() {
            "location" => locations.push((other, dist, score)),
            "faction" => factions.push((other, dist, score)),
            _ => characters.push((other, dist, score)),
        }
    }
    // Highest score first (most influential neighbours), then cap
    // each section at its top-N.  The cap is what keeps the prompt
    // bounded now that the radius cutoff is gone.
    locations.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    locations.truncate(MAX_NEARBY_LOCATIONS);
    characters.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    characters.truncate(MAX_NEARBY_CHARACTERS);
    // Factions: nearest first, capped at MAX_NEARBY_FACTIONS.
    factions.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    factions.truncate(MAX_NEARBY_FACTIONS);

    let mut s = String::new();
    if !locations.is_empty() {
        s.push_str(&format!("### Nearby Locations (top {} by influence)\n", MAX_NEARBY_LOCATIONS));
        for (other, dist, score) in &locations {
            s.push_str(&format_nearby_entry(other, *dist));
        }
        s.push('\n');
    }
    if !characters.is_empty() {
        s.push_str(&format!("### Nearby Characters (top {} by influence)\n", MAX_NEARBY_CHARACTERS));
        for (other, dist, score) in &characters {
            s.push_str(&format_nearby_entry(other, *dist));
        }
        s.push('\n');
    }
    if !factions.is_empty() {
        s.push_str(&format!("### Nearby Factions ({} nearest)\n", MAX_NEARBY_FACTIONS));
        for (other, dist, score) in &factions {
            s.push_str(&format_nearby_entry(other, *dist));
        }
        s.push('\n');
    }
    s
}

/// Maximum number of locations to include in the "Nearby Locations"
/// section of the LLM context.  Locations are sorted by influence
/// score (highest first) and the top N are kept.  Tunable — see
/// docs/world-mechanics.md.
const MAX_NEARBY_LOCATIONS: usize = 5;

/// Maximum number of characters to include in the "Nearby Characters"
/// section of the LLM context.  Characters are sorted by influence
/// score (highest first) and the top N are kept.  Tunable — see
/// docs/world-mechanics.md.
const MAX_NEARBY_CHARACTERS: usize = 5;

/// Maximum number of factions to include in the "Nearby Factions"
/// section of the LLM context.  Factions are sorted by distance
/// ascending (nearest first), so this is "the N closest factions
/// to the subject."  Tunable — see docs/world-mechanics.md.
const MAX_NEARBY_FACTIONS: usize = 5;

/// Format one nearby-entity row for the LLM context.
/// Shows name/type/distance/power/description/props.  Per Arcurus
/// 2026-06-07 (#openworld): the previous version also rendered
/// `visibility` and the internal influence `score` here, but those
/// are sort-internal details, not facts the LLM needs to act on.
/// The LLM already has the sorted list order, so the score was
/// redundant, and the visibility stat is more useful in the
/// per-entity `Properties:` block (when present) than as a
/// metadata field next to the name.  The `💤×0.01` marker is kept
/// because the sleeping state is a real semantic signal the LLM
/// should see.
fn format_nearby_entry(other: &WorldEntity, dist: f64) -> String {
    let power = other.properties_int.get("power").copied().unwrap_or(0);
    let is_sleeping = other.tags.iter().any(|t| t == "sleeping");
    let mut s = format!(
        "- **{}** ({}) — dist {:.1}, power {}{}\n",
        other.name,
        other.entity_type,
        dist,
        power,
        if is_sleeping { " 💤×0.01" } else { "" },
    );
    if !other.description.is_empty() {
        s.push_str(&format!("  {}\n", other.description));
    }
    let key_props: Vec<String> = other
        .properties_int
        .iter()
        .take(3)
        .map(|(k, v)| format!("{}: {}", k, v))
        .collect();
    if !key_props.is_empty() {
        s.push_str(&format!("  Properties: {}\n", key_props.join(", ")));
    }
    s
}

fn build_power_tier_str(entity: &WorldEntity) -> String {
    let power_keys = ["power", "strength", "army_size", "wealth", "influence"];
    let mut total_power: i64 = 0;
    for key in &power_keys {
        if let Some(v) = entity.properties_int.get(*key) {
            total_power += v;
        }
    }
    for (_, v) in &entity.properties_float {
        if *v > 0.0 {
            total_power += *v as i64;
        }
    }
    if total_power >= 1000 {
        format!(
            "Legendary (Power: {}) - Among the most powerful beings in the world",
            total_power
        )
    } else if total_power >= 500 {
        format!(
            "Epic (Power: {}) - A formidable force to be reckoned with",
            total_power
        )
    } else if total_power >= 200 {
        format!(
            "Rare (Power: {}) - Above average strength and influence",
            total_power
        )
    } else if total_power >= 50 {
        format!(
            "Uncommon (Power: {}) - A competent and capable individual",
            total_power
        )
    } else {
        format!("Common (Power: {}) - An ordinary entity in the world", total_power)
    }
}

fn build_world_events_str(world: &World) -> String {
    if world.active_events.is_empty() {
        return String::new();
    }
    let mut s = String::from("## Active World Events\n\n");
    for event in &world.active_events {
        if event.active {
            s.push_str(&format!("### {}\n{}", event.name, event.description));
            if !event.influence.is_empty() {
                s.push_str(&format!(
                    "\n**How this affects entities:** {}",
                    event.influence
                ));
            }
            s.push_str("\n\n");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use uuid::Uuid;

    /// Build a test entity at coords far from the world clock (which lives at 0,0)
    /// so nearby-entity tests aren't polluted by the clock.
    fn make_entity(name: &str, x: f64, y: f64) -> WorldEntity {
        let mut e = WorldEntity::new("hero", name, x, y);
        e.description = "A test entity".to_string();
        e.tags = vec!["test".to_string()];
        e
    }

    fn make_world() -> World {
        World::new("test_world")
    }

    #[test]
    fn property_context_empty_entity_returns_empty_string() {
        let entity = make_entity("Test", 10000.0, 10000.0);
        let result = build_property_context(&entity, None);
        assert_eq!(result, "");
    }

    #[test]
    fn property_context_with_int_props_includes_relative_unknown_when_no_stats() {
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_int.insert("power".to_string(), 50);
        entity.properties_int.insert("wealth".to_string(), 200);
        let result = build_property_context(&entity, None);
        assert!(result.contains("power: 50 (unknown)"));
        assert!(result.contains("wealth: 200 (unknown)"));
    }

    #[test]
    fn property_context_with_float_props_formats_two_decimals() {
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_float.insert("speed".to_string(), 1.5);
        let result = build_property_context(&entity, None);
        assert!(result.contains("speed: 1.50"));
    }

    #[test]
    fn power_tier_legendary_above_1000() {
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_int.insert("power".to_string(), 1500);
        let result = build_power_tier_str(&entity);
        assert!(result.starts_with("Legendary"), "got: {}", result);
    }

    #[test]
    fn power_tier_epic_between_500_and_999() {
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_int.insert("strength".to_string(), 600);
        let result = build_power_tier_str(&entity);
        assert!(result.starts_with("Epic"), "got: {}", result);
    }

    #[test]
    fn power_tier_rare_between_200_and_499() {
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_int.insert("army_size".to_string(), 300);
        let result = build_power_tier_str(&entity);
        assert!(result.starts_with("Rare"), "got: {}", result);
    }

    #[test]
    fn power_tier_uncommon_between_50_and_199() {
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_int.insert("wealth".to_string(), 100);
        let result = build_power_tier_str(&entity);
        assert!(result.starts_with("Uncommon"), "got: {}", result);
    }

    #[test]
    fn power_tier_common_below_50() {
        let entity = make_entity("Test", 10000.0, 10000.0);
        let result = build_power_tier_str(&entity);
        assert!(result.starts_with("Common"), "got: {}", result);
    }

    #[test]
    fn power_tier_sums_float_props_when_positive() {
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_float.insert("magic".to_string(), 250.0);
        let result = build_power_tier_str(&entity);
        // 250 -> Rare tier
        assert!(result.starts_with("Rare"), "got: {}", result);
    }

    #[test]
    fn nearby_entities_empty_world_returns_no_other_entities() {
        let world = make_world();
        // Place the test entity far from clock; we use a sentinel
        // entity that doesn't exist in the world, so the helper
        // simply returns "no nearby entities".
        let entity = make_entity("Lone", 50000.0, 50000.0);
        let result = build_nearby_entities_str(&world, &entity);
        assert_eq!(result, "No other entities nearby.");
    }

    #[test]
    fn nearby_entities_excludes_self() {
        let mut world = make_world();
        let entity = make_entity("Self", 10000.0, 10000.0);
        let id = entity.id;
        world.entities.insert(id, entity.clone());
        let result = build_nearby_entities_str(&world, &entity);
        assert!(!result.contains("**Self**"));
    }

    #[test]
    fn nearby_entities_includes_close_neighbor() {
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let mut other = make_entity("Neighbor", 10050.0, 10050.0);
        other.properties_int.insert("power".to_string(), 10);
        let me_id = me.id;
        let other_id = other.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(other_id, other);
        let result = build_nearby_entities_str(&world, &me);
        assert!(result.contains("**Neighbor**"));
        assert!(!result.contains("**Me**"));
        // Format: section header + the metadata line that now shows
        // `dist` and `power` only (visibility + score were removed
        // per Arcurus 2026-06-07 — they were sort-internal noise
        // the LLM doesn't need).
        assert!(result.contains("### Nearby Characters"));
        assert!(result.contains("power 10"));
        assert!(!result.contains("visibility 0"), "visibility stat was removed from the metadata line");
        assert!(!result.contains("score "), "internal influence score was removed from the metadata line");
        assert!(result.contains("Properties: power: 10"));
    }

    #[test]
    fn nearby_entities_splits_locations_from_characters() {
        // Place a location and a character close to me; verify they
        // appear under different section headers.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let mut a_loc = make_entity("TownSquare", 10010.0, 10010.0);
        a_loc.entity_type = "location".to_string();
        a_loc.properties_int.insert("power".to_string(), 5);
        let mut a_char = make_entity("Stranger", 10020.0, 10020.0);
        a_char.entity_type = "character".to_string();
        a_char.properties_int.insert("power".to_string(), 5);
        let me_id = me.id;
        let loc_id = a_loc.id;
        let char_id = a_char.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(loc_id, a_loc);
        world.entities.insert(char_id, a_char);
        let result = build_nearby_entities_str(&world, &me);
        // Both section headers present.
        assert!(result.contains("### Nearby Locations"));
        assert!(result.contains("### Nearby Characters"));
        // Each entry shows up under its own header.
        let loc_section_start = result.find("### Nearby Locations").unwrap();
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let loc_section = &result[loc_section_start..char_section_start];
        let char_section = &result[char_section_start..];
        assert!(loc_section.contains("**TownSquare**"));
        assert!(!loc_section.contains("**Stranger**"));
        assert!(char_section.contains("**Stranger**"));
        assert!(!char_section.contains("**TownSquare**"));
    }

    #[test]
    fn nearby_entities_sorted_by_influence_score() {
        // Two characters: one with high power (sorted first) and one
        // with zero power.  Equal distance, so the higher-power one
        // should appear first within the "Nearby Characters" block.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let mut weak = make_entity("Weakling", 10030.0, 10030.0);
        weak.entity_type = "character".to_string();
        weak.properties_int.insert("power".to_string(), 1);
        let mut strong = make_entity("Strongman", 10030.0, 10030.0);
        strong.entity_type = "character".to_string();
        strong.properties_int.insert("power".to_string(), 500);
        let me_id = me.id;
        let weak_id = weak.id;
        let strong_id = strong.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(weak_id, weak);
        world.entities.insert(strong_id, strong);
        let result = build_nearby_entities_str(&world, &me);
        // Pull out just the "Nearby Characters" section.
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let char_section = &result[char_section_start..];
        let pos_strong = char_section.find("**Strongman**").unwrap();
        let pos_weak = char_section.find("**Weakling**").unwrap();
        assert!(pos_strong < pos_weak, "Strongman should appear before Weakling");
    }

    #[test]
    fn nearby_entities_sleeping_tag_sorts_to_bottom() {
        // Two characters at equal distance: one awake with HIGH
        // power, one sleeping with LOWER power.  Even though the
        // sleeping entity has lower raw power, the test is
        // specifically that the 0.01× multiplier suppresses the
        // sleeping entity.  Compare:
        //   awake  = max(1, 500)/42.4 ≈ 11.79
        //   sleeping = max(1, 200)/42.4 × 0.01 ≈ 0.0472
        // The awake one sorts first.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let mut awake = make_entity("Weakling", 10030.0, 10030.0);
        awake.entity_type = "character".to_string();
        awake.properties_int.insert("power".to_string(), 500);
        let mut sleeping = make_entity("SleepingLegend", 10030.0, 10030.0);
        sleeping.entity_type = "character".to_string();
        sleeping.properties_int.insert("power".to_string(), 200);
        sleeping.tags.push("sleeping".to_string());
        let me_id = me.id;
        let awake_id = awake.id;
        let sleeping_id = sleeping.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(awake_id, awake);
        world.entities.insert(sleeping_id, sleeping);
        let result = build_nearby_entities_str(&world, &me);
        // Both should appear in the Characters section.
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let char_section = &result[char_section_start..];
        let pos_awake = char_section.find("**Weakling**").unwrap();
        let pos_sleeping = char_section.find("**SleepingLegend**").unwrap();
        assert!(
            pos_awake < pos_sleeping,
            "the awake weakling should sort BEFORE the sleeping legend (sleeping ×0.01)"
        );
        // The sleeping row should also carry the 💤 marker in the output line.
        assert!(result.contains("SleepingLegend") && result.contains("💤×0.01"),
            "sleeping row should be tagged with the 💤×0.01 marker");
    }

    #[test]
    fn nearby_entities_sleeping_appears_after_awake_equivalent() {
        // Two equivalent entities at the same position, equal power,
        // equal visibility.  The only difference is the `sleeping`
        // tag on one of them.  Awake must appear before sleeping in
        // the rendered output (sort still works the same way; we
        // just no longer print the score that proved it).
        //
        // Replaces the previous
        // `nearby_entities_sleeping_score_is_one_hundredth_of_baseline`
        // test, which used to verify the 100× ratio in the printed
        // score string.  After visibility/score were removed from
        // the metadata line (Arcurus 2026-06-07), the test was
        // rewritten to verify the same effect via list order.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let mut e_awake = make_entity("TestEntity", 10030.0, 10030.0);
        e_awake.entity_type = "character".to_string();
        e_awake.properties_int.insert("power".to_string(), 100);
        e_awake.properties_int.insert("visibility".to_string(), 50);
        let mut e_sleeping = e_awake.clone();
        e_sleeping.id = Uuid::new_v4();
        e_sleeping.tags.push("sleeping".to_string());
        let me_id = me.id;
        let awake_id = e_awake.id;
        let sleeping_id = e_sleeping.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(awake_id, e_awake);
        world.entities.insert(sleeping_id, e_sleeping);
        let result = build_nearby_entities_str(&world, &me);
        // Pull out the Characters section and verify the awake
        // line precedes the sleeping one.  Both lines are tagged
        // with the same entity name (`**TestEntity**`); we
        // distinguish them by the 💤 marker.
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let char_section = &result[char_section_start..];
        let pos_awake = char_section
            .find("**TestEntity**")
            .expect("awake line missing");
        let pos_sleeping = char_section
            .rfind("**TestEntity**")
            .expect("sleeping line missing");
        assert!(
            pos_awake < pos_sleeping,
            "awake line should appear before sleeping line (positions: awake={pos_awake}, sleeping={pos_sleeping})"
        );
        // Sleeping row should still carry the 💤 marker.
        assert!(result.contains("💤×0.01"), "sleeping row should keep the 💤×0.01 marker");
    }

    #[test]
    fn nearby_entities_negative_visibility_does_not_crash_sort() {
        // Entity with power=2 and visibility=-10 (sum=-8) used to be
        // at risk of producing a negative sort score.  The
        // `max(1, power + visibility)` floor protects against that.
        // We no longer print the score, but the sort must still
        // produce a valid output and the entity must still appear.
        //
        // Replaces the previous
        // `nearby_entities_score_floors_negative_visibility_at_one`
        // test, which used to parse the score from the printed line.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let mut hidden = make_entity("Hidden", 10050.0, 10050.0);
        hidden.entity_type = "character".to_string();
        hidden.properties_int.insert("power".to_string(), 2);
        hidden.properties_int.insert("visibility".to_string(), -10);
        let me_id = me.id;
        let hidden_id = hidden.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(hidden_id, hidden);
        let result = build_nearby_entities_str(&world, &me);
        // Power stat is still shown in the metadata line.
        assert!(result.contains("**Hidden**"));
        assert!(result.contains("power 2"));
        // The entity is still listed under Characters.
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let char_section = &result[char_section_start..];
        assert!(char_section.contains("**Hidden**"));
        // No negative score substring should have leaked into the
        // output (we don't print score anymore, but a regression
        // could in theory introduce it; this is a sanity check).
        assert!(!result.contains("score -"), "no negative score should appear in the rendered output");
    }

    #[test]
    fn nearby_entities_splits_factions_from_characters() {
        // Faction must appear in the new "Nearby Factions" section,
        // NOT in the "Nearby Characters" catch-all.  A regular
        // character next to it must still appear in the Characters
        // section.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let mut a_faction = make_entity("Ironforge Clan", 10010.0, 10010.0);
        a_faction.entity_type = "faction".to_string();
        a_faction.properties_int.insert("power".to_string(), 200);
        let mut a_char = make_entity("Stranger", 10020.0, 10020.0);
        a_char.entity_type = "character".to_string();
        a_char.properties_int.insert("power".to_string(), 5);
        let me_id = me.id;
        let faction_id = a_faction.id;
        let char_id = a_char.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(faction_id, a_faction);
        world.entities.insert(char_id, a_char);
        let result = build_nearby_entities_str(&world, &me);
        // The new section header must be present.
        assert!(result.contains("### Nearby Factions"));
        // The faction lives in the Factions section, not the Characters one.
        let faction_section_start = result.find("### Nearby Factions").unwrap();
        let faction_section = &result[faction_section_start..];
        assert!(faction_section.contains("**Ironforge Clan**"));
        assert!(!faction_section.contains("**Stranger**"));
        // The character lives in the Characters section as before.
        assert!(result.contains("### Nearby Characters"));
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let char_section = &result[char_section_start..faction_section_start];
        assert!(char_section.contains("**Stranger**"));
        assert!(!char_section.contains("**Ironforge Clan**"));
    }

    #[test]
    fn nearby_entities_factions_sorted_nearest_first_and_capped_at_5() {
        // Place 7 factions around the subject at varying distances.
        // The section should keep only the 5 closest, in ascending
        // distance order, and drop the two farthest.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        world.entities.insert(me_id, me.clone());

        // Distances 10, 20, 30, 40, 50, 200, 300 from me (offsets
        // along the +x axis are fine for Euclidean distance).
        let distances = [10.0_f64, 20.0, 30.0, 40.0, 50.0, 200.0, 300.0];
        let mut faction_ids: Vec<uuid::Uuid> = Vec::new();
        for (i, d) in distances.iter().enumerate() {
            let mut f = make_entity(&format!("Faction{}", i), 10000.0 + d, 10000.0);
            f.entity_type = "faction".to_string();
            f.properties_int.insert("power".to_string(), 100);
            let fid = f.id;
            faction_ids.push(fid);
            world.entities.insert(fid, f);
        }
        let result = build_nearby_entities_str(&world, &me);
        // The two farthest (Faction5 at 200, Faction6 at 300) must be dropped.
        assert!(!result.contains("**Faction5**"), "Faction5 (200u) should be dropped (cap 5)");
        assert!(!result.contains("**Faction6**"), "Faction6 (300u) should be dropped (cap 5)");
        // The five closest must be present.
        for i in 0..5 {
            assert!(
                result.contains(&format!("**Faction{}**", i)),
                "Faction{} ({}u) should be present in the Factions section",
                i, distances[i]
            );
        }
        // Header should mention "5 nearest".
        assert!(result.contains("(5 nearest)"), "header should indicate the cap of 5: {}", result);
        // Nearest-first order: Faction0 (10u) must come before Faction1 (20u),
        // which must come before Faction2 (30u), etc.
        let faction_section_start = result.find("### Nearby Factions").unwrap();
        let faction_section = &result[faction_section_start..];
        let positions: Vec<usize> = (0..5)
            .map(|i| {
                faction_section
                    .find(&format!("**Faction{}**", i))
                    .unwrap_or_else(|| panic!("Faction{} missing from section", i))
            })
            .collect();
        for w in positions.windows(2) {
            assert!(
                w[0] < w[1],
                "Factions should appear in ascending distance order; positions: {:?}",
                positions
            );
        }
    }

    #[test]
    fn nearby_entities_includes_faraway_high_power_entities() {
        // Per Arcurus 2026-06-07 #openworld: the previous 150-unit
        // radius was a hidden limit.  A high-power entity far from
        // the subject should still appear, ranked by the influence
        // score.  Place a legend 500 units away with power=1000;
        // it should surface in the Characters section above any
        // closer low-power bystander.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        // Close but weak.
        let mut weak = make_entity("Bystander", 10010.0, 10010.0);
        weak.entity_type = "character".to_string();
        weak.properties_int.insert("power".to_string(), 1);
        // Far but mighty — 500 units along the x-axis.
        let mut legend = make_entity("FarLegend", 10500.0, 10000.0);
        legend.entity_type = "character".to_string();
        legend.properties_int.insert("power".to_string(), 1000);
        let me_id = me.id;
        let weak_id = weak.id;
        let legend_id = legend.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(weak_id, weak);
        world.entities.insert(legend_id, legend);
        let result = build_nearby_entities_str(&world, &me);
        // Both should appear; the previous radius-based code would
        // have hidden FarLegend entirely.
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let char_section = &result[char_section_start..];
        assert!(char_section.contains("**FarLegend**"),
            "FarLegend (500u away) should appear in the no-radius list: {char_section}");
        assert!(char_section.contains("**Bystander**"),
            "Bystander (14u away) should still appear: {char_section}");
        // FarLegend's score (1000/500 = 2.0) outranks Bystander's
        // (1/14 ≈ 0.071), so the legend sorts first.
        let pos_legend = char_section.find("**FarLegend**").unwrap();
        let pos_bystander = char_section.find("**Bystander**").unwrap();
        assert!(pos_legend < pos_bystander,
            "FarLegend should sort above Bystander (higher power outweighs distance)");
    }

    #[test]
    fn nearby_entities_caps_locations_at_top_5_by_influence() {
        // Per Arcurus 2026-06-07 #openworld: each section is now
        // capped at MAX_NEARBY_LOCATIONS = 5 by influence score,
        // instead of taking every entity within the 150-radius.
        // Place 8 locations; only the top 5 by score should appear.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        world.entities.insert(me_id, me.clone());
        // All 8 locations at the SAME distance (50u) so the score
        // ranking is purely by power.  Powers span 1..=8 so the
        // top 5 are the ones with power >= 4.
        for i in 1..=8 {
            let mut loc = make_entity(&format!("Loc{}", i), 10050.0, 10000.0);
            loc.entity_type = "location".to_string();
            loc.properties_int.insert("power".to_string(), i as i64);
            let lid = loc.id;
            world.entities.insert(lid, loc);
        }
        let result = build_nearby_entities_str(&world, &me);
        let loc_section_start = result.find("### Nearby Locations").unwrap();
        let loc_section = &result[loc_section_start..];
        // Top 5 by score (highest power) must be present.
        for i in 4..=8 {
            assert!(loc_section.contains(&format!("**Loc{}**", i)),
                "Loc{} (power={}) should be in the top 5: {loc_section}", i, i);
        }
        // Bottom 3 (power 1, 2, 3) must be dropped.
        for i in 1..=3 {
            assert!(!loc_section.contains(&format!("**Loc{}**", i)),
                "Loc{} (power={}) should be dropped by the 5-cap: {loc_section}", i, i);
        }
        // Header should mention the cap.
        assert!(loc_section.contains("(top 5 by influence)"),
            "Locations header should mention the top-5 cap: {loc_section}");
    }

    #[test]
    fn nearby_entities_caps_characters_at_top_5_by_influence() {
        // Same shape as the locations test, but for characters.
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        world.entities.insert(me_id, me.clone());
        for i in 1..=8 {
            let mut ch = make_entity(&format!("Char{}", i), 10050.0, 10000.0);
            ch.entity_type = "character".to_string();
            ch.properties_int.insert("power".to_string(), i as i64);
            let cid = ch.id;
            world.entities.insert(cid, ch);
        }
        let result = build_nearby_entities_str(&world, &me);
        let char_section_start = result.find("### Nearby Characters").unwrap();
        let char_section = &result[char_section_start..];
        for i in 4..=8 {
            assert!(char_section.contains(&format!("**Char{}**", i)),
                "Char{} (power={}) should be in the top 5", i, i);
        }
        for i in 1..=3 {
            assert!(!char_section.contains(&format!("**Char{}**", i)),
                "Char{} (power={}) should be dropped by the 5-cap", i, i);
        }
        assert!(char_section.contains("(top 5 by influence)"),
            "Characters header should mention the top-5 cap: {char_section}");
    }

    #[test]
    fn world_events_empty_world_returns_empty_string() {
        // A truly event-less world (we explicitly clear the seed
        // defaults to test the empty-input branch; in production a
        // fresh World::new() now ships with 5 lore events — see
        // World::seed_default_events / todo e4cc4203).
        let mut world = make_world();
        world.active_events.clear();
        let result = build_world_events_str(&world);
        assert_eq!(result, "");
    }

    #[test]
    fn action_context_includes_all_five_blocks() {
        let mut world = make_world();
        let mut entity = make_entity("Hero", 10000.0, 10000.0);
        entity.properties_int.insert("power".to_string(), 100);
        let id = entity.id;
        world.entities.insert(id, entity.clone());
        let ctx = build_action_context(&world, &entity, 500);
        assert!(ctx.prop_context.contains("power: 100"));
        assert!(ctx.power_tier_str.starts_with("Uncommon"));
        assert!(!ctx.nearby_entities_str.is_empty());
        // Default global cap of 500 should be reflected since the test world
        // has no per-world override (defaults to 0 → use global default).
        assert_eq!(ctx.max_history_summary_chars, 500);
    }

    #[test]
    fn action_prompt_replaces_all_placeholders() {
        let entity = make_entity("Aragorn", 10.0, 20.0);
        let ctx = ActionContext {
            prop_context: "props".to_string(),
            entity_history_str: "hist".to_string(),
            nearby_entities_str: "near".to_string(),
            power_tier_str: "tier".to_string(),
            world_events_str: "events".to_string(),
            history_summary_str: "summary".to_string(),
            max_history_summary_chars: 500,
            history_summary_chars_used: 7,
        };
        let template = "{world_name} {entity_name} {entity_type} {description} {tags} {x} {y} {property_context} {power_tier} {entity_history} {nearby_entities} {world_events} {history_summary} {max_history_summary_chars} {history_summary_header}";
        let result = build_action_prompt("Middle Earth", &entity, &ctx, template);
        // 7 chars of summary already used; cap 500 ⇒ 493 free.
        // The header now includes that breakdown.
        assert_eq!(
            result,
            "Middle Earth Aragorn hero A test entity test 10.0 20.0 props tier hist near events summary 500 Current History Summary (cap 500 chars, used 7, 493 free):"
        );
    }

    #[test]
    fn action_prompt_preserves_template_unchanged_when_no_placeholders() {
        let entity = make_entity("X", 1.0, 2.0);
        let ctx = ActionContext::default();
        let template = "static text with no placeholders";
        let result = build_action_prompt("W", &entity, &ctx, template);
        assert_eq!(result, "static text with no placeholders");
    }

    /// Regression test: the LLM prompt template's "respond with this JSON
    /// shape" example must itself be syntactically valid JSON after
    /// unescaping the `{{` / `}}` mustache-style braces, otherwise the
    /// LLM is asked to mirror invalid JSON.
    ///
    /// Background: the template is sent to the LLM with literal `{{` and
    /// `}}` (mustache-style escape for a single `{` / `}` in the
    /// response). The LLM is told to "Respond ONLY with valid JSON" and
    /// shown an example block. The example must describe a parseable JSON
    /// shape (with `{{` → `{` and `}}` → `}`), otherwise the LLM is asked
    /// to mirror broken JSON.
    ///
    /// Found in production: a missing comma between `history_summary` and
    /// `history_summary_replace` made the example block invalid JSON, and
    /// the LLM occasionally produced JS-style `Number(...)` wrappers and
    /// concatenated sibling replace objects as "workarounds" — both of
    /// which serde_json::from_str then rejected (~7/day parse_error
    /// warnings in the LLM log).
    #[test]
    fn entity_action_template_json_example_is_valid_json() {
        // Tests run with CARGO_MANIFEST_DIR set to the crate root.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set (running outside cargo?)");
        let template_path = std::path::Path::new(&manifest_dir)
            .join("ai_templates")
            .join("EntityAction.md");
        let template = std::fs::read_to_string(&template_path)
            .unwrap_or_else(|e| panic!("read {:?}: {}", template_path, e));

        // Render the template with all placeholders filled so any
        // placeholder that ends up inside the JSON example block has
        // concrete values. Use a minimal dummy entity and context.
        // Note: `build_action_prompt` does plain String::replace and
        // does NOT process `{{` / `}}` (mustache escape is intentional —
        // the LLM unescapes them in its response).
        let entity = make_entity("X", 0.0, 0.0);
        let ctx = ActionContext {
            prop_context: String::new(),
            entity_history_str: String::new(),
            nearby_entities_str: String::new(),
            power_tier_str: String::new(),
            world_events_str: String::new(),
            history_summary_str: String::new(),
            max_history_summary_chars: 500,
            history_summary_chars_used: 0,
        };
        let rendered = build_action_prompt("TestWorld", &entity, &ctx, &template);

        // The example block is the first top-level `{{ ... }}` in the
        // rendered template (line `{{` ... line `}}`). The rendered
        // template keeps `{{` and `}}` literal.
        let start_marker = "\"action\":";
        let start = rendered
            .find(start_marker)
            .unwrap_or_else(|| panic!("no '{}' in rendered template", start_marker));
        // Walk back to the opening `{{` on its own line.
        let open_idx = rendered[..start]
            .rfind("\n{{")
            .map(|i| i + 1)
            .unwrap_or_else(|| {
                if rendered.starts_with("{{") {
                    0
                } else {
                    panic!("no opening '{{' for example block")
                }
            });
        // Find the first `\n}}` after the opening (the example ends with
        // `}}` on its own line).
        let after_open = open_idx + 2;
        let close_rel = rendered[after_open..]
            .find("\n}}")
            .unwrap_or_else(|| panic!("no closing '}}' for example block"));
        let close_idx = after_open + close_rel + 1; // position of the '}'
        let example_raw = &rendered[open_idx..=close_idx + 1];

        // Unescape mustache: `{{` → `{` and `}}` → `}` — this is the
        // shape the LLM is being asked to produce. Then normalize the
        // illustrative placeholders so serde_json can parse the shape:
        //   - `change_value` (a bare identifier) → `0` (number)
        //   - `, ...` (placeholder for "more KV pairs" inside an object
        //     value, e.g. `"effects": {"property_name": 0, ...}`)
        //     → `` (empty). Without this strip, the inner object would
        //     end with `, "…"` which is an orphan value with no key.
        //   - remaining `...` (ellipsis used as a value placeholder in
        //     string positions like `"action": "..."`) → `"…"`
        //     (a string sentinel). The strip above must run first so
        //     this replace doesn't match the `, ...` form.
        let example_unescaped = example_raw
            .replace("{{", "{")
            .replace("}}", "}")
            .replace("change_value", "0")
            .replace(", ...", "")
            .replace("...", "\"…\"");

        // Parse it. If the template's example is invalid JSON, fail with
        // both the unescaped shape and the raw literal so the bug is
        // obvious.
        serde_json::from_str::<serde_json::Value>(&example_unescaped).unwrap_or_else(|e| {
            panic!(
                "EntityAction.md JSON example is not valid JSON.\n\
                 Error: {}\n\
                 Shape the LLM is being asked to produce (after unescaping {{{{ / }}}}):\n{}\n\
                 Raw literal in template:\n{}",
                e, example_unescaped, example_raw
            )
        });
    }
}
