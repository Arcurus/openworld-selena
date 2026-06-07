//! Internal / operator-only entity properties.
//!
//! These properties exist on every entity (via `properties_int` /
//! `properties_float` / `properties_string`) but are NEVER shown to
//! the LLM and NEVER writable by `apply_effects_to_target` or the
//! per-property PUT endpoint by an LLM-emit effect. They are
//! bookkeeping state for the operator and the orchestrator
//! (scheduling, history markers, stats, etc.).
//!
//! # The rule
//!
//! Every code path that touches entity properties MUST consult these
//! lists to enforce the "internal = LLM-invisible, effect-writable:
//! no" rule:
//!
//! - The LLM-facing `{property_context}` block in the action prompt
//!   (see `context_builder::build_property_context`) filters out any
//!   property whose name is in the relevant list below.
//! - The LLM-facing nearby-entity block
//!   (see `context_builder::build_nearby_entities_str`) does the same
//!   for OTHER entities' properties.
//! - `apply_effects_to_target` in main.rs REJECTS any LLM-emit effect
//!   whose key matches an internal property (with a warning), so the
//!   LLM cannot accidentally (or maliciously) tamper with these
//!   bookkeeping values.
//! - The per-property PUT endpoint (`/api/entities/:id/properties/int/:key`)
//!   is operator-only and IS allowed to write internal properties, so
//!   the operator can manually override the marker (e.g. reset
//!   `last_processed_other_tick` to 0 to force reprocessing). The PUT
//!   endpoint is the documented escape hatch for ops.
//!
//! # Adding a new internal property
//!
//! Per Arcurus 2026-06-07 (#openworld): "best make a list that we can
//! then update if we add new, so all the code that touches properties
//! knows to ignore them. ... if we add more we just need it ad to the
//! list and not change code again".
//!
//! So the workflow is:
//!
//! 1. Add the property name to the appropriate const slice below
//!    (LLM_INTERNAL_INT_PROPERTIES, LLM_INTERNAL_FLOAT_PROPERTIES, or
//!    LLM_INTERNAL_STRING_PROPERTIES).
//! 2. Add a doc-comment to the entry explaining what the property
//!    does and who writes it.
//! 3. Update `docs/world-mechanics.md` § "Internal Properties" to
//!    keep the docs in sync.
//!
//! No code change to the LLM-facing builders or the effect-applier
//! is needed — they iterate over the const slices.
//!
//! # Why not just one slice (or one function that checks the LLM type)?
//!
//! Each slice is per-property-type (int / float / string) because
//! `WorldEntity` has three separate maps and the call sites typically
//! only touch one type at a time. A single `is_internal(key)` would
//! have to know the type at the call site (the LLM doesn't pass
//! types; only the property name), which would force every call site
//! to look up the type. Splitting by type is simpler and more
//! explicit.

/// Int properties that are operator-only (NOT shown to the LLM, NOT
/// writable by LLM-emit effects).
///
/// Operator can still write via the per-property PUT endpoint (the
/// documented escape hatch).
pub const LLM_INTERNAL_INT_PROPERTIES: &[&str] = &[
    // Per Arcurus 2026-06-07 (#openworld): the "processed up to"
    // marker for the unprocessed-other-actions LLM block.  The LLM
    // never sees this name; the orchestrator advances it via
    // `entity_history::add_to_history` after every LLM call; the
    // operator can override it via the per-property PUT (e.g. set
    // to 0 to force reprocessing of all unprocessed actions).
    "last_processed_other_tick",
];

/// Float properties that are operator-only.  Currently empty;
/// add new entries here as we introduce new bookkeeping floats.
pub const LLM_INTERNAL_FLOAT_PROPERTIES: &[&str] = &[];

/// String properties that are operator-only.  Currently empty;
/// add new entries here as we introduce new bookkeeping strings.
pub const LLM_INTERNAL_STRING_PROPERTIES: &[&str] = &[];

/// True if `name` is in any of the LLM-internal property lists
/// (regardless of type).  Convenience predicate for the effect-
/// applier, which sees a property name (not a type) in each
/// `effects` map entry and needs to know "should I reject this
/// write?" without first resolving the type.
pub fn is_internal_property(name: &str) -> bool {
    LLM_INTERNAL_INT_PROPERTIES.contains(&name)
        || LLM_INTERNAL_FLOAT_PROPERTIES.contains(&name)
        || LLM_INTERNAL_STRING_PROPERTIES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_processed_other_tick_is_internal() {
        assert!(is_internal_property("last_processed_other_tick"));
    }

    #[test]
    fn regular_props_are_not_internal() {
        // Sanity: the LLM-meaningful properties that show up in the
        // entity context block must NOT be flagged as internal.
        for prop in &[
            "morale",
            "power",
            "reputation",
            "knowledge",
            "wealth",
            "visibility",
            "magic_protection",
        ] {
            assert!(
                !is_internal_property(prop),
                "regular prop '{}' should not be flagged as internal",
                prop
            );
        }
    }
}
