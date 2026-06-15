use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
use chrono::{DateTime, Utc};

use crate::world_data::time_system::EntityTimePreferences;

/// Represents a single property value that can be int, float, or string
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Int(i64),
    Float(f64),
    String(String),
}

impl PropertyValue {
    /// Convert to numeric value for calculations
    pub fn as_f64(&self) -> f64 {
        match self {
            PropertyValue::Int(i) => *i as f64,
            PropertyValue::Float(f) => *f,
            PropertyValue::String(s) => s.parse().unwrap_or(0.0),
        }
    }
    
    /// Convert to int value
    pub fn as_i64(&self) -> i64 {
        match self {
            PropertyValue::Int(i) => *i,
            PropertyValue::Float(f) => *f as i64,
            PropertyValue::String(s) => s.parse().unwrap_or(0),
        }
    }
}

/// A World Entity - can be a location, character, faction, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldEntity {
    /// Unique identifier
    pub id: Uuid,
    
    /// Entity type: "location", "character", "faction", "item", etc.
    pub entity_type: String,
    
    /// Display name
    pub name: String,
    
    /// Description of the entity
    pub description: String,
    
    /// Long description - detailed history/background
    #[serde(default)]
    pub long_description: String,
    
    /// Position in the world
    pub x: f64,
    pub y: f64,
    
    /// Properties stored as different types
    pub properties_int: HashMap<String, i64>,
    pub properties_float: HashMap<String, f64>,
    pub properties_string: HashMap<String, String>,
    
    /// Tags for filtering and categorization
    pub tags: Vec<String>,
    
    /// Optional owner (UUID of another entity)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<Uuid>,
    
    /// Entities owned by this entity
    #[serde(default)]
    pub owned_entities: Vec<Uuid>,
    
    /// History of events/actions
    #[serde(default)]
    pub history: Vec<HistoryEntry>,

    /// History summary (updated periodically)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_summary: Option<String>,

    /// When this entity was last active in a world action
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_action_at: Option<DateTime<Utc>>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,

    /// Time preferences for this entity
    #[serde(default)]
    pub time_preferences: EntityTimePreferences,

    // -------------------------------------------------------------------
    // Hardcoded game-relationship fields (added 2026-06-15 per Arcurus
    // #openworld: "fractionId, fractionSecretLoyalId, homeLocationId,
    // birthLocationId, leaderId, regionId ... to the hard coded entity
    // data fields").
    //
    // All six are `Option<Uuid>`: an entity that does not belong to a
    // faction / does not have a home / etc. simply has `None`. This
    // mirrors the `owner_id` precedent: it is also `Option<Uuid>` and
    // the LLM never sees it, the operator sets it via the API.
    //
    // These are REAL struct fields (not `properties_int` keys) so:
    //   1. The LLM effect writer cannot tamper with them — they're
    //      not in any property map, so the per-property PUT is the
    //      only way to change them (operator-only).
    //   2. The LLM-facing property context builder never lists them
    //      (it iterates the property maps, not the struct fields).
    //   3. Save/load round-trips them as first-class fields via the
    //      binary format (ENTITY_VERSION bumped 1→2 on 2026-06-15).
    //   4. We can index them / build lookups (faction → members)
    //      without scanning every entity's `properties_int`.
    //
    // Naming note: Arcurus wrote "fractionId" in chat; the in-world
    // term (and the existing `entity_type`) is "faction". We use
    // `faction_id` everywhere in the codebase to match the lore and
    // the rest of the engine (confirmed by Arcurus 2026-06-15).
    // -------------------------------------------------------------------

    /// Primary faction this entity belongs to. `None` for unaffiliated
    /// entities (most locations, lone heroes, dragons, artifacts).
    /// For a faction entity itself, this is `None` — a faction is the
    /// top of the membership chain, not a member of itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faction_id: Option<Uuid>,

    /// Secret loyalty to a different faction. Set ONLY when this entity
    /// is publicly a member of one faction (`faction_id`) but secretly
    /// loyal to another (a spy / double agent). For most entities this
    /// is `None`. Like `faction_id`, `None` for faction entities.
    /// The "secret" semantic is important: the LLM context builder
    /// should NOT show this field to the LLM (a faction member's
    /// secret loyalty is a spoiler for the simulator). Kept off the
    /// LLM-emit surface entirely; only the operator can set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faction_secret_loyal_id: Option<Uuid>,

    /// Home location for entities that can return "home" (most
    /// characters; a few locations like sanctuaries). `None` for
    /// entities without a fixed home (wanderers, dragons, etc.).
    /// For a location entity, `home_location_id` is `None` — a
    /// location IS a place, it does not have a separate "home".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_location_id: Option<Uuid>,

    /// Birthplace of this entity. `None` when lore does not pin a
    /// specific birthplace. Conceptually distinct from `home_location_id`
    /// (where they live now vs. where they were born).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_location_id: Option<Uuid>,

    /// Leader of this entity. Convention (per Arcurus 2026-06-15):
    ///   - For a FACTION entity, `leader_id` points to the character
    ///     who leads that faction.
    ///   - For a CHARACTER entity, `leader_id` may point to another
    ///     character/leader they follow (e.g. a squire following a
    ///     knight commander). For most current characters this is
    ///     `None` — we add character→leader links as the lore grows.
    ///   - For a LOCATION/DRAGON/ARTIFACT entity, `leader_id` is
    ///     `None` (locations are not "led" by anyone; dragons are
    ///     solitary sovereigns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_id: Option<Uuid>,

    /// Region this entity belongs to. Currently the world has one
    /// region (the realm itself) — see the region entity created in
    /// the v2 rollout (`The Realm of Shadows`, `entity_type="region"`).
    /// System entities (World Clock and any `meta`-tagged entity) have
    /// `region_id = None` and the system-entity guard prevents the
    /// LLM from setting it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_id: Option<Uuid>,

    // -------------------------------------------------------------------
    // Marker field (moved out of `properties_int` on 2026-06-15).
    // -------------------------------------------------------------------
    /// Per-entity "processed up to" marker for the unprocessed-other-
    /// actions LLM block. After every LLM call that showed this entity
    /// the "actions from other entities" list, the orchestrator
    /// advances this marker to the max tick of the entries that were
    /// rendered, so future calls don't re-show the same entries.
    ///
    /// **Previously** this lived in `properties_int["last_processed_other_tick"]`
    /// (the LLM-internal bookkeeping list). The move to a struct field
    /// (per Arcurus 2026-06-15 #openworld: "we dont mix anymore
    /// programatical stuff with game based values") keeps the
    /// LLM-internal bookkeeping list clean and gives the marker a
    /// dedicated, well-typed slot.
    ///
    /// The v1→v2 migration in `persistence.rs::deserialize_world`
    /// reads the existing value from `properties_int` on first load
    /// and seeds this field. The old `properties_int` key is kept in
    /// the entity map (per Arcurus "let the old ... field in for now
    /// just update the code to use the new one. once all works we can
    /// delet it"). The code path uses ONLY this field; the old key in
    /// `properties_int` is dead data, pending a future cleanup pass.
    ///
    /// Default: 0 (never processed any other-entity actions).
    #[serde(default)]
    pub last_processed_other_tick: i64,
}

