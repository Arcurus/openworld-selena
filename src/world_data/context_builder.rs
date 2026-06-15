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
use crate::world_data::action_history_log::{self, ActionHistoryEntry};

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
    /// Recent world actions from the cross-entity action_history.jsonl
    /// log that the LLM has not yet seen — i.e. entries with
    /// `timestamp > actor.last_action_at` (or all recent entries if
    /// the actor has never acted), filtered to drop the actor's own
    /// most recent action (so the LLM doesn't see "what it's about
    /// to do") and system-entity actions. Goes into
    /// `{recent_world_actions}`.
    /// Per Arcurus 2026-06-07 (#openworld).
    pub recent_world_actions_str: String,
    /// "Unprocessed world actions from other entities that
    /// affected this one" — i.e. history entries with
    /// `entity_id != this` AND the entry's effects mention
    /// this entity by dotted name AND the entry's `tick` is
    /// greater than the entity's
    /// `last_processed_other_tick` field
    /// (or 0 if unset).  Rendered compact, oldest first,
    /// capped at `MAX_UNPROCESSED_OTHER_ACTIONS_CHARS` chars
    /// total.  The LLM uses this to keep its
    /// `history_summary_replace` in sync with what other
    /// entities have done to it.  Per Arcurus 2026-06-07
    /// (#openworld): "for the connection we go for now if
    /// the entity was affected in the entities effect" and
    /// "Whatever fits in the 10 k (the long version) i
    /// think for now no need to mention the effects, just
    /// that they are applied already.  put as many
    /// unprocessed world actions from other entities in
    /// as they fit in the 10k".
    pub unprocessed_other_actions_str: String,
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
        recent_world_actions_str: {
            // Load the cross-entity feed (capped) once, then hand
            // the entries to the pure renderer. Doing the I/O here
            // (not inside build_recent_world_actions_str) keeps the
            // renderer unit-testable without touching the live log
            // file.
            let cutoff = entity.last_action_at;
            let raw = action_history_log::load_recent_world_actions(
                MAX_RECENT_WORLD_ACTIONS,
                cutoff,
            );
            build_recent_world_actions_str(world, entity, &raw)
        },
        unprocessed_other_actions_str: {
            // Per Arcurus 2026-06-07 (#openworld).  The 10K
            // cap is large enough that the file scan is the
            // bottleneck, so we read ALL entries (no per-call
            // cap) and let the renderer cap by char count.
            // This is a one-shot scan of the whole JSONL log,
            // which at 5-10K entries is sub-millisecond.
            //
            // 2026-06-15: the marker moved from
            // `properties_int["last_processed_other_tick"]` to the
            // dedicated `WorldEntity::last_processed_other_tick`
            // field. Read the field directly.
            let last_processed_tick = entity.last_processed_other_tick;
            let raw = action_history_log::load_all_world_actions();
            build_unprocessed_other_actions_str(
                world,
                entity,
                &raw,
                last_processed_tick,
            )
        },
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
///
/// `property_docs` is the rendered text of
/// `ai_templates/property_docs.md` — the LLM-facing reference for
/// named properties (visibility, corruption, etc.).  It is loaded
/// separately by the caller so the template can stay focused on
/// the prompt structure and the docs can be edited without
/// touching the template.
pub fn build_action_prompt(
    world_name: &str,
    entity: &WorldEntity,
    ctx: &ActionContext,
    template: &str,
    property_docs: &str,
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
        .replace("{property_docs}", property_docs)
        .replace("{power_tier}", &ctx.power_tier_str)
        .replace("{entity_history}", &ctx.entity_history_str)
        .replace("{nearby_entities}", &ctx.nearby_entities_str)
        .replace("{world_events}", &ctx.world_events_str)
        .replace("{history_summary}", &ctx.history_summary_str)
        .replace("{max_history_summary_chars}", &ctx.max_history_summary_chars.to_string())
        .replace("{history_summary_header}", &build_history_summary_header(ctx))
        .replace("{recent_world_actions}", &ctx.recent_world_actions_str)
        .replace("{unprocessed_other_actions}", &ctx.unprocessed_other_actions_str)
}

