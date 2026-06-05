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
    let nearby = world.get_entities_in_radius(entity.x, entity.y, 150.0);
    let nearby: Vec<_> = nearby.iter().filter(|e| e.id != entity.id).collect();
    if nearby.is_empty() {
        return String::from("No other entities nearby.");
    }
    let mut s = String::new();
    for other in &nearby {
        let dist = ((other.x - entity.x).powi(2) + (other.y - entity.y).powi(2)).sqrt();
        s.push_str(&format!(
            "- **{}** ({}) - Distance: {:.1}\n",
            other.name, other.entity_type, dist
        ));
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
        assert!(result.contains("Properties: power: 10"));
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
        };
        let template = "{world_name} {entity_name} {entity_type} {description} {tags} {x} {y} {property_context} {power_tier} {entity_history} {nearby_entities} {world_events} {history_summary} {max_history_summary_chars}";
        let result = build_action_prompt("Middle Earth", &entity, &ctx, template);
        assert_eq!(
            result,
            "Middle Earth Aragorn hero A test entity test 10.0 20.0 props tier hist near events summary 500"
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