impl WorldEntity {
    /// Create a new entity with basic fields
    pub fn new(entity_type: &str, name: &str, x: f64, y: f64) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            entity_type: entity_type.to_string(),
            name: name.to_string(),
            description: String::new(),
            long_description: String::new(),
            x,
            y,
            properties_int: HashMap::new(),
            properties_float: HashMap::new(),
            properties_string: HashMap::new(),
            tags: Vec::new(),
            owner_id: None,
            owned_entities: Vec::new(),
            history: Vec::new(),
            history_summary: None,
            last_action_at: None,
            created_at: now,
            updated_at: now,
            time_preferences: EntityTimePreferences::new(),
            // Hardcoded game-relationship fields (2026-06-15). All
            // default to None; the operator / API sets them when the
            // entity is created or via PUT.
            faction_id: None,
            faction_secret_loyal_id: None,
            home_location_id: None,
            birth_location_id: None,
            leader_id: None,
            region_id: None,
            // Marker for the unprocessed-other-actions LLM block.
            // Default 0 = "never processed any other-entity actions".
            last_processed_other_tick: 0,
        }
    }
    
    /// Get an integer property
    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.properties_int.get(key).copied()
    }
    
    /// Get a float property
    pub fn get_float(&self, key: &str) -> Option<f64> {
        self.properties_float.get(key).copied()
    }
    
    /// Get a string property
    pub fn get_string(&self, key: &str) -> Option<&String> {
        self.properties_string.get(key)
    }
    
    /// Set an integer property
    pub fn set_int(&mut self, key: &str, value: i64) {
        self.properties_int.insert(key.to_string(), value);
        self.updated_at = Utc::now();
    }
    
    /// Set a float property
    pub fn set_float(&mut self, key: &str, value: f64) {
        self.properties_float.insert(key.to_string(), value);
        self.updated_at = Utc::now();
    }
    
    /// Set a string property
    pub fn set_string(&mut self, key: &str, value: &str) {
        self.properties_string.insert(key.to_string(), value.to_string());
        self.updated_at = Utc::now();
    }
    
    /// Check if entity has a specific tag
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// True if this is a system (non-LLM-driven) entity. Identified by
    /// `entity_type == "abstract"` (the new umbrella category per
    /// Arcurus 2026-06-07 #openworld, which replaced the narrower
    /// `"world_clock"` name), or `entity_type == "world_clock"`
    /// (kept for backward compat with pre-migration save files), or
    /// any tag of `"meta"`. Property writes to such entities are
    /// blocked at the LLM-effect layer to prevent garbage from
    /// corrupting world state (see todo c7f3bc27).
    pub fn is_system_entity(&self) -> bool {
        self.entity_type == "abstract"
            || self.entity_type == "world_clock"
            || self.has_tag("meta")
    }

    /// Add a tag
    pub fn add_tag(&mut self, tag: &str) {
        if !self.has_tag(tag) {
            self.tags.push(tag.to_string());
            self.updated_at = Utc::now();
        }
    }

    /// Remove a tag (no-op if not present).
    pub fn remove_tag(&mut self, tag: &str) {
        if let Some(pos) = self.tags.iter().position(|t| t == tag) {
            self.tags.remove(pos);
            self.updated_at = Utc::now();
        }
    }

    /// Add history entry
    pub fn add_history(&mut self, action: &str, details: &str, outcome: &str) {
        self.history.push(HistoryEntry::new(action, details, outcome));
        self.last_action_at = Some(Utc::now());
        self.updated_at = Utc::now();
    }
    
    /// Calculate distance to another entity
    pub fn distance_to(&self, other: &WorldEntity) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
    
    /// Calculate power score for action weighting
    pub fn power_score(&self) -> f64 {
        self.get_int("power").unwrap_or(0) as f64
            + self.get_float("power").unwrap_or(0.0)
            + self.get_string("power_rank").map(|r| match r.as_str() {
                "legendary" => 100.0,
                "epic" => 50.0,
                "rare" => 25.0,
                "common" => 10.0,
                _ => 0.0,
            }).unwrap_or(0.0)
    }
    
    /// Calculate wealth score
    pub fn wealth_score(&self) -> f64 {
        self.get_int("wealth").unwrap_or(0) as f64
            + self.get_float("wealth").unwrap_or(0.0)
    }
    
    /// Calculate mana score
    pub fn mana_score(&self) -> f64 {
        self.get_int("mana").unwrap_or(0) as f64
            + self.get_int("black_mana").unwrap_or(0) as f64 * 1.5
            + self.get_int("white_mana").unwrap_or(0) as f64 * 1.5
            + self.get_float("mana").unwrap_or(0.0)
    }
    
    /// Get unspent power (total - spent)
    pub fn unspent_power(&self) -> f64 {
        let total = self.power_score();
        let spent = self.get_int("power_spent").unwrap_or(0) as f64
            + self.get_float("power_spent").unwrap_or(0.0);
        (total - spent).max(0.0)
    }
    
    /// Get unspent wealth (total - spent)
    pub fn unspent_wealth(&self) -> f64 {
        let total = self.wealth_score();
        let spent = self.get_int("wealth_spent").unwrap_or(0) as f64
            + self.get_float("wealth_spent").unwrap_or(0.0);
        (total - spent).max(0.0)
    }
    
    /// Get unspent mana (total - spent)
    pub fn unspent_mana(&self) -> f64 {
        let total = self.mana_score();
        let spent = self.get_int("mana_spent").unwrap_or(0) as f64
            + self.get_float("mana_spent").unwrap_or(0.0);
        (total - spent).max(0.0)
    }
    
    /// Calculate hours since last action
    pub fn hours_since_last_action(&self) -> f64 {
        if let Some(last_action) = self.last_action_at {
            let now = Utc::now();
            let duration = now - last_action;
            duration.num_minutes() as f64 / 60.0
        } else {
            // Never acted - return high value to prioritize new entities
            1000.0
        }
    }
    
    /// Calculate action selection score for this entity
    /// Formula: unspent_power * unspent_wealth * unspent_mana * time_since_last_action
    pub fn action_selection_score(&self) -> f64 {
        let power = self.unspent_power().max(1.0);  // At least 1 to avoid zero
        let wealth = self.unspent_wealth().max(1.0);
        let mana = self.unspent_mana().max(1.0);
        let time_factor = self.hours_since_last_action().max(1.0);
        
        power * wealth * mana * time_factor
    }
}