fn build_property_context(entity: &WorldEntity, type_stats: Option<&EntityTypeStats>) -> String {
    let mut prop_context = String::new();
    for (key, value) in &entity.properties_int {
        // Skip operator-internal / LLM-invisible properties
        // (see `internal_properties::LLM_INTERNAL_INT_PROPERTIES`).
        // The LLM must never see the marker, scheduling flags,
        // or other bookkeeping state.
        if crate::world_data::internal_properties::LLM_INTERNAL_INT_PROPERTIES.contains(&key.as_str()) {
            continue;
        }
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
        if crate::world_data::internal_properties::LLM_INTERNAL_FLOAT_PROPERTIES.contains(&key.as_str()) {
            continue;
        }
        prop_context.push_str(&format!("  - {}: {:.2}\n", key, value));
    }
    for (key, value) in &entity.properties_string {
        if crate::world_data::internal_properties::LLM_INTERNAL_STRING_PROPERTIES.contains(&key.as_str()) {
            continue;
        }
        prop_context.push_str(&format!("  - {}: {}\n", key, value));
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
        // Skip operator-internal / LLM-invisible properties
        // for OTHER entities too (the LLM must never see the
        // marker, scheduling flags, or other bookkeeping
        // state on anyone, not just on the actor).
        .filter(|(k, _)| {
            !crate::world_data::internal_properties::LLM_INTERNAL_INT_PROPERTIES
                .contains(&k.as_str())
        })
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

/// Maximum number of recent world actions to surface in the
/// `{recent_world_actions}` block. Tunable. 10 is a sweet spot:
/// enough to give the LLM a clear "what just happened elsewhere"
/// picture (typical turn sees 1-3 actions by other entities), not
/// so many that the prompt bloats.
const MAX_RECENT_WORLD_ACTIONS: usize = 10;

/// Build the "Recent World Actions" block for the LLM context.
///
/// `recent_entries` is the pre-loaded cross-entity feed (most-recent
/// first), already capped at `MAX_RECENT_WORLD_ACTIONS`. The caller
/// (`build_action_context`) does the I/O; this function is pure so
/// tests can pass any list of entries without touching the live
/// `action_history.jsonl` file.
///
/// Filtering rules:
///   1. Drop the actor's own MOST RECENT action (the one that
///      triggered this LLM call — the LLM is *generating* that
///      action, not reacting to it). Other entries by the same
///      actor (e.g. an earlier action in the same turn) ARE
///      surfaced, because the LLM benefits from seeing its own
///      history in the global feed too.
///   2. Drop system-entity actions (World Clock, anything tagged
///      'meta' — bookkeeping noise the LLM doesn't need).
///
/// Per Arcurus 2026-06-07 (#openworld): "add to the world action
/// llm call an insertion of not yet processed world actions".
fn build_recent_world_actions_str(
    world: &World,
    entity: &WorldEntity,
    recent_entries: &[ActionHistoryEntry],
) -> String {
    if recent_entries.is_empty() {
        // No unprocessed actions — most common case (the actor is
        // acting for the first time, or no other entity has acted
        // since its last action). Don't render an empty section.
        return String::new();
    }

    let mut rows: Vec<String> = Vec::with_capacity(recent_entries.len());
    let mut dropped_own_most_recent = false;
    for entry in recent_entries {
        if !dropped_own_most_recent
            && entry.entity_id == entity.id.to_string()
        {
            dropped_own_most_recent = true;
            continue;
        }
        if is_system_entry(world, entry) {
            continue;
        }
        rows.push(format_recent_action_row(entry));
    }
    if rows.is_empty() {
        return String::new();
    }
    let mut s = String::from("## Recent World Actions (since your last action)\n\n");
    for r in rows {
        s.push_str(&r);
    }
    s
}

/// Char cap for the `{unprocessed_other_actions}` block.
/// Per Arcurus 2026-06-07 (#openworld): "Whatever fits in
/// the 10 k (the long version) i think for now no need to
/// mention the effects, just that they are applied
/// already.  put as many unprocessed world actions from
/// other entities in as they fit in the 10k".  We use 9.5K
/// to leave headroom for the block header + the rest of
/// the prompt.
const MAX_UNPROCESSED_OTHER_ACTIONS_CHARS: usize = 9_500;

/// Build the "Unprocessed world actions from other
/// entities" block for the LLM context.
///
/// Filtering rules (per Arcurus 2026-06-07 #openworld):
///   1. The action's actor must NOT be this entity
///      (`entry.entity_id != entity.id`).
///   2. The action must have *affected* this entity, which
///      we detect by checking whether any of the entry's
///      effect keys starts with `"<this entity name>."`
///      (the dotted-name convention used by the LLM when
///      it writes cross-entity effects).  Per Arcurus
///      2026-06-07: "for the connection we go for now if
///      the entity was affected in the entities effect".
///   3. The action must NOT be from a system entity
///      (World Clock etc.) — bookkeeping noise.
///   4. The entry's `tick` must be strictly greater than
///      `entity.last_processed_other_tick`
///      (or 0 if unset).  This is the "processed up to"
///      marker that advances on every LLM call for this
///      entity (see `tick_unprocessed_other_actions` in
///      main.rs) and is operator-settable via direct
///      struct-field assignment.  (2026-06-15: moved out of
///      `properties_int` to a first-class field.)
///
/// Render rules:
///   - Chronological (oldest first) — natural reading
///     order; the LLM sees the cause before the effect.
///   - Compact one-liner per action, no effects listed
///     (per Arcurus: "no need to mention the effects, just
///     that they are applied already").  The LLM doesn't
///     need to re-emit them, just be aware of the
///     relationship change.
///   - Outcome: full text (up to 1000 chars) is shown in
///     each row.  Per Arcurus 2026-06-07 (#openworld):
///     "please dont cut it, or cut it very high at 1000
///     chars or so and log a warning if you do!"  The
///     unprocessed block carries the FULL description of
///     the other entities' events that the LLM needs to
///     process, so the outcome is not truncated for
///     space-saving purposes.  Only as a safety net for
///     unusually long outcomes (> 1000 chars) do we
///     truncate to 1000 + `…`, and we log a warning when
///     we do (server-side, NOT to the LLM).  See
///     `format_unprocessed_other_action_row`.
///   - If the char cap is reached, the OLDEST entries are
///     KEPT and the NEWEST ones are dropped.  Per Arcurus
///     2026-06-07: "we first fill the 10k with the oldest
///     not processed messages, and log a warning if not
///     all fittet in.  if the next does fit in, simply
///     dont put it in, done.  next time it will continue
///     with it."  The dropped rows stay above the
///     per-entity marker, so the next LLM call re-sees
///     them — the LLM works through the backlog
///     chronologically.  One warning is logged per call
///     (server-side, NOT to the LLM).
///   - If even the first row doesn't fit (degenerate
///     case — the cap is 9.5K chars and a typical row
///     is ~280 chars, so 30+ rows fit), the block is
///     omitted from the prompt, an ERROR is logged to
///     the server log, and the per-entity marker does
///     NOT advance (so the operator can investigate
///     without the data slipping past).
///   - If the filtered list is empty, return an empty
///     string (don't render an empty block).
///
/// `all_entries` is the pre-loaded full history log
/// (oldest-first).  The caller (`build_action_context`)
/// does the I/O; this function is pure so tests can pass
/// any list of entries without touching the live
/// `action_history.jsonl` file.
fn build_unprocessed_other_actions_str(
    world: &World,
    entity: &WorldEntity,
    all_entries: &[ActionHistoryEntry],
    last_processed_tick: i64,
) -> String {
    if all_entries.is_empty() {
        return String::new();
    }
    let my_id = entity.id.to_string();
    let my_name_prefix = format!("{}.", entity.name);
    // Filter to the relevant entries.
    let mut matching: Vec<&ActionHistoryEntry> = Vec::new();
    for entry in all_entries {
        if entry.entity_id == my_id {
            continue;  // own action
        }
        if entry.tick <= last_processed_tick {
            continue;  // already processed
        }
        if is_system_entry(world, entry) {
            continue;  // World Clock, meta-tagged, etc.
        }
        // "Was this entity affected?" = at least one
        // effect key starts with "<this entity name>.".
        // Note: a system entity (entity.is_system_entity())
        // has no name in the usual sense and won't match
        // the prefix; we also explicitly skip system
        // entries above.
        let affects_me = entry
            .effects
            .keys()
            .any(|k| k.starts_with(&my_name_prefix));
        if !affects_me {
            continue;
        }
        matching.push(entry);
    }
    if matching.is_empty() {
        return String::new();
    }

    // Build the rows.
    //
    // We sort the matching entries by WALL-CLOCK
    // timestamp ASC (oldest first) so the LLM reads
    // cause-then-effect in chronological order.  Per
    // Arcurus 2026-06-07 (#openworld): "we first fill
    // the 10k with the oldest not processed messages".
    //
    // Why timestamp and not tick: the `tick` field is
    // monotonic for new actions (= world.action_count at
    // commit time) but the backfilled tick values
    // (1..=N assigned in append order during
    // `backfill_ticks`) are not strictly wall-clock
    // monotonic — e.g. a recent action that was written
    // after the tick field was added carries a low tick
    // value (the world.action_count at that moment),
    // while an older action in the backfill might carry
    // a much higher sequential tick.  So tick-order can
    // produce a non-monotonic jump in wall-clock time.
    // The wall-clock timestamp in each row is the
    // authoritative chronology; the tick is the
    // filter/marker key (because it's the durable
    // monotonic counter).
    let mut sorted_matching: Vec<&ActionHistoryEntry> = matching;
    sorted_matching.sort_by_key(|e| e.timestamp);

    let mut rows: Vec<(i64, String)> = Vec::with_capacity(sorted_matching.len());
    for entry in &sorted_matching {
        rows.push((entry.tick, format_unprocessed_other_action_row(entry)));
    }

    // Fill oldest-first until the cap is hit.  Per Arcurus
    // 2026-06-07 (#openworld): "we first fill the 10k with
    // the oldest not processed messages, and log a warning
    // if not all fitted in.  if the next does fit in,
    // simply dont put it in, done.  next time it will
    // continue with it."
    //
    // The dropped rows (the ones that didn't fit) are the
    // NEWEST ones.  They stay above the marker, so the
    // next LLM call will re-see them — the LLM works
    // through the backlog chronologically.
    let mut s = String::from(
        "## Unprocessed world actions from other entities\n\n\
         These are recent world actions from other entities that have affected you.\n\
         Their effects are already reflected in your current state. You don't need to re-emit these effects in your response.\n\n\
         You should mention their impact and any change of relations in your `history_summary_replace`, AND you should also include the action you just emitted itself, so your narrative memory stays in sync with what other entities have been doing and with what you just did.\n\n",
    );
    let header_len = s.len();
    let mut total_chars = header_len;
    let mut rows_kept: usize = 0;
    for (i, (_, row)) in rows.iter().enumerate() {
        if total_chars + row.len() > MAX_UNPROCESSED_OTHER_ACTIONS_CHARS {
            // This row would overflow.  Stop.  Per
            // Arcurus 2026-06-07: the dropped row stays
            // in the queue and gets re-shown next call.
            break;
        }
        total_chars += row.len();
        rows_kept = i + 1;
    }

    if rows_kept == 0 {
        // Cap is so tight that even the first row
        // doesn't fit.  This is a degenerate case (the
        // cap is 9.5K chars, a typical row is ~280
        // chars, so 30+ rows fit).  Per Arcurus
        // 2026-06-07: log an error to the server log
        // (NOT to the LLM prompt), do NOT add the block
        // to the prompt, and do NOT advance the marker
        // — so the operator can investigate and bump
        // the cap or trim the row.
        eprintln!(
            "[unprocessed-other-actions] cap too tight: even the first row didn't fit for entity '{}' (cap {} chars, first row {} chars, total matching entries {}). Not advancing the marker; please inspect.",
            entity.name,
            MAX_UNPROCESSED_OTHER_ACTIONS_CHARS,
            rows[0].1.len(),
            rows.len()
        );
        return String::new();
    }

    if rows_kept < rows.len() {
        // Some entries were dropped (they were the
        // newest unprocessed ones).  Log a single
        // warning per call.  Per Arcurus 2026-06-07:
        // "log a warning if not all fittet in".
        let oldest_kept_tick = rows[rows_kept - 1].0;
        let newest_dropped_tick = rows[rows.len() - 1].0;
        let dropped_count = rows.len() - rows_kept;
        eprintln!(
            "[unprocessed-other-actions] dropped {} newest entries for entity '{}' (oldest kept tick {}, newest dropped tick {}, cap {} chars, total matching entries {}). Next call will continue with the dropped ones.",
            dropped_count,
            entity.name,
            oldest_kept_tick,
            newest_dropped_tick,
            MAX_UNPROCESSED_OTHER_ACTIONS_CHARS,
            rows.len()
        );
    }

    for (_, row) in &rows[..rows_kept] {
        s.push_str(row);
    }
    s
}

/// Compute the max tick of history entries that would be
/// rendered in the unprocessed-other-actions block for
/// this entity.  This is the tick value the per-entity
/// `last_processed_other_tick` marker should advance TO
/// after the LLM call, per Arcurus 2026-06-07 (#openworld):
/// "it needs to be set to the creating tick time of the
/// other history message last included in the llm to
/// process."
///
/// Note: the FILTER is tick-based (so the marker
/// advancement is well-defined and monotonic), but the
/// RENDERER sorts by wall-clock timestamp.  So the
/// "entries that would be rendered" are the same set
/// filtered by tick, but ordered by timestamp; the max
/// tick is the same either way.
///
/// Same filtering rules as
/// `build_unprocessed_other_actions_str` (drop own
/// actions, drop system entities, drop entries at or
/// below the current marker, drop entries where no
/// effect key starts with `"<this entity name>."`).
/// Returns 0 if no entries match — callers should use
/// `max(current_marker, returned_value)` so the marker
/// never regresses.
///
/// Why a separate function (rather than returning the
/// value from `build_unprocessed_other_actions_str`):
/// the prompt-builder and the process-action handler are
/// separate API calls with a time gap.  The prompt-builder
/// shows the block; the process-action handler computes
/// the max tick AGAIN (cheap; the JSONL is small) so it
/// can advance the marker without needing the
/// prompt-builder to round-trip the value through the
/// API.
///
/// Pure: no I/O.  Tests can pass any list of entries
/// without touching the live `action_history.jsonl` file.
pub fn compute_max_unprocessed_tick(
    world: &World,
    entity: &WorldEntity,
    all_entries: &[ActionHistoryEntry],
    last_processed_tick: i64,
) -> i64 {
    if all_entries.is_empty() {
        return 0;
    }
    let my_id = entity.id.to_string();
    let my_name_prefix = format!("{}.", entity.name);
    let mut max_tick: i64 = 0;
    for entry in all_entries {
        if entry.entity_id == my_id {
            continue;  // own action
        }
        if entry.tick <= last_processed_tick {
            continue;  // already processed
        }
        if is_system_entry(world, entry) {
            continue;  // World Clock, meta-tagged, etc.
        }
        let affects_me = entry
            .effects
            .keys()
            .any(|k| k.starts_with(&my_name_prefix));
        if !affects_me {
            continue;
        }
        if entry.tick > max_tick {
            max_tick = entry.tick;
        }
    }
    max_tick
}

/// Outcome truncation cap for the unprocessed-other-actions
/// block.  Per Arcurus 2026-06-07 (#openworld): "please dont
/// cut it, or cut it very high at 1000 chars or so and log a
/// warning if you do!" — the unprocessed block carries the
/// full description of the other entities' events that the
/// LLM needs to process, so it should NOT truncate just to
/// save space.  We cut only as a safety net for unusually
/// long outcomes (> 1000 chars), and we log a warning when
/// we do.
///
/// Note: the entity's OWN history block
/// (`{entity_history}`, rendered by `format_history_for_llm`)
/// always shows the FULL outcome (no truncation) — only the
/// "short" mode there drops the `details` field, not the
/// outcome.  The truncation here is a per-row safety net for
/// the unprocessed block.
const UNPROCESSED_OUTCOME_MAX_CHARS: usize = 1_000;

/// Format one unprocessed-other-action row for the LLM
/// context.  One row per action:
///
/// `- [YYYY-MM-DD HH:MM] **EntityName**: \`action_name\` — <full outcome> (effects applied)`
///
/// Per Arcurus 2026-06-07 (#openworld): "we added the full
/// description of the other event" — so we keep the FULL
/// outcome (no truncation) for outcomes up to
/// `UNPROCESSED_OUTCOME_MAX_CHARS = 1000` chars.  Above
/// that we truncate to 1000 + `…` and log a warning.
///
/// Also per Arcurus 2026-06-07: "no need to mention the
/// effects, just that they are applied already" — we
/// append a single "(effects applied)" tag so the LLM knows
/// the state has already been updated.
fn format_unprocessed_other_action_row(entry: &ActionHistoryEntry) -> String {
    let ts = entry.timestamp.format("%Y-%m-%d %H:%M");
    let outcome_full = entry.outcome.replace('\n', " ");
    let outcome_chars = outcome_full.chars().count();
    if outcome_chars > UNPROCESSED_OUTCOME_MAX_CHARS {
        // Safety-net truncation: outcome is unusually
        // long.  Per Arcurus 2026-06-07, log a warning
        // so the operator knows which entry was cut.
        // This is rare in practice (only 1 of 4000+
        // entries on the live world has outcome > 1000
        // chars; the typical LLM-emit outcome is 200-
        // 500 chars).
        eprintln!(
            "[unprocessed-other-actions] truncated outcome for {} `{}` (tick {}): original {} chars, cut to {}",
            entry.entity_name,
            entry.action,
            entry.tick,
            outcome_chars,
            UNPROCESSED_OUTCOME_MAX_CHARS
        );
    }
    let outcome_one_line: String = outcome_full
        .chars()
        .take(UNPROCESSED_OUTCOME_MAX_CHARS)
        .collect();
    let outcome_suffix = if outcome_chars > UNPROCESSED_OUTCOME_MAX_CHARS {
        "…"
    } else {
        ""
    };
    if outcome_one_line.is_empty() {
        format!(
            "- [{}] **{}**: `{}` (effects applied)\n",
            ts, entry.entity_name, entry.action
        )
    } else {
        format!(
            "- [{}] **{}**: `{}` — {}{} (effects applied)\n",
            ts,
            entry.entity_name,
            entry.action,
            outcome_one_line,
            outcome_suffix
        )
    }
}

/// True if the action_history entry was performed by a system entity
/// (World Clock or anything tagged 'meta'). These are bookkeeping
/// actions, not narrative events the LLM needs in the recent-feed
/// block.  Resolved by matching the entry's entity_id against the
/// world's live entity list, falling back to the entity_name when
/// the id is missing (shouldn't happen, but be defensive).
fn is_system_entry(world: &World, entry: &ActionHistoryEntry) -> bool {
    // Fast path: match by id.
    if let Some(e) = world.entities.values().find(|e| e.id.to_string() == entry.entity_id) {
        return e.is_system_entity();
    }
    // Fallback: match by exact name (case-sensitive).
    if let Some(e) = world.entities.values().find(|e| e.name == entry.entity_name) {
        return e.is_system_entity();
    }
    // Unknown entity — assume non-system so we don't silently drop
    // legitimate narrative actions just because the world forgot
    // the actor (e.g. after a load + entity prune).
    false
}

/// Format one recent-action row for the LLM context. Compact
/// one-liner: `- [HH:MM] EntityName: action_name — outcome`
///
/// We truncate the outcome to ~200 chars to keep the section
/// bounded; the full narrative is in the entity's own history
/// block (the actor's history is included in `{entity_history}`
/// anyway, and a peek into other entities' most recent outcomes
/// is what we want here, not their full narrative).
fn format_recent_action_row(entry: &ActionHistoryEntry) -> String {
    let ts = entry.timestamp.format("%Y-%m-%d %H:%M");
    let outcome_one_line = entry
        .outcome
        .replace('\n', " ")
        .chars()
        .take(200)
        .collect::<String>();
    let outcome_suffix = if entry.outcome.chars().count() > 200 {
        "…"
    } else {
        ""
    };
    format!(
        "- [{}] **{}**: `{}` — {}{}\n",
        ts, entry.entity_name, entry.action, outcome_one_line, outcome_suffix
    )
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
    fn property_context_filters_out_internal_properties() {
        // Per Arcurus 2026-06-07 (#openworld): the marker
        // (and any other internal bookkeeping properties)
        // must NOT appear in the entity's own property
        // context block.  The LLM must never see the
        // marker.
        //
        // 2026-06-15: the marker moved to a first-class
        // `WorldEntity::last_processed_other_tick` field
        // (no longer in `properties_int`).  The defense
        // is now architectural: the property context is
        // built from `properties_int` only, so a name
        // that's not in `properties_int` can't appear in
        // the LLM context regardless of the internal-
        // property list.  This test pins the new
        // mechanism: the struct field is NEVER rendered.
        let mut entity = make_entity("Test", 10000.0, 10000.0);
        entity.properties_int.insert("power".to_string(), 50);
        entity.last_processed_other_tick = 12345;
        let result = build_property_context(&entity, None);
        assert!(result.contains("power: 50"));
        assert!(
            !result.contains("last_processed_other_tick"),
            "marker field must not appear in LLM context (architectural, not filter-based)"
        );
        assert!(
            !result.contains("12345"),
            "marker value must not appear in LLM context"
        );
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
            recent_world_actions_str: "recent".to_string(),
            unprocessed_other_actions_str: "unprocessed".to_string(),
        };
        let template = "{world_name} {entity_name} {entity_type} {description} {tags} {x} {y} {property_context} {property_docs} {power_tier} {entity_history} {nearby_entities} {world_events} {history_summary} {max_history_summary_chars} {history_summary_header}";
        let result = build_action_prompt("Middle Earth", &entity, &ctx, template, "DOCS");
        // 7 chars of summary already used; cap 500 ⇒ 493 free.
        // The header now includes that breakdown.
        assert_eq!(
            result,
            "Middle Earth Aragorn hero A test entity test 10.0 20.0 props DOCS tier hist near events summary 500 Current History Summary (cap 500 chars, used 7, 493 free):"
        );
    }

    #[test]
    fn action_prompt_preserves_template_unchanged_when_no_placeholders() {
        let entity = make_entity("X", 1.0, 2.0);
        let ctx = ActionContext::default();
        let template = "static text with no placeholders";
        let result = build_action_prompt("W", &entity, &ctx, template, "");
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
            recent_world_actions_str: String::new(),
            unprocessed_other_actions_str: String::new(),
        };
        let rendered = build_action_prompt("TestWorld", &entity, &ctx, &template, "");

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

    // -- build_recent_world_actions_str tests ------------------------
    // Per Arcurus 2026-06-07 (#openworld): the world-action LLM
    // prompt must include a feed of recent cross-entity actions
    // that the actor has not yet seen.  This is the pure renderer
    // (it takes pre-loaded entries, so tests don't touch the live
    // `action_history.jsonl` log).
    use chrono::{TimeZone, Utc};

    fn entry_at(
        entity_id: &str,
        entity_name: &str,
        action: &str,
        outcome: &str,
        ts_secs: i64,
    ) -> ActionHistoryEntry {
        ActionHistoryEntry {
            entity_id: entity_id.to_string(),
            entity_name: entity_name.to_string(),
            timestamp: Utc.timestamp_opt(ts_secs, 0).unwrap(),
            action: action.to_string(),
            outcome: outcome.to_string(),
            details: String::new(),
            effects: serde_json::Map::new(),
            warnings: Vec::new(),
            tick: 0,
        }
    }

    #[test]
    fn recent_world_actions_empty_when_no_entries() {
        let world = make_world();
        let entity = make_entity("Lone", 50000.0, 50000.0);
        let result = build_recent_world_actions_str(&world, &entity, &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn recent_world_actions_renders_recent_entries() {
        let mut world = make_world();
        let mut me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        let mut other = make_entity("Other", 10100.0, 10100.0);
        other.entity_type = "character".to_string();
        let other_id = other.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(other_id, other.clone());

        // Feed (most-recent first):
        //   [0] Other do_x at 3000   (newest, surfaces)
        //   [1] Me    do_y at 2000   (newest Me — gets dropped as
        //                            "the action the LLM is generating")
        //   [2] Me    do_z at 1000   (older Me — MUST still surface)
        let raw = vec![
            entry_at(&other_id.to_string(), "Other", "do_x", "x outcome", 3000),
            entry_at(&me_id.to_string(), "Me", "do_y", "y outcome", 2000),
            entry_at(&me_id.to_string(), "Me", "do_z", "z outcome", 1000),
        ];
        let result = build_recent_world_actions_str(&world, &me, &raw);
        assert!(result.starts_with("## Recent World Actions"));
        // Other's action is rendered
        assert!(result.contains("**Other**"), "got: {}", result);
        assert!(result.contains("`do_x`"), "got: {}", result);
        // The newest Me entry (do_y) is the one being generated, so
        // it must NOT appear.
        assert!(!result.contains("`do_y`"), "newest Me entry leaked: {}", result);
        // The older Me entry (do_z) MUST still surface — we only
        // drop the most-recent Me entry, not all of them.
        assert!(result.contains("**Me**"), "got: {}", result);
        assert!(result.contains("`do_z`"), "older Me entry missing: {}", result);
    }

    #[test]
    fn recent_world_actions_drops_actors_own_most_recent() {
        // If the most-recent entry in the feed is the actor's own
        // action, it must be filtered out (so the LLM doesn't see
        // "what it's about to do" — it IS generating that action).
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        world.entities.insert(me_id, me.clone());

        let raw = vec![
            entry_at(&me_id.to_string(), "Me", "self_act", "self outcome", 5000),
            entry_at(&me_id.to_string(), "Me", "old_self_act", "old outcome", 2000),
        ];
        let result = build_recent_world_actions_str(&world, &me, &raw);
        // The most-recent (self_act) is dropped
        assert!(!result.contains("`self_act`"), "got: {}", result);
        // The older one stays
        assert!(result.contains("`old_self_act`"), "got: {}", result);
    }

    #[test]
    fn recent_world_actions_drops_system_entities() {
        // Build a world with the World Clock (system entity) and a
        // normal actor.  The clock's actions must be filtered out.
        let mut world = make_world();
        // Use a non-actor entity as the clock target.
        let clock_id = uuid::Uuid::nil();
        let me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        world.entities.insert(me_id, me.clone());

        let raw = vec![
            // Most-recent: clock action (system) — should be dropped
            entry_at(
                "00000000-0000-0000-0000-000000000001",
                "World Clock",
                "tick",
                "time passes",
                9000,
            ),
            // Next: Me's own most-recent — should also be dropped
            entry_at(&me_id.to_string(), "Me", "self_act", "self outcome", 8000),
            // Older: a normal character's action — should surface
            entry_at(
                "11111111-1111-1111-1111-111111111111",
                "Normal NPC",
                "patrol",
                "walked the walls",
                7000,
            ),
        ];
        let result = build_recent_world_actions_str(&world, &me, &raw);
        // The clock's tick is dropped
        assert!(!result.contains("World Clock"), "got: {}", result);
        assert!(!result.contains("`tick`"), "got: {}", result);
        // The Me self action is dropped
        assert!(!result.contains("`self_act`"), "got: {}", result);
        // The normal NPC's action surfaces
        assert!(result.contains("**Normal NPC**"), "got: {}", result);
        assert!(result.contains("`patrol`"), "got: {}", result);
    }

    #[test]
    fn recent_world_actions_returns_empty_if_all_filtered() {
        // If every entry is the actor's most-recent (i.e. one entry
        // by the actor, nothing else) the section must be omitted
        // entirely (no header alone).
        let mut world = make_world();
        let me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        world.entities.insert(me_id, me.clone());

        let raw = vec![entry_at(
            &me_id.to_string(),
            "Me",
            "only_act",
            "only outcome",
            5000,
        )];
        let result = build_recent_world_actions_str(&world, &me, &raw);
        assert!(result.is_empty(), "expected empty, got: {}", result);
    }

    #[test]
    fn recent_world_actions_truncates_long_outcomes() {
        let mut world = make_world();
        let mut other = make_entity("Bard", 10100.0, 10100.0);
        other.entity_type = "character".to_string();
        let other_id = other.id;
        let me = make_entity("Me", 10000.0, 10000.0);
        let me_id = me.id;
        world.entities.insert(me_id, me.clone());
        world.entities.insert(other_id, other.clone());

        // 500-char outcome — must be truncated to ~200 + ellipsis.
        let long_outcome: String = "x".repeat(500);
        let raw = vec![entry_at(
            &other_id.to_string(),
            "Bard",
            "compose",
            &long_outcome,
            3000,
        )];
        let result = build_recent_world_actions_str(&world, &me, &raw);
        assert!(result.contains("…"), "expected ellipsis on long outcome");
        // Should NOT contain all 500 xs
        assert!(!result.contains(&"x".repeat(300)), "outcome not truncated");
    }

    // ========================================================================
    // build_unprocessed_other_actions_str tests
    // ========================================================================

    /// Build a fully-formed `World` with two non-system entities
    /// ("Me" and "Bard") for the unprocessed-block tests.  The
    /// "Me" entity is the "this entity" the LLM call is being
    /// made for; "Bard" is the "other entity" whose actions may
    /// or may not affect "Me".
    fn make_world_two_non_system_entities() -> (World, Uuid, Uuid) {
        let mut world = make_world();
        let mut bard = make_entity("Bard", 10100.0, 10100.0);
        bard.entity_type = "character".to_string();
        let bard_id = bard.id;
        let mut me = make_entity("Me", 10000.0, 10000.0);
        me.entity_type = "character".to_string();
        let me_id = me.id;
        world.entities.insert(me_id, me);
        world.entities.insert(bard_id, bard);
        (world, me_id, bard_id)
    }

    /// Build an entry with a custom effect map.  Helper for
    /// the unprocessed-block tests below.
    fn entry_with_effects(
        entity_id: &str,
        entity_name: &str,
        action: &str,
        outcome: &str,
        ts_secs: i64,
        tick: i64,
        effects: serde_json::Map<String, serde_json::Value>,
    ) -> ActionHistoryEntry {
        ActionHistoryEntry {
            entity_id: entity_id.to_string(),
            entity_name: entity_name.to_string(),
            timestamp: Utc.timestamp_opt(ts_secs, 0).unwrap(),
            action: action.to_string(),
            outcome: outcome.to_string(),
            details: String::new(),
            effects,
            warnings: Vec::new(),
            tick,
        }
    }

    #[test]
    fn unprocessed_empty_when_no_entries() {
        let (world, me_id, _) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let result = build_unprocessed_other_actions_str(&world, me, &[], 0);
        assert!(result.is_empty(), "expected empty, got: {}", result);
    }

    #[test]
    fn unprocessed_empty_when_no_other_actions_affect_me() {
        // Bard acts, but Bard's effects do NOT mention "Me".
        // → unprocessed list is empty.
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut effects = serde_json::Map::new();
        effects.insert("Bard.composure".to_string(), serde_json::json!(1));
        let raw = vec![entry_with_effects(
            &bard_id.to_string(),
            "Bard",
            "compose",
            "Bard composes a song",
            3000,
            100,
            effects,
        )];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        assert!(result.is_empty(), "expected empty, got: {}", result);
    }

    #[test]
    fn unprocessed_includes_other_action_with_my_name_in_effects() {
        // Bard attacks "Me" — the effect key "Me.health" starts
        // with "Me.", so this action is "unprocessed for Me".
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut effects = serde_json::Map::new();
        effects.insert("Bard.composure".to_string(), serde_json::json!(1));
        effects.insert("Me.health".to_string(), serde_json::json!(-10));
        let raw = vec![entry_with_effects(
            &bard_id.to_string(),
            "Bard",
            "attack",
            "Bard attacks Me",
            3000,
            100,
            effects,
        )];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        assert!(!result.is_empty(), "expected non-empty, got: {}", result);
        assert!(result.contains("Bard"), "should include actor name");
        assert!(result.contains("attack"), "should include action name");
        assert!(
            result.contains("effects applied"),
            "should include 'effects applied' tag"
        );
    }

    #[test]
    fn unprocessed_skips_own_actions() {
        // "Me" acts on themselves — must be skipped even if the
        // effect key starts with "Me.".
        let (world, me_id, _) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut effects = serde_json::Map::new();
        effects.insert("Me.morale".to_string(), serde_json::json!(5));
        let raw = vec![entry_with_effects(
            &me_id.to_string(),
            "Me",
            "rally",
            "Me rallies themselves",
            3000,
            100,
            effects,
        )];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        assert!(result.is_empty(), "own actions must be skipped, got: {}", result);
    }

    #[test]
    fn unprocessed_skips_entries_at_or_below_marker() {
        // Two actions by Bard affecting Me: tick 99 (already
        // processed) and tick 100 (new).  Marker is 99, so only
        // tick 100 is unprocessed.
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut effects_a = serde_json::Map::new();
        effects_a.insert("Me.health".to_string(), serde_json::json!(-5));
        let mut effects_b = serde_json::Map::new();
        effects_b.insert("Me.health".to_string(), serde_json::json!(-7));
        let raw = vec![
            entry_with_effects(
                &bard_id.to_string(),
                "Bard",
                "stab",
                "Bard stabs Me (old)",
                3000,
                99,
                effects_a,
            ),
            entry_with_effects(
                &bard_id.to_string(),
                "Bard",
                "slash",
                "Bard slashes Me (new)",
                3001,
                100,
                effects_b,
            ),
        ];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 99);
        assert!(!result.contains("(old)"), "marker should filter out tick=99");
        assert!(result.contains("(new)"), "tick=100 should be present");
    }

    #[test]
    fn unprocessed_handles_missing_marker() {
        // Marker is 0 (the property doesn't exist yet).  Both
        // entries (tick 1 and 2) should be unprocessed.
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut effects = serde_json::Map::new();
        effects.insert("Me.health".to_string(), serde_json::json!(-5));
        let raw = vec![entry_with_effects(
            &bard_id.to_string(),
            "Bard",
            "attack",
            "Bard attacks Me",
            3000,
            1,
            effects.clone(),
        )];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        assert!(!result.is_empty());
    }

    #[test]
    fn unprocessed_skips_system_entities() {
        // World Clock is a system entity — its actions should be
        // skipped.  (We mark it via is_system_entity; the
        // simplest way is to set entity_type="abstract".)
        let mut world = make_world();
        let mut clock = make_entity("WorldClock", 0.0, 0.0);
        clock.entity_type = "abstract".to_string();
        let clock_id = clock.id;
        let mut me = make_entity("Me", 10000.0, 10000.0);
        me.entity_type = "character".to_string();
        let me_id = me.id;
        world.entities.insert(me_id, me);
        world.entities.insert(clock_id, clock);

        let me = world.entities.get(&me_id).unwrap();
        let mut effects = serde_json::Map::new();
        effects.insert("Me.tick".to_string(), serde_json::json!(1));
        let raw = vec![entry_with_effects(
            &clock_id.to_string(),
            "WorldClock",
            "tick",
            "WorldClock advances time",
            3000,
            100,
            effects,
        )];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        assert!(
            result.is_empty(),
            "system-entity actions must be skipped, got: {}",
            result
        );
    }

    #[test]
    fn unprocessed_caps_at_9500_chars_oldest_kept() {
        // Per Arcurus 2026-06-07 (#openworld): "we first
        // fill the 10k with the oldest not processed
        // messages, and log a warning if not all fittet
        // in."  So when the cap is hit, the OLDEST
        // entries are KEPT and the NEWEST ones are
        // DROPPED.  This is the opposite of the old
        // sliding-window behavior.
        //
        // Generate 200 fake entries (i=0..200) with
        // tick=i+1 (so the first entry has tick 1, the
        // last has tick 200).  All entries affect "Me".
        // Each row is ~400 chars (300-char outcome +
        // prefix).  With cap 9.5K and ~500-char header,
        // we can fit ~22 rows.  So the OLDEST 22 entries
        // (tick 1..=22) should be kept and the rest
        // (tick 23..=200) should be dropped.
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut raw: Vec<ActionHistoryEntry> = Vec::new();
        for i in 0..200 {
            let mut effects = serde_json::Map::new();
            effects.insert("Me.health".to_string(), serde_json::json!(-1));
            let long_outcome: String = "y".repeat(300);
            raw.push(entry_with_effects(
                &bard_id.to_string(),
                "Bard",
                "attack",
                &long_outcome,
                3000 + i,
                i as i64 + 1,
                effects,
            ));
        }
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        // Cap respected.
        assert!(
            result.len() <= MAX_UNPROCESSED_OTHER_ACTIONS_CHARS,
            "result {} chars exceeds cap of {}",
            result.len(),
            MAX_UNPROCESSED_OTHER_ACTIONS_CHARS
        );
        // The result contains the rows.
        assert!(
            result.contains("attack"),
            "result should contain at least one row"
        );
        // Note: we don't assert the EXACT count of kept
        // rows (depends on row width + header width),
        // but the cap-respected assertion above
        // guarantees we're not wildly over.
    }

    #[test]
    fn unprocessed_keeps_oldest_drops_newest_on_overflow() {
        // Sharper test of the oldest-first policy:
        // generate 100 rows of ~250 chars each (forces
        // overflow; cap is 9.5K so only ~30 rows fit)
        // and verify the OLDEST ones are present in
        // the result and the NEWEST ones are not.
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut raw: Vec<ActionHistoryEntry> = Vec::new();
        for i in 0..100 {
            let mut effects = serde_json::Map::new();
            effects.insert("Me.health".to_string(), serde_json::json!(-1));
            let outcome = format!("OUTCOME_{:04}_{}", i, "x".repeat(180));
            raw.push(entry_with_effects(
                &bard_id.to_string(),
                "Bard",
                &format!("action_{:04}", i),
                &outcome,
                3000 + i,
                i as i64 + 1,
                effects,
            ));
        }
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        // The OLDEST outcome ("OUTCOME_0000") should
        // be present (oldest is kept first).
        assert!(
            result.contains("OUTCOME_0000"),
            "oldest outcome should be present (got: first 200 chars of result: {})",
            &result[..200.min(result.len())]
        );
        // The NEWEST outcome ("OUTCOME_0099") should
        // NOT be present (newest is dropped first when
        // the cap is hit).
        assert!(
            !result.contains("OUTCOME_0099"),
            "newest outcome should be dropped when cap is hit (got: tail 200 chars of result: {})",
            &result[result.len().saturating_sub(200)..]
        );
    }

    #[test]
    fn unprocessed_marker_unchanged_when_nothing_shown() {
        // When no entries match the filter, the block
        // is empty AND (by design, not by the renderer
        // itself) the per-entity marker should not
        // advance.  We don't test the marker here
        // (that's entity_history's concern) but we DO
        // test that the renderer returns empty so the
        // caller knows nothing was shown.
        let (world, me_id, _) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        // No entries that affect "Me".
        let raw: Vec<ActionHistoryEntry> = vec![];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn compute_max_unprocessed_tick_returns_max_of_filtered() {
        // Marker computation: returns the max tick of
        // entries that pass the filter.
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut raw: Vec<ActionHistoryEntry> = Vec::new();
        for i in 0..10 {
            let mut effects = serde_json::Map::new();
            effects.insert("Me.health".to_string(), serde_json::json!(-1));
            raw.push(entry_with_effects(
                &bard_id.to_string(),
                "Bard",
                "attack",
                "o",
                3000 + i,
                i as i64 + 1,  // ticks 1..=10
                effects,
            ));
        }
        // With marker=0, all 10 entries match, max tick = 10.
        let max = compute_max_unprocessed_tick(&world, me, &raw, 0);
        assert_eq!(max, 10);
        // With marker=5, only entries with tick > 5
        // (i.e. ticks 6..=10) match, max tick = 10.
        let max = compute_max_unprocessed_tick(&world, me, &raw, 5);
        assert_eq!(max, 10);
        // With marker=10, no entries match (tick > 10
        // is the filter), max tick = 0 (default).
        let max = compute_max_unprocessed_tick(&world, me, &raw, 10);
        assert_eq!(max, 0);
    }

    #[test]
    fn unprocessed_outcome_full_when_under_1000_chars() {
        // Per Arcurus 2026-06-07 (#openworld): "please
        // dont cut it, or cut it very high at 1000 chars
        // or so".  An outcome of 800 chars should appear
        // in full in the row (no truncation).
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut effects = serde_json::Map::new();
        effects.insert("Me.health".to_string(), serde_json::json!(-1));
        let long_outcome = "a".repeat(800);
        let raw = vec![entry_with_effects(
            &bard_id.to_string(),
            "Bard",
            "attack",
            &long_outcome,
            3000,
            1,
            effects,
        )];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        // The full 800 'a' chars should be present (no
        // truncation under 1000).
        assert!(result.contains(&"a".repeat(800)));
        // And the row should NOT end with ellipsis
        // (only added when truncated).
        let row = result.lines().find(|l| l.contains("Bard")).unwrap();
        assert!(!row.contains("…"), "row should not be truncated (got: {})", row);
    }

    #[test]
    fn unprocessed_outcome_safety_net_truncates_above_1000_chars() {
        // Per Arcurus 2026-06-07: "cut it very high at
        // 1000 chars or so and log a warning if you
        // do!".  An outcome > 1000 chars is truncated to
        // 1000 + ellipsis, and the function would log a
        // warning (we just check the row content here,
        // not the eprintln output).
        let (world, me_id, bard_id) = make_world_two_non_system_entities();
        let me = world.entities.get(&me_id).unwrap();
        let mut effects = serde_json::Map::new();
        effects.insert("Me.health".to_string(), serde_json::json!(-1));
        let too_long_outcome = "b".repeat(1500);
        let raw = vec![entry_with_effects(
            &bard_id.to_string(),
            "Bard",
            "attack",
            &too_long_outcome,
            3000,
            1,
            effects,
        )];
        let result = build_unprocessed_other_actions_str(&world, me, &raw, 0);
        // The row should end with ellipsis (truncated).
        let row = result.lines().find(|l| l.contains("Bard")).unwrap();
        assert!(row.contains("…"), "row should be truncated (got: {}...)", &row[..row.len().min(200)]);
        // The 1500 b's should NOT all be present (truncated).
        assert!(!result.contains(&"b".repeat(1500)));
        // But 1000 b's SHOULD be present (cut to 1000).
        assert!(result.contains(&"b".repeat(1000)));
    }
}