/// A single history entry for an entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub details: String,
    pub outcome: String,
}

impl HistoryEntry {
    pub fn new(action: &str, details: &str, outcome: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            action: action.to_string(),
            details: details.to_string(),
            outcome: outcome.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_create_entity() {
        let entity = WorldEntity::new("location", "Test Village", 10.0, 20.0);
        assert_eq!(entity.name, "Test Village");
        assert_eq!(entity.x, 10.0);
        assert_eq!(entity.y, 20.0);
    }
    
    #[test]
    fn test_properties() {
        let mut entity = WorldEntity::new("character", "Hero", 0.0, 0.0);
        entity.set_int("power", 50);
        entity.set_float("defense", 0.75);
        entity.set_string("class", "Warrior");
        
        assert_eq!(entity.get_int("power"), Some(50));
        assert_eq!(entity.get_float("defense"), Some(0.75));
        assert_eq!(entity.get_string("class"), Some(&"Warrior".to_string()));
    }
    
    #[test]
    fn test_tags() {
        let mut entity = WorldEntity::new("location", "Dark Forest", 0.0, 0.0);
        entity.add_tag("dangerous");
        entity.add_tag("forested");
        
        assert!(entity.has_tag("dangerous"));
        assert!(!entity.has_tag("peaceful"));
    }
    
    #[test]
    fn test_distance() {
        let e1 = WorldEntity::new("location", "A", 0.0, 0.0);
        let e2 = WorldEntity::new("location", "B", 3.0, 4.0);
        assert!((e1.distance_to(&e2) - 5.0).abs() < 0.001);
    }

    // -------------------------------------------------------------------
    // v2 relationship-fields tests (2026-06-15, per Arcurus #openworld)
    // -------------------------------------------------------------------

    #[test]
    fn new_entity_has_all_relationship_fields_set_to_none() {
        // The default for the six relationship fields (and the
        // marker) must be `None` / 0, so a freshly-created entity
        // is "unaffiliated, no home, no leader, no region" out of
        // the box.  The operator sets the actual values via the
        // API.
        let e = WorldEntity::new("character", "Lonely", 0.0, 0.0);
        assert!(e.faction_id.is_none());
        assert!(e.faction_secret_loyal_id.is_none());
        assert!(e.home_location_id.is_none());
        assert!(e.birth_location_id.is_none());
        assert!(e.leader_id.is_none());
        assert!(e.region_id.is_none());
        assert_eq!(e.last_processed_other_tick, 0);
    }

    #[test]
    fn relationship_fields_round_trip_through_serde() {
        // Sanity check that the new fields are part of the JSON
        // serialization (they ARE because of `Serialize` on the
        // struct, but the `skip_serializing_if` and `default`
        // annotations need a moment of attention).  The serde
        // shape we expect on the wire: a v2 entity JSON includes
        // the six fields as `null` when not set, and as a UUID
        // string when set.
        let mut e = WorldEntity::new("character", "Kira", 0.0, 0.0);
        let faction = Uuid::new_v4();
        let region = Uuid::new_v4();
        e.faction_id = Some(faction);
        e.region_id = Some(region);
        e.last_processed_other_tick = 1234;

        let json = serde_json::to_string(&e).unwrap();
        // Both set fields present as JSON strings.
        assert!(json.contains(&format!("\"faction_id\":\"{}\"", faction)));
        assert!(json.contains(&format!("\"region_id\":\"{}\"", region)));
        assert!(json.contains("\"last_processed_other_tick\":1234"));
        // The unset fields are SKIPPED on serialize (we use
        // `skip_serializing_if = "Option::is_none"`), so they
        // must NOT be present in the JSON.
        assert!(!json.contains("faction_secret_loyal_id"));
        assert!(!json.contains("home_location_id"));
        assert!(!json.contains("birth_location_id"));
        assert!(!json.contains("leader_id"));

        // Deserialize back and confirm the values survived.
        let de: WorldEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(de.faction_id, Some(faction));
        assert_eq!(de.region_id, Some(region));
        assert_eq!(de.last_processed_other_tick, 1234);
        // The fields we left None on the original must come
        // back as None on the deserialized value (serde's
        // `default` annotation gives us this for free).
        assert!(de.faction_secret_loyal_id.is_none());
        assert!(de.home_location_id.is_none());
        assert!(de.birth_location_id.is_none());
        assert!(de.leader_id.is_none());
    }

    #[test]
    fn test_is_system_entity() {
        // world_clock type -> system (legacy, kept for
        // backward compat with pre-2026-06-07 save files)
        let mut clock = WorldEntity::new("world_clock", "World Clock", 0.0, 0.0);
        assert!(clock.is_system_entity());

        // abstract type -> system (new canonical type for
        // the clock + future non-narrative bookkeeping
        // entities; per Arcurus 2026-06-07 #openworld)
        let mut abstract_entity = WorldEntity::new("abstract", "World Clock", 0.0, 0.0);
        assert!(abstract_entity.is_system_entity());

        // Any other "abstract" entity (e.g. a hypothetical
        // "Lore Anchor" or "Time Marker") also counts.
        let mut marker = WorldEntity::new("abstract", "Spring Equinox Marker", 0.0, 0.0);
        assert!(marker.is_system_entity());

        // meta tag -> system (tag-based recognition still
        // works for any entity_type)
        let mut meta = WorldEntity::new("config", "Meta Config", 0.0, 0.0);
        meta.add_tag("meta");
        assert!(meta.is_system_entity());

        // regular entity -> not system
        let hero = WorldEntity::new("hero", "Kira", 0.0, 0.0);
        assert!(!hero.is_system_entity());

        // regular entity with non-meta tag -> not system
        let mut loc = WorldEntity::new("location", "Oak Valley", 0.0, 0.0);
        loc.add_tag("peaceful");
        assert!(!loc.is_system_entity());

        // abstract entity without meta tag is still system
        // (recognition is on entity_type, not on tag)
        let bare = WorldEntity::new("abstract", "Bare Marker", 0.0, 0.0);
        assert!(bare.is_system_entity());
    }
}
