use crate::WorldEntity;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::world_data::time_system::WorldTime;

/// Special entity type for world clock
const CLOCK_ENTITY_TYPE: &str = "world_clock";

/// Represents an active world event that influences entity actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldEvent {
    /// Unique identifier for the event
    pub id: String,
    /// Name/title of the event
    pub name: String,
    /// Detailed description of the event
    pub description: String,
    /// How this event affects entity behaviors
    #[serde(default)]
    pub influence: String,
    /// Whether this event is currently active
    #[serde(default = "default_true")]
    pub active: bool,
}

pub fn default_true() -> bool { true }

/// The canonical set of lore-based default events for a new world.
/// Mirrors the six events documented in
/// `docs/world_lore.md` and `docs/world_events.md`:
/// five "Shadow Awakening" era events (doom arc) plus the
/// counterbalancing "Spring Festival of Renewal" (hope arc).
/// Stable UUIDs are used so a freshly seeded world is reproducible
/// across runs.
pub fn default_world_events() -> Vec<WorldEvent> {
    vec![
        WorldEvent {
            id: "e10f8432-2dbe-4b73-9826-2366c7772c9f".to_string(),
            name: "The Shadow Awakens".to_string(),
            description: "For centuries dismissed as myth, the Scrolls of the First Age spoke of a darkness that would return when the realm forgot its vigilance. Now the signs are undeniable: shadows in the Northern Pass stretch longer than the mountains themselves, animals flee southward, and the Moonwell at Elder Moonthorn reflects something other than the future. The Prophecy of the Shadow Crown has begun to unfold.".to_string(),
            influence: "Entities grow suspicious, militaristic, and watchful. Silverstream Keep mobilizes. Ironforge forges weapons day and night. Whisperwood closes its borders. Trade becomes riskier. Trust between factions erodes. Power-hungry actors see opportunity. The realm is tense with approaching doom.".to_string(),
            active: true,
        },
        WorldEvent {
            id: "c7eca4b6-8dc8-45ba-ba5a-337803de3019".to_string(),
            name: "Velora Walks Again".to_string(),
            description: "A knight in corroded silver armor has been sighted on the roads at night. Her helm reflects no light and she leaves no shadow. Velora the Undying, who held the Northern Pass alone for seven days during the Demon Wars, has returned. She seeks the Forgotten Heir mentioned in the prophecy and trades secrets with those brave enough to meet her gaze.".to_string(),
            influence: "Heroes and knights feel a stirring of destiny. Some seek Velora out for blessings. Others fear her appearance as a sign of the worst. Kira Dawnblade in particular feels the prophecy pulling at her. Mira the Merchant has rare tales to sell. The Silver Wardens of Silverstream Keep sense the return of their founder.".to_string(),
            active: true,
        },
        WorldEvent {
            id: "a23dac23-4fd1-4936-9696-059cae6ce77d".to_string(),
            name: "The Shadowmaw Stirs".to_string(),
            description: "Ironforge miners report tremors deep beneath Frostpeak. The forges have grown hot without fuel. The clan elders whisper of bad dreams — impossible dreams of black wings and a heartbeat that shakes the world. Vaelthrix the Endless, the ancient dragon who slept beneath the Frostpeak Mountains before the First Age, has begun to dream. Her dreams leak into the world as visions and earthquakes.".to_string(),
            influence: "Dwarves of Ironforge grow fearful but resolute. Miners dig deeper in search of ancient weapons. Mountain-dwelling entities feel the tremors. The wandering bard hears songs about dragons returning. Some interpret the dreams as omens; others as opportunities. The realm feels heavier, charged with waiting.".to_string(),
            active: true,
        },
        WorldEvent {
            id: "88f129bd-2c08-4f23-9969-4818d3858bfd".to_string(),
            name: "The Silver Wardens Mobilize".to_string(),
            description: "The banners of Silverstream Keep fly from every tower. Knights ride out in pairs along the northern roads. A formal decree has been issued: every traveler must declare their business or be turned back. The Silver Wardens — Silverstream Keep's elite order — believe themselves the prophesied defenders of the realm. They have begun recruiting among the common folk, and the cost of admission is a secret they will not share.".to_string(),
            influence: "Knights and warriors grow bold. Refugees and villagers consider joining. Bandits and outlaws grow more cautious. The Keep itself grows in power, but at the cost of internal suspicion. The mobilization of one faction pressures all others — should they also prepare for war? Trade slows. Tensions rise along every road.".to_string(),
            active: true,
        },
        WorldEvent {
            id: "46a976d2-c2a7-46b5-903f-1a04ae751058".to_string(),
            name: "The Bells of the Sunken Temple".to_string(),
            description: "Travelers near the southern marshlands report hearing bells at dusk. The Sunken Temple — half-submerged since the Second Age and abandoned for a thousand years — has begun to ring. No one has yet dared enter. The Wandering Bard claims to have heard a voice singing along with the bells, in a language no scholar recognizes. Mira the Scribe is taking notes.".to_string(),
            influence: "Scholars and sages grow curious. Adventurers plan expeditions. Locals avoid the marshlands. Zephyrus the Oracle speaks in riddles about it, which everyone interprets differently. The realm feels as if something is waking that was meant to stay asleep. The Drowned City, said to be the temple sister, has grown quieter — its silence more ominous than its noise.".to_string(),
            active: true,
        },
        WorldEvent {
            id: "88ee73fc-69cb-4366-a5ea-481aa175cfab".to_string(),
            name: "The Spring Festival of Renewal".to_string(),
            description: "Despite the spreading shadow, the villages of the realm gather in the Oak Valley green at the height of spring to celebrate survival itself. For three days and nights, the folk of Oak Valley, Silverstream, and the Ironforge trade roads open their gates, lay down old grudges, and remember that hope is something you must tend like a fire. Bards sing, children run free, and even the Shadow Crown's reach seems—impossibly—a little lighter when every hearth in the valley burns at once. Mira the Scribe calls it the only honest currency: shared bread, shared song, shared laughter in the dark.".to_string(),
            influence: "Trade flows more freely for the festival's duration. Factional suspicion eases; the Silver Wardens soften their patrols and even share a cup with passing rangers. Children play without fear, and the realm's surviving heroes feel their burdens lifted. Entities grow reflective rather than reactive, planning for a future they had nearly given up on. The Wandering Bard calls it 'the stubborn ember.' Ironforge forges glow warmer for the celebration. Kira Dawnblade attends for the first time in years.".to_string(),
            active: true,
        },
    ]
}

/// Get the fixed UUID for world clock entity
fn clock_entity_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
}

/// The world state - holds all entities and world metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct World {
    /// World name
    pub name: String,
    
    /// World description
    #[serde(default)]
    pub description: String,
    
    /// All entities indexed by UUID
    pub entities: HashMap<Uuid, WorldEntity>,
    
    /// Paths between entities for non-Euclidean travel
    #[serde(default)]
    pub paths: Vec<Path>,
    
    /// World settings
    pub settings: WorldSettings,
    
    /// World integer properties
    #[serde(default)]
    pub properties_int: HashMap<String, i64>,
    
    /// World float properties
    #[serde(default)]
    pub properties_float: HashMap<String, f64>,
    
    /// World string properties
    #[serde(default)]
    pub properties_string: HashMap<String, String>,
    
    /// Last world action timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_world_action: Option<chrono::DateTime<Utc>>,
    
    /// Number of world actions performed
    #[serde(default)]
    pub action_count: u64,
    
    /// World time tracking (days, hours, time of day)
    #[serde(default)]
    pub world_time: WorldTime,
    
    /// Active world events that influence entity actions
    #[serde(default)]
    pub active_events: Vec<WorldEvent>,
}

/// Statistics for a property across entities of a specific type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyStats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub count: usize,
}

/// Statistics for all properties grouped by entity type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityTypeStats {
    pub properties_int: HashMap<String, PropertyStats>,
    pub properties_float: HashMap<String, PropertyStats>,
}

/// World statistics - computed once after load/save
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldStats {
    pub by_type: HashMap<String, EntityTypeStats>,
    pub world_properties: HashMap<String, PropertyStats>,
}

impl World {
    /// Calculate world statistics for all properties grouped by entity type
    pub fn calculate_stats(&self) -> WorldStats {
        let mut by_type: HashMap<String, EntityTypeStats> = HashMap::new();
        
        // Group entities by type
        let mut entities_by_type: HashMap<String, Vec<&WorldEntity>> = HashMap::new();
        for entity in self.entities.values() {
            entities_by_type
                .entry(entity.entity_type.clone())
                .or_default()
                .push(entity);
        }
        
        // Calculate stats for each type
        for (entity_type, entities) in entities_by_type {
            let mut type_stats = EntityTypeStats {
                properties_int: HashMap::new(),
                properties_float: HashMap::new(),
            };
            
            // Collect all int property keys
            let mut int_keys: HashSet<String> = HashSet::new();
            for entity in &entities {
                for key in entity.properties_int.keys() {
                    int_keys.insert(key.clone());
                }
            }
            
            // Calculate stats for each int property
            for key in int_keys {
                let values: Vec<f64> = entities
                    .iter()
                    .filter_map(|e| e.properties_int.get(&key).map(|v| *v as f64))
                    .collect();
                
                if !values.is_empty() {
                    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
                    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let avg = values.iter().sum::<f64>() / values.len() as f64;
                    
                    type_stats.properties_int.insert(key, PropertyStats {
                        min,
                        max,
                        avg,
                        count: values.len(),
                    });
                }
            }
            
            // Collect all float property keys
            let mut float_keys: HashSet<String> = HashSet::new();
            for entity in &entities {
                for key in entity.properties_float.keys() {
                    float_keys.insert(key.clone());
                }
            }
            
            // Calculate stats for each float property
            for key in float_keys {
                let values: Vec<f64> = entities
                    .iter()
                    .filter_map(|e| e.properties_float.get(&key).map(|v| *v))
                    .collect();
                
                if !values.is_empty() {
                    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
                    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let avg = values.iter().sum::<f64>() / values.len() as f64;
                    
                    type_stats.properties_float.insert(key, PropertyStats {
                        min,
                        max,
                        avg,
                        count: values.len(),
                    });
                }
            }
            
            by_type.insert(entity_type, type_stats);
        }
        
        // Calculate world property stats (from world properties themselves)
        let mut world_properties: HashMap<String, PropertyStats> = HashMap::new();
        
        // World int properties stats
        for (key, value) in &self.properties_int {
            world_properties.insert(key.clone(), PropertyStats {
                min: *value as f64,
                max: *value as f64,
                avg: *value as f64,
                count: 1,
            });
        }
        
        // World float properties stats
        for (key, value) in &self.properties_float {
            world_properties.insert(key.clone(), PropertyStats {
                min: *value,
                max: *value,
                avg: *value,
                count: 1,
            });
        }
        
        WorldStats {
            by_type,
            world_properties,
        }
    }
    
    /// Get the relative description for a property value
    pub fn get_relative_value(value: f64, min: f64, max: f64, avg: f64) -> &'static str {
        if max == min {
            return "medium";
        }
        
        let range = max - min;
        let position = (value - min) / range;
        
        if position < 0.2 {
            "very low"
        } else if position < 0.4 {
            "low"
        } else if position < 0.6 {
            "medium"
        } else if position < 0.8 {
            "high"
        } else {
            "very high"
        }
    }
}

/// A path connecting two entities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Path {
    pub id: Uuid,
    pub from_id: Uuid,
    pub to_id: Uuid,
    pub blocked: bool,
    pub blocked_reason: Option<String>,
    /// Distance modifier (1.0 = normal, >1.0 = longer path)
    pub distance_modifier: f64,
    pub path_type: String, // "road", "river", "mountain", "magical", etc.
}

impl World {
    /// Create a new empty world
    pub fn new(name: &str) -> Self {
        let mut world = Self {
            name: name.to_string(),
            description: String::new(),
            entities: HashMap::new(),
            paths: Vec::new(),
            settings: WorldSettings::default(),
            properties_int: HashMap::new(),
            properties_float: HashMap::new(),
            properties_string: HashMap::new(),
            last_world_action: None,
            action_count: 0,
            world_time: WorldTime::new(),
            active_events: Vec::new(),
        };
        // Create the world clock entity
        world.create_clock_entity();
        // Auto-bootstrap with the canonical lore events so a fresh world
        // already has narrative momentum (see todo e4cc4203).
        world.seed_default_events();
        world
    }

    /// Seed the world with the 7 canonical sample entities described in
    /// the README and `docs/world_entities.md`:
    ///   Oak Valley Village, Shadow Ridge Camp, Elder Moonthorn,
    ///   Whisperwood Forest, Silverstream Keep, Ironforge Clan,
    ///   Mira the Merchant.
    ///
    /// **Idempotent**: entities whose (name, entity_type) already exist
    /// are skipped. Safe to call on a fresh world OR on an existing
    /// world that's missing one or more of the canonical entities.
    ///
    /// **NOT called from `World::new()`** — per Arcurus 2026-06-04,
    /// fresh worlds start with just the world clock + the canonical
    /// lore events (no auto-seeded entities). The web UI's
    /// "Generate sample entities" button (and the
    /// `POST /api/world/create?generate_sample=true` endpoint) call
    /// this explicitly.
    ///
    /// Returns the number of entities actually added.
    pub fn seed_sample_entities(&mut self) -> usize {
        use WorldEntity;

        fn make_location(name: &str, x: f64, y: f64, desc: &str, tags: &[&str]) -> WorldEntity {
            let mut e = WorldEntity::new("location", name, x, y);
            e.description = desc.to_string();
            for t in tags {
                e.add_tag(t);
            }
            e
        }
        fn make_character(name: &str, x: f64, y: f64, desc: &str, tags: &[&str]) -> WorldEntity {
            let mut e = WorldEntity::new("character", name, x, y);
            e.description = desc.to_string();
            for t in tags {
                e.add_tag(t);
            }
            e
        }
        fn make_faction(name: &str, x: f64, y: f64, desc: &str, tags: &[&str]) -> WorldEntity {
            let mut e = WorldEntity::new("faction", name, x, y);
            e.description = desc.to_string();
            for t in tags {
                e.add_tag(t);
            }
            e
        }

        // Tiny builders so the per-entity `.with_int(...)` calls read
        // fluently. Equivalent to `e.set_int("power", 45)`.
        trait IntInit {
            fn with_int(self, key: &str, val: i64) -> Self;
        }
        impl IntInit for WorldEntity {
            fn with_int(mut self, key: &str, val: i64) -> Self {
                self.set_int(key, val);
                self
            }
        }

        let candidates: Vec<WorldEntity> = vec![
            make_location(
                "Oak Valley Village", 150.0, 250.0,
                "A peaceful farming village.",
                &["village", "peaceful", "farming"],
            ),
            make_location(
                "Shadow Ridge Camp", 280.0, 320.0,
                "Hidden bandit encampment.",
                &["bandit", "dangerous", "mountain"],
            )
            .with_int("power", 45)
            .with_int("wealth", 200)
            .with_int("black_mana", 80),
            make_character(
                "Elder Moonthorn", 145.0, 245.0,
                "Wise guardian of the forest.",
                &["elf", "wise", "guardian"],
            ),
            make_location(
                "Whisperwood Forest", 140.0, 220.0,
                "Ancient forest with strange magic.",
                &["forest", "magical", "ancient"],
            ),
            make_location(
                "Silverstream Keep", 320.0, 180.0,
                "Fortified castle overlooking the river.",
                &["castle", "royal"],
            )
            .with_int("power", 100)
            .with_int("wealth", 500),
            make_faction(
                "Ironforge Clan", 420.0, 350.0,
                "Mighty dwarven smiths and warriors.",
                &["dwarven", "clan", "smiths"],
            ),
            make_character(
                "Mira the Merchant", 200.0, 290.0,
                "Traveling merchant with exotic goods.",
                &["merchant", "trader"],
            ),
        ];

        let mut added = 0;
        for ent in candidates {
            // Skip if a same-named same-type entity already exists.
            let dup = self.entities.values().any(|e| {
                e.name.eq_ignore_ascii_case(&ent.name) && e.entity_type == ent.entity_type
            });
            if dup {
                continue;
            }
            // Build with a fresh UUID via add_entity.
            self.add_entity(ent);
            added += 1;
        }
        added
    }

    /// Seed the world with the canonical set of lore-based default events
    /// (the "Shadow Awakening" era described in `docs/world_lore.md` and
    /// `docs/world_events.md`). Idempotent: a no-op if the world already
    /// has any active events. New worlds start with these so the LLM has
    /// narrative context from the very first entity action.
    pub fn seed_default_events(&mut self) {
        if !self.active_events.is_empty() {
            return;
        }
        self.active_events = default_world_events();
    }
    
    /// Create the world clock entity if it doesn't exist
    pub fn create_clock_entity(&mut self) {
        if !self.entities.contains_key(&clock_entity_id()) {
            let mut clock = WorldEntity::new(CLOCK_ENTITY_TYPE, "World Clock", 0.0, 0.0);
            clock.id = clock_entity_id();
            clock.description = "The world clock entity that tracks time in ticks".to_string();
            clock.tags.push("meta".to_string());
            
            // Initialize time properties
            clock.properties_int.insert("day".to_string(), 0);
            clock.properties_int.insert("hour".to_string(), 8);
            clock.properties_int.insert("actions_today".to_string(), 0);
            clock.properties_float.insert("total_years".to_string(), 0.0);
            clock.properties_string.insert("last_real_time".to_string(), Utc::now().to_rfc3339());
            
            self.entities.insert(clock_entity_id(), clock);
        }
    }
    
    /// Get the world clock entity (creates if not exists)
    pub fn get_clock_entity_mut(&mut self) -> Option<&mut WorldEntity> {
        if !self.entities.contains_key(&clock_entity_id()) {
            self.create_clock_entity();
        }
        self.entities.get_mut(&clock_entity_id())
    }
    
    /// Get the world clock entity (immutable)
    pub fn get_clock_entity(&self) -> Option<&WorldEntity> {
        self.entities.get(&clock_entity_id())
    }
    
    /// Sync world_time from the clock entity
    pub fn sync_time_from_clock(&mut self) {
        if let Some(clock) = self.entities.get(&clock_entity_id()) {
            self.world_time.day = clock.get_int("day").unwrap_or(0) as u32;
            self.world_time.hour = clock.get_int("hour").unwrap_or(8) as u8;
            self.world_time.actions_today = clock.get_int("actions_today").unwrap_or(0) as u32;
            self.world_time.total_years = clock.get_float("total_years").unwrap_or(0.0);
            
            if let Some(last_time_str) = clock.get_string("last_real_time") {
                if let Ok(last_time) = chrono::DateTime::parse_from_rfc3339(last_time_str) {
                    self.world_time.last_real_time = Some(last_time.with_timezone(&Utc));
                }
            }
        }
    }
    
    /// Sync world_time to the clock entity
    pub fn sync_time_to_clock(&mut self) {
        if let Some(clock) = self.entities.get_mut(&clock_entity_id()) {
            clock.properties_int.insert("day".to_string(), self.world_time.day as i64);
            clock.properties_int.insert("hour".to_string(), self.world_time.hour as i64);
            clock.properties_int.insert("actions_today".to_string(), self.world_time.actions_today as i64);
            clock.properties_float.insert("total_years".to_string(), self.world_time.total_years);
            
            if let Some(last_time) = self.world_time.last_real_time {
                clock.properties_string.insert("last_real_time".to_string(), last_time.to_rfc3339());
            }
        }
    }

    /// Sanitize int AND float properties on system entities
    /// (`is_system_entity()`).
    ///
    /// Background: before `c7f3bc27` (fix(clock): protect system entities
    /// from LLM effect writes), the LLM effect writer occasionally emitted
    /// garbage values (e.g. `day = 2.5e9`, `history_entries = 3.8e18`) on
    /// the world_clock entity. Those bad values were then auto-saved
    /// faithfully, so even after the upstream fix, persisted worlds still
    /// have the corruption visible via `/api/world`.
    ///
    /// This method resets any system-entity int property whose magnitude
    /// exceeds a sane bound for that key. For the well-known clock keys
    /// (`day`, `hour`, `actions_today`) the value is restored from the
    /// current `world_time`; for all other system-entity int props the
    /// value is reset to 0.
    ///
    /// For float properties on system entities, the only well-known key is
    /// `total_years` — a time counter that legitimately grows without
    /// bound (1 real minute = 1 game year, so a long-running world
    /// accumulates millions of years over a few real days). It is clamped
    /// to `MAX_TOTAL_YEARS` (1e15 — about 31 million real-time millennia
    /// at 1 real-sec-per-game-year) with the original sign preserved;
    /// anything beyond that is the LLM garbage signature. All other
    /// system-entity float keys fall back to the non-system float cap
    /// (1e7) — defensive default.
    ///
    /// Returns `(entity_id, key, old_value, new_value)` for every int
    /// repair AND `(entity_id, key, old_value, new_value)` (f64-typed)
    /// for every float repair. The two lists are kept separate so callers
    /// can log them in distinct streams. Non-system entities are untouched
    /// (use `sanitize_non_system_entity_properties` for those).
    pub fn sanitize_system_entities(
        &mut self,
    ) -> (Vec<(Uuid, String, i64, i64)>, Vec<(Uuid, String, f64, f64)>) {
        // Sane magnitude bounds per key. Anything beyond these is the LLM
        // garbage signature, not a realistic world value.
        //   day:           < 10M (~27k years)
        //   hour:          0..23
        //   actions_today: < 1M
        //   has_history:   0 or 1 (boolean)
        //   is_recording:  0 or 1 (boolean)
        //   last_recorded_day: < 10M
        //   history_entries:   < 1M (count of recorded entries)
        //   everything else:   < 1M (clock has no big counters)
        const MAX_DAY: i64 = 10_000_000;
        const MAX_HOUR: i64 = 23;
        const MAX_ACTIONS_TODAY: i64 = 1_000_000;
        const MAX_BOOL: i64 = 1;
        const MAX_GENERAL: i64 = 1_000_000;

        // Float bounds. `total_years` is a *time* counter that grows
        // without bound (1 real minute = 1 game year, observed running
        // worlds at 2.1M years after a few real days). The cap is set
        // high (1e15) to be permissive for legitimate long-running
        // worlds while still rejecting the LLM "1e18 / -INF" garbage
        // signature. Other float keys fall back to MAX_GENERAL_FLOAT
        // (1e7) as a defensive default — we don't expect any other
        // float on the clock, but a stray key shouldn't survive
        // unrepaired.
        const MAX_TOTAL_YEARS: f64 = 1.0e15;
        const MAX_GENERAL_FLOAT: f64 = 1.0e7;

        let mut int_repairs: Vec<(Uuid, String, i64, i64)> = Vec::new();
        let mut float_repairs: Vec<(Uuid, String, f64, f64)> = Vec::new();

        // Collect (id, key, old_val) first to avoid borrowck issues while
        // mutating the entity map during the loop.
        let mut int_to_fix: Vec<(Uuid, String, i64)> = Vec::new();
        let mut float_to_fix: Vec<(Uuid, String, f64)> = Vec::new();
        for (id, entity) in &self.entities {
            if !entity.is_system_entity() {
                continue;
            }
            for (key, val) in &entity.properties_int {
                let sane_max = match key.as_str() {
                    "day" => MAX_DAY,
                    "hour" => MAX_HOUR,
                    "actions_today" => MAX_ACTIONS_TODAY,
                    "has_history" | "is_recording" => MAX_BOOL,
                    "last_recorded_day" | "history_entries" => MAX_GENERAL,
                    _ => MAX_GENERAL,
                };
                // Use unsigned magnitude to avoid i64::MIN.abs() overflow.
                let magnitude = val.unsigned_abs();
                if magnitude > sane_max as u64 {
                    int_to_fix.push((*id, key.clone(), *val));
                }
            }
            for (key, val) in &entity.properties_float {
                // Non-finite (NaN / Inf) is always garbage.
                if !val.is_finite() {
                    float_to_fix.push((*id, key.clone(), *val));
                    continue;
                }
                let sane_max: f64 = match key.as_str() {
                    "total_years" => MAX_TOTAL_YEARS,
                    _ => MAX_GENERAL_FLOAT,
                };
                if val.abs() >= sane_max {
                    float_to_fix.push((*id, key.clone(), *val));
                }
            }
        }

        for (id, key, old_val) in int_to_fix {
            let new_val: i64 = match key.as_str() {
                "day" => self.world_time.day as i64,
                "hour" => self.world_time.hour as i64,
                "actions_today" => self.world_time.actions_today as i64,
                _ => 0,
            };
            if let Some(entity) = self.entities.get_mut(&id) {
                entity.properties_int.insert(key.clone(), new_val);
            }
            int_repairs.push((id, key, old_val, new_val));
        }

        for (id, key, old_val) in float_to_fix {
            // Re-derive the cap here so the clamp matches the per-key
            // bound used in the detection loop above. This is the
            // same shape as the non-system sanitizer: clamp to the
            // per-key sane_max with the original sign, so a -1e18
            // total_years becomes -1e15 (not 0.0) and preserves the
            // "negative" semantic for any operator reading it.
            let sane_max: f64 = match key.as_str() {
                "total_years" => MAX_TOTAL_YEARS,
                _ => MAX_GENERAL_FLOAT,
            };
            let new_val = if !old_val.is_finite() {
                0.0
            } else {
                old_val.signum() * sane_max
            };
            if let Some(entity) = self.entities.get_mut(&id) {
                entity.properties_float.insert(key.clone(), new_val);
            }
            float_repairs.push((id, key, old_val, new_val));
        }

        (int_repairs, float_repairs)
    }

    /// Sanitize int AND float properties on non-system entities.
    ///
    /// Background: `c7f3bc27` blocks LLM effect writes against system
    /// entities, and `sanitize_system_entities` cleans up the persisted
    /// garbage on the world_clock. But the same LLM garbage pattern
    /// happens on *normal* entities too — observed in the wild: a dragon
    /// entity whose `power` field is `-4.05e18` and whose `shadow_reach`
    /// float is `8.03e19` (well beyond any sane magnitude for those
    /// keys). The entity was created and fed garbage values before
    /// `c7f3bc27` shipped, and the magnitude guard only rejects NEW
    /// writes — the old corruption is still on disk and gets re-saved
    /// on every cycle.
    ///
    /// This method is the non-system counterpart to
    /// `sanitize_system_entities`. It applies sane per-key magnitude
    /// bounds (with a conservative `MAX_GENERAL` default of 1M for
    /// ints and `MAX_GENERAL_FLOAT` of 1e7 for floats — anything beyond
    /// is almost certainly the LLM garbage signature, not a realistic
    /// world value). The bounds per key are:
    ///
    ///   * `actions_today`, `has_history`, `is_recording`,
    ///     `history_entries`, `last_recorded_day`, `power`, `reputation`,
    ///     `wealth`, `knowledge`, `visibility`, `divine_favor`,
    ///     `allies`, `enemies`, `troops`, `mana`, `gold`, `troop_count`,
    ///     `danger_level`, `corruption`, `army_size`, `political_power`,
    ///     `loyalty`, `tithe`, `population`, `garrison`, `supplies`,
    ///     `curse_strength`, `blessing`, `piety`, `fervor`, `evil`,
    ///     `good`, `law`, `chaos`, `strength`, `dexterity`,
    ///     `constitution`, `intelligence`, `wisdom`, `charisma`,
    ///     `level`, `experience`, `hp`, `mp`, `armor_class`, `speed`,
    ///     `max_hp`, `max_mp`, `attack`, `defense`, `magic`, `skill`,
    ///     `rank`, `age`, `age_years`, `hunt_score`, `harvest`,
    ///     `bandit_power`, `spell_power`, `health`, `energy`, `focus`,
    ///     `morale`, `mood`, `inspiration`, `discipline`, `courage`,
    ///     `patience`, `rage`, `suspicion`, `awakening_count`,
    ///     `dominion`, `command`, `reach`, `ambition`,
    ///     `*-power` (e.g. `Shadow Ridge Camp.power`): < 1M
    ///   * Boolean keys (`awakening`, `recording`): 0 or 1
    ///   * Everything else: < 1M (matches the default)
    ///
    ///   * Float bounds: spatial/scale keys (`location_x`, `location_y`,
    ///     `influence_radius`, `shadow_reach`, `view_radius`,
    ///     `territory_radius`, `patrol_radius`, `search_radius`,
    ///     `spell_range`, `view_range`, `hearing_radius`) use
    ///     `MAX_SPATIAL_FLOAT` (1e6) — anything in the millions is the
    ///     LLM "1e7 garbage" signature seen in prod. Other float keys
    ///     use `MAX_GENERAL_FLOAT` (1e7) as a catch-all. The clamp
    ///     preserves the original sign.
    ///
    /// Returns `(entity_id, key, old_value, new_value)` for every int
    /// repair and `(entity_id, key, old_value, new_value)` (f64-typed)
    /// for every float repair. The two lists are kept separate so
    /// callers can log them in distinct streams. System entities are
    /// left alone (use `sanitize_system_entities` for those).
    pub fn sanitize_non_system_entity_properties(
        &mut self,
    ) -> (Vec<(Uuid, String, i64, i64)>, Vec<(Uuid, String, f64, f64)>) {
        // Conservative per-key bound. The keys listed are the ones we've
        // seen the LLM write to non-system entities in /api/entities. The
        // `_` arm catches anything else (e.g. a future key) with the same
        // 1M ceiling — anything beyond that is almost certainly LLM
        // garbage, not a meaningful world value.
        const MAX_BOOL: i64 = 1;
        const MAX_GENERAL: i64 = 1_000_000;
        // Float caps:
        //   * `MAX_SPATIAL_FLOAT` (1e6) is the bound for spatial/scale floats
        //     — location_x, location_y, influence_radius, shadow_reach,
        //     view_radius, territory_radius, etc. These are game-world
        //     coordinates or distances; values in the millions are the LLM
        //     "1e7 garbage" signature seen in prod (e.g. dragon.location_x
        //     pinned at 10,000,000.0 from repeated LLM writes).
        //   * `MAX_GENERAL_FLOAT` (1e7) is the catch-all for any other
        //     float key — 10M is still suspicious for non-spatial floats
        //     (mana, piety, etc.) and worth flagging, but more permissive
        //     than the spatial cap.
        const MAX_SPATIAL_FLOAT: f64 = 1.0e6;
        const MAX_GENERAL_FLOAT: f64 = 1.0e7;

        let mut int_repairs: Vec<(Uuid, String, i64, i64)> = Vec::new();
        let mut float_repairs: Vec<(Uuid, String, f64, f64)> = Vec::new();

        // Collect first to avoid borrowck issues.
        let mut int_to_fix: Vec<(Uuid, String, i64)> = Vec::new();
        let mut float_to_fix: Vec<(Uuid, String, f64)> = Vec::new();
        for (id, entity) in &self.entities {
            if entity.is_system_entity() {
                continue; // leave system entities to sanitize_system_entities
            }
            for (key, val) in &entity.properties_int {
                let sane_max: i64 = match key.as_str() {
                    "awakening" | "is_recording" | "has_history" | "recording" => MAX_BOOL,
                    _ => MAX_GENERAL,
                };
                let magnitude = val.unsigned_abs();
                if magnitude > sane_max as u64 {
                    int_to_fix.push((*id, key.clone(), *val));
                }
            }
            for (key, val) in &entity.properties_float {
                if !val.is_finite() {
                    float_to_fix.push((*id, key.clone(), *val));
                    continue;
                }
                // Per-key spatial cap: location/radius/reach floats get
                // a tighter 1e6 bound so the LLM "1e7 garbage" signature
                // (dragon.location_x = 10_000_000.0, artifact.influence_radius
                // = 8_508_917.82 observed in prod) gets actually repaired
                // on load. The check uses `>=` so boundary values are
                // flagged too (so a future regression to 1e7 is visible
                // in the repair list, not silently preserved).
                let sane_max: f64 = match key.as_str() {
                    "location_x" | "location_y"
                    | "location"
                    | "influence_radius"
                    | "shadow_reach" | "view_radius" | "territory_radius"
                    | "patrol_radius" | "search_radius" | "spell_range"
                    | "view_range" | "hearing_radius" => MAX_SPATIAL_FLOAT,
                    _ => MAX_GENERAL_FLOAT,
                };
                if val.abs() >= sane_max {
                    float_to_fix.push((*id, key.clone(), *val));
                }
            }
        }

        for (id, key, old_val) in int_to_fix {
            if let Some(entity) = self.entities.get_mut(&id) {
                entity.properties_int.insert(key.clone(), 0);
            }
            int_repairs.push((id, key, old_val, 0));
        }
        for (id, key, old_val) in float_to_fix {
            // Re-derive the cap here so the clamp matches the per-key
            // bound used in the detection loop above.
            let sane_max: f64 = match key.as_str() {
                "location_x" | "location_y"
                | "location"
                | "influence_radius"
                | "shadow_reach" | "view_radius" | "territory_radius"
                | "patrol_radius" | "search_radius" | "spell_range"
                | "view_range" | "hearing_radius" => MAX_SPATIAL_FLOAT,
                _ => MAX_GENERAL_FLOAT,
            };
            let new_val = if !old_val.is_finite() {
                0.0
            } else {
                // Clamp to the per-key sane_max with the original sign.
                // A shadow_reach of -8e19 should become a clamped -1e6,
                // not 0, so the agent's "negative reach" semantic is
                // preserved while the magnitude is tamed.
                old_val.signum() * sane_max
            };
            if let Some(entity) = self.entities.get_mut(&id) {
                entity.properties_float.insert(key.clone(), new_val);
            }
            float_repairs.push((id, key, old_val, new_val));
        }

        (int_repairs, float_repairs)
    }

    /// Add an entity to the world
    pub fn add_entity(&mut self, entity: WorldEntity) -> Uuid {
        let id = entity.id;
        self.entities.insert(id, entity);
        id
    }
    
    /// Remove an entity
    pub fn remove_entity(&mut self, id: &Uuid) -> Option<WorldEntity> {
        // Also remove from any owner's owned list
        for entity in self.entities.values_mut() {
            entity.owned_entities.retain(|e| e != id);
        }
        self.entities.remove(id)
    }
    
    /// Get an entity by ID
    pub fn get_entity(&self, id: &Uuid) -> Option<&WorldEntity> {
        self.entities.get(id)
    }
    
    /// Get mutable entity
    pub fn get_entity_mut(&mut self, id: &Uuid) -> Option<&mut WorldEntity> {
        self.entities.get_mut(id)
    }
    
    /// Get all entities of a specific type
    pub fn get_entities_by_type(&self, entity_type: &str) -> Vec<&WorldEntity> {
        self.entities.values()
            .filter(|e| e.entity_type == entity_type)
            .collect()
    }
    
    /// Get entities filtered by tags (AND logic)
    pub fn get_entities_with_tags(&self, tags: &[String]) -> Vec<&WorldEntity> {
        self.entities.values()
            .filter(|e| tags.iter().all(|t| e.has_tag(t)))
            .collect()
    }
    
    /// Get entities filtered by tags (OR logic)
    pub fn get_entities_with_any_tag(&self, tags: &[String]) -> Vec<&WorldEntity> {
        self.entities.values()
            .filter(|e| tags.iter().any(|t| e.has_tag(t)))
            .collect()
    }
    
    /// Search entities by name (case-insensitive partial match)
    pub fn search_by_name(&self, query: &str) -> Vec<&WorldEntity> {
        let query_lower = query.to_lowercase();
        self.entities.values()
            .filter(|e| e.name.to_lowercase().contains(&query_lower))
            .collect()
    }
    
    /// Get entities within a radius
    pub fn get_entities_in_radius(&self, x: f64, y: f64, radius: f64) -> Vec<&WorldEntity> {
        self.entities.values()
            .filter(|e| {
                let dx = e.x - x;
                let dy = e.y - y;
                (dx * dx + dy * dy).sqrt() <= radius
            })
            .collect()
    }
    
    /// Get all entity IDs
    pub fn entity_ids(&self) -> Vec<Uuid> {
        self.entities.keys().cloned().collect()
    }
    
    /// Get entity count
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }
    
    /// Transfer ownership of an entity
    pub fn transfer_ownership(&mut self, entity_id: &Uuid, new_owner_id: &Uuid) -> bool {
        // Remove from old owner
        if let Some(old_owner_id) = self.get_entity(entity_id).and_then(|e| e.owner_id) {
            if let Some(old_owner) = self.entities.get_mut(&old_owner_id) {
                old_owner.owned_entities.retain(|e| e != entity_id);
            }
        }
        
        // Add to new owner
        if let Some(entity) = self.entities.get_mut(entity_id) {
            entity.owner_id = Some(*new_owner_id);
            if let Some(new_owner) = self.entities.get_mut(new_owner_id) {
                if !new_owner.owned_entities.contains(entity_id) {
                    new_owner.owned_entities.push(*entity_id);
                }
            }
            true
        } else {
            false
        }
    }
    
    /// Add a path between two entities
    pub fn add_path(&mut self, from_id: Uuid, to_id: Uuid, path_type: &str) -> Option<Path> {
        // Verify both entities exist
        if !self.entities.contains_key(&from_id) || !self.entities.contains_key(&to_id) {
            return None;
        }
        
        let path = Path {
            id: Uuid::new_v4(),
            from_id,
            to_id,
            blocked: false,
            blocked_reason: None,
            distance_modifier: 1.0,
            path_type: path_type.to_string(),
        };
        
        self.paths.push(path.clone());
        Some(path)
    }
    
    /// Find path between two entities
    pub fn find_path(&self, from_id: &Uuid, to_id: &Uuid) -> Option<&Path> {
        self.paths.iter().find(|p| 
            (p.from_id == *from_id && p.to_id == *to_id) ||
            (p.from_id == *to_id && p.to_id == *from_id)
        )
    }
    
    /// Get paths from an entity
    pub fn get_paths_from(&self, entity_id: &Uuid) -> Vec<&Path> {
        self.paths.iter()
            .filter(|p| p.from_id == *entity_id && !p.blocked)
            .collect()
    }
    
    /// Calculate path distance between two entities (considering paths)
    pub fn path_distance(&self, from_id: &Uuid, to_id: &Uuid) -> Option<f64> {
        let from = self.get_entity(from_id)?;
        let to = self.get_entity(to_id)?;
        
        // Check if direct path exists
        if let Some(path) = self.find_path(from_id, to_id) {
            let direct_dist = from.distance_to(to);
            return Some(direct_dist * path.distance_modifier);
        }
        
        // Otherwise use Euclidean distance
        Some(from.distance_to(to))
    }
    
    /// Select the next entity for action based on action selection score
    /// Formula: unspent_power * unspent_wealth * unspent_mana * time_since_last_action
    /// 
    /// Returns the entity with the highest score, or None if no eligible entities.
    /// Optionally filter by entity types.
    pub fn select_next_entity(&self, entity_types: Option<&[&str]>) -> Option<&WorldEntity> {
        let mut best_entity: Option<&WorldEntity> = None;
        let mut best_score: f64 = f64::MIN;
        
        for entity in self.entities.values() {
            // Filter by entity types if specified
            if let Some(types) = entity_types {
                if !types.contains(&entity.entity_type.as_str()) {
                    continue;
                }
            }
            
            let score = entity.action_selection_score();
            
            if score > best_score {
                best_score = score;
                best_entity = Some(entity);
            }
        }
        
        best_entity
    }
    
    /// Select top N entities for actions, sorted by action selection score
    pub fn select_top_entities(&self, n: usize, entity_types: Option<&[&str]>) -> Vec<&WorldEntity> {
        let mut entities: Vec<&WorldEntity> = self.entities.values()
            .filter(|e| {
                if let Some(types) = entity_types {
                    types.contains(&e.entity_type.as_str())
                } else {
                    true
                }
            })
            .collect();
        
        // Sort by action selection score (descending)
        entities.sort_by(|a, b| {
            let score_a = b.action_selection_score();
            let score_b = a.action_selection_score();
            score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
        });
        
        entities.into_iter().take(n).collect()
    }
}

impl Path {
    /// Create a new path
    pub fn new(from_id: Uuid, to_id: Uuid, path_type: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            from_id,
            to_id,
            blocked: false,
            blocked_reason: None,
            distance_modifier: 1.0,
            path_type: path_type.to_string(),
        }
    }
}

/// World settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSettings {
    /// Number of world actions per game year (tick-based: 1 year = 1 real minute)
    /// With 20% allocation of 4500 actions per 5h budget, that's ~3 actions per year
    pub actions_per_year: u32,
    /// Whether tick-based action processing is enabled
    pub tick_action_enabled: bool,
    /// Base time multiplier for entity selection (hours since last action)
    pub time_weight_factor: f64,
    /// Proximity weight factor
    pub proximity_weight_factor: f64,
    /// Power weight factor
    pub power_weight_factor: f64,
    /// Mana/wealth weight factor
    pub resource_weight_factor: f64,
    /// Auto-save interval in seconds
    pub auto_save_interval_secs: u64,
    /// History entries to display fully in LLM context
    /// (also the LLM's anti-repetition window — the LLM sees this many
    ///  most-recent actions and is told not to pick one semantically
    ///  the same as any of them).
    pub history_entries_fully_displayed: u32,
    /// History entries to show in shortened form (beyond the
    /// fully-displayed window, these appear as a brief one-liner).
    pub history_entries_shortened: u32,
    /// Max characters the LLM is allowed to use when writing the
    /// per-entity history_summary field. Soft cap — the server
    /// truncates with "…" if the LLM goes over.
    /// **0 means "use the global default"** (from
    /// `settings.json → llm.default_max_history_summary_chars`,
    /// default 2000). Set to a positive integer to override
    /// per-world.
    pub max_history_summary_chars: u32,
}

impl Default for WorldSettings {
    fn default() -> Self {
        Self {
            actions_per_year: 3,  // ~900 actions per 5h (20% of 4500 budget)
            tick_action_enabled: false,  // Disabled by default until fully implemented
            time_weight_factor: 1.0,
            proximity_weight_factor: 1.0,
            power_weight_factor: 1.0,
            resource_weight_factor: 1.0,
            auto_save_interval_secs: 300,
            history_entries_fully_displayed: 10,  // bumped 5→10 for anti-repetition window
            history_entries_shortened: 10,
            max_history_summary_chars: 0,  // 0 = use global default from settings.json (currently 2000)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_create_world() {
        let world = World::new("Test World");
        assert_eq!(world.name, "Test World");
        // World::new() always creates the world clock entity, so a
        // fresh world has 1 entity by default. (This used to assert
        // 0 — that was a pre-existing bug; the clock has been there
        // since World::create_clock_entity was introduced.)
        assert_eq!(world.entity_count(), 1);
        // New worlds auto-bootstrap with the canonical lore events
        // (todo e4cc4203). Exactly six, all active: five "Shadow
        // Awakening" doom-arc events + the counterbalancing
        // "Spring Festival of Renewal" hope-arc event.
        assert_eq!(world.active_events.len(), 6);
        assert!(world.active_events.iter().all(|e| e.active));
        let names: Vec<&str> = world
            .active_events
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"The Shadow Awakens"));
        assert!(names.contains(&"Velora Walks Again"));
        assert!(names.contains(&"The Shadowmaw Stirs"));
        assert!(names.contains(&"The Silver Wardens Mobilize"));
        assert!(names.contains(&"The Bells of the Sunken Temple"));
        assert!(names.contains(&"The Spring Festival of Renewal"));
    }

    #[test]
    fn test_seed_default_events_is_idempotent() {
        let mut world = World::new("Test");
        let initial_count = world.active_events.len();
        assert!(initial_count > 0, "World::new should seed defaults");
        // Calling again must not duplicate.
        world.seed_default_events();
        assert_eq!(world.active_events.len(), initial_count);
    }

    #[test]
    fn test_seed_default_events_respects_existing_events() {
        let mut world = World::new("Test");
        // User-added custom event after construction.
        world.active_events.push(WorldEvent {
            id: "11111111-1111-1111-1111-111111111111".to_string(),
            name: "Custom Plot".to_string(),
            description: "Something the players started".to_string(),
            influence: "Adventurers feel bold".to_string(),
            active: true,
        });
        let count_before = world.active_events.len();
        world.seed_default_events();
        assert_eq!(
            world.active_events.len(),
            count_before,
            "seed_default_events must not touch worlds that already have events"
        );
        // Custom event preserved.
        assert!(world
            .active_events
            .iter()
            .any(|e| e.name == "Custom Plot"));
    }

    #[test]
    fn test_default_events_have_unique_ids_and_are_active() {
        let events = default_world_events();
        assert!(!events.is_empty());
        let mut ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        let original_len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), original_len, "all event ids must be unique");
        assert!(events.iter().all(|e| e.active));
        // Every event has a non-empty name + description + influence,
        // otherwise the LLM context builder has nothing to render.
        for e in &events {
            assert!(!e.name.is_empty(), "event name is empty for {}", e.id);
            assert!(
                !e.description.is_empty(),
                "event description is empty for {}",
                e.id
            );
            assert!(
                !e.influence.is_empty(),
                "event influence is empty for {}",
                e.id
            );
        }
    }

    #[test]
    fn test_add_remove_entity() {
        let mut world = World::new("Test");
        // World::new() ships with the world clock entity (count 1).
        assert_eq!(world.entity_count(), 1);
        let entity = WorldEntity::new("location", "Village", 0.0, 0.0);
        let id = world.add_entity(entity);

        assert_eq!(world.entity_count(), 2);
        assert!(world.get_entity(&id).is_some());

        world.remove_entity(&id);
        // Clock remains after removing the village.
        assert_eq!(world.entity_count(), 1);
    }
    
    #[test]
    fn test_search_by_name() {
        let mut world = World::new("Test");
        world.add_entity(WorldEntity::new("location", "Oak Village", 0.0, 0.0));
        world.add_entity(WorldEntity::new("location", "Pine Village", 1.0, 0.0));
        world.add_entity(WorldEntity::new("character", "Oak Guard", 0.0, 0.0));
        
        let results = world.search_by_name("village");
        assert_eq!(results.len(), 2);
    }
    
    #[test]
    fn test_ownership_transfer() {
        let mut world = World::new("Test");
        let king = WorldEntity::new("character", "King", 0.0, 0.0);
        let village = WorldEntity::new("location", "Village", 1.0, 0.0);

        let king_id = world.add_entity(king);
        let village_id = world.add_entity(village);

        world.transfer_ownership(&village_id, &king_id);

        assert!(world.get_entity(&village_id).unwrap().owner_id == Some(king_id));
        assert!(world.get_entity(&king_id).unwrap().owned_entities.contains(&village_id));
    }

    #[test]
    fn test_sanitize_system_entities_repairs_garbage_on_clock() {
        // Reproduces the c7f3bc27 / 2df49bd8 scenario: a world_clock entity
        // with int properties that have the LLM-garbage signature (values
        // wildly outside the sane magnitude bounds). The sanitizer must
        // detect and reset them.
        let mut world = World::new("Test");
        let clock_id = clock_entity_id();
        // Wreck the clock with the exact kind of garbage observed in prod.
        // (Some values overflow i64 — use i64::MAX/MIN to express the
        // "definitely LLM garbage" magnitude in the i64 type.)
        {
            let clock = world.entities.get_mut(&clock_id).unwrap();
            clock.set_int("day", 2_520_157_029);
            clock.set_int("hour", i64::MAX);
            clock.set_int("actions_today", i64::MIN);
            clock.set_int("has_history", -4_320_000_000_000_000_000);
            clock.set_int("history_entries", i64::MAX);
            clock.set_int("last_recorded_day", 7_778_455_365_021_577_000);
            clock.set_int("is_recording", 151_918_487);
            clock.set_int("power", i64::MIN);
        }

        // world_time default: day=0, hour=8, actions_today=0.
        let (int_repairs, _float_repairs) = world.sanitize_system_entities();
        assert!(
            int_repairs.len() >= 6,
            "expected the sanitizer to flag the garbage clock props, got {} repairs",
            int_repairs.len()
        );

        let clock = world.entities.get(&clock_id).unwrap();
        // Known clock keys are restored from world_time.
        assert_eq!(clock.get_int("day").unwrap(), 0);
        assert_eq!(clock.get_int("hour").unwrap(), 8);
        assert_eq!(clock.get_int("actions_today").unwrap(), 0);
        // Other system-entity int props are reset to 0.
        assert_eq!(clock.get_int("has_history").unwrap(), 0);
        assert_eq!(clock.get_int("history_entries").unwrap(), 0);
        assert_eq!(clock.get_int("last_recorded_day").unwrap(), 0);
        assert_eq!(clock.get_int("is_recording").unwrap(), 0);
        assert_eq!(clock.get_int("power").unwrap(), 0);
    }

    #[test]
    fn test_sanitize_system_entities_leaves_non_system_entities_alone() {
        // The sanitizer must only touch system entities. A regular
        // character with a big int property should not be modified.
        let mut world = World::new("Test");
        let hero = WorldEntity::new("character", "Garruk the Mighty", 0.0, 0.0);
        let hero_id = world.add_entity(hero);
        world.entities.get_mut(&hero_id).unwrap().set_int("power", 5_000_000_000);

        let (int_repairs, _float_repairs) = world.sanitize_system_entities();
        assert!(int_repairs.is_empty(), "non-system entities must not be sanitized");
        assert_eq!(
            world.entities.get(&hero_id).unwrap().get_int("power").unwrap(),
            5_000_000_000,
            "non-system entity power must be preserved"
        );
    }

    #[test]
    fn test_sanitize_system_entities_preserves_sane_values() {
        // A clock with realistic values must not be touched.
        let mut world = World::new("Test");
        let clock_id = clock_entity_id();
        {
            let clock = world.entities.get_mut(&clock_id).unwrap();
            clock.set_int("day", 1234);
            clock.set_int("hour", 14);
            clock.set_int("actions_today", 42);
            clock.set_int("has_history", 1);
        }

        let (int_repairs, _float_repairs) = world.sanitize_system_entities();
        assert!(int_repairs.is_empty(), "sane clock values must not trigger repairs");

        let clock = world.entities.get(&clock_id).unwrap();
        assert_eq!(clock.get_int("day").unwrap(), 1234);
        assert_eq!(clock.get_int("hour").unwrap(), 14);
        assert_eq!(clock.get_int("actions_today").unwrap(), 42);
        assert_eq!(clock.get_int("has_history").unwrap(), 1);
    }

    #[test]
    fn test_sanitize_system_entities_repairs_garbage_float_total_years() {
        // The world_clock's `total_years` float is a time counter that
        // legitimately grows without bound (1 real minute = 1 game year),
        // but the LLM effect writer occasionally emits the
        // "1e18 / -INF / NaN" garbage signature. The sanitizer must
        // detect NaN/Inf and clamp out-of-bounds values to MAX_TOTAL_YEARS
        // (1e15) with the original sign preserved.
        let mut world = World::new("Test");
        let clock_id = clock_entity_id();
        {
            let clock = world.entities.get_mut(&clock_id).unwrap();
            // legitimate long-running value
            clock.set_float("total_years", 2_100_000.0);
            // out-of-bounds positive
            clock.set_float("shadow_reach", 1.0e18);
            // out-of-bounds negative
            clock.set_float("influence_radius", -1.0e20);
            // NaN
            clock.set_float("view_radius", f64::NAN);
            // +Inf
            clock.set_float("hearing_radius", f64::INFINITY);
            // -Inf
            clock.set_float("spell_range", f64::NEG_INFINITY);
        }

        let (_int_repairs, float_repairs) = world.sanitize_system_entities();
        assert_eq!(
            float_repairs.len(),
            5,
            "expected 5 float repairs (everything except the legitimate 2.1M), got {}",
            float_repairs.len()
        );

        let clock = world.entities.get(&clock_id).unwrap();
        // Legitimate value untouched.
        assert_eq!(clock.get_float("total_years").unwrap(), 2_100_000.0);
        // Out-of-bounds clamped to MAX_TOTAL_YEARS = 1e15 with sign preserved.
        // (shadow_reach is not a known clock key, so it falls under
        // MAX_GENERAL_FLOAT = 1e7 with sign preserved.)
        assert_eq!(clock.get_float("shadow_reach").unwrap(), 1.0e7);
        assert_eq!(clock.get_float("influence_radius").unwrap(), -1.0e7);
        // NaN / ±Inf reset to 0.0.
        assert_eq!(clock.get_float("view_radius").unwrap(), 0.0);
        assert_eq!(clock.get_float("hearing_radius").unwrap(), 0.0);
        assert_eq!(clock.get_float("spell_range").unwrap(), 0.0);
    }

    #[test]
    fn test_sanitize_non_system_repairs_garbage_int_on_character() {
        // A character entity with the LLM-garbage signature on a regular
        // int property. The non-system sanitizer must reset to 0 and
        // report the repair.
        let mut world = World::new("Test");
        let hero = WorldEntity::new("character", "Vaelthrix", 0.0, 0.0);
        let hero_id = world.add_entity(hero);
        world
            .entities
            .get_mut(&hero_id)
            .unwrap()
            .set_int("power", -4_050_000_000_000_000_000);

        let (int_repairs, float_repairs) = world.sanitize_non_system_entity_properties();
        assert!(float_repairs.is_empty());
        assert_eq!(int_repairs.len(), 1);
        let (_id, key, old, new) = &int_repairs[0];
        assert_eq!(key, "power");
        assert_eq!(*old, -4_050_000_000_000_000_000);
        assert_eq!(*new, 0);
        assert_eq!(world.entities.get(&hero_id).unwrap().get_int("power").unwrap(), 0);
    }

    #[test]
    fn test_sanitize_non_system_clamps_garbage_float_preserving_sign() {
        // The non-system sanitizer must clamp out-of-bounds floats to
        // ±MAX_GENERAL_FLOAT (1e7), preserving the original sign so the
        // agent's "negative reach" semantic isn't lost.
        let mut world = World::new("Test");
        let dragon = WorldEntity::new("creature", "Old Dragon", 10.0, 10.0);
        let dragon_id = world.add_entity(dragon);
        world
            .entities
            .get_mut(&dragon_id)
            .unwrap()
            .set_float("shadow_reach", 8.03e19);
        world
            .entities
            .get_mut(&dragon_id)
            .unwrap()
            .set_float("view_radius", -4.5e18);

        let (int_repairs, float_repairs) = world.sanitize_non_system_entity_properties();
        assert!(int_repairs.is_empty());
        assert_eq!(float_repairs.len(), 2);

        // Original sign is preserved, magnitude is clamped to the
        // spatial cap (1e6) since shadow_reach / view_radius are
        // spatial keys.
        let sr = world
            .entities
            .get(&dragon_id)
            .unwrap()
            .get_float("shadow_reach")
            .unwrap();
        assert!(sr > 0.0 && sr <= 1.0e6, "shadow_reach must clamp positive to spatial cap, got {}", sr);
        let vr = world
            .entities
            .get(&dragon_id)
            .unwrap()
            .get_float("view_radius")
            .unwrap();
        assert!(vr < 0.0 && vr >= -1.0e6, "view_radius must clamp negative to spatial cap, got {}", vr);
    }

    #[test]
    fn test_sanitize_non_system_clamps_garbage_spatial_at_one_e_seven_boundary() {
        // The exact prod pattern: a dragon entity whose location_x /
        // location_y / shadow_reach are pinned at 1e7 from repeated LLM
        // writes (the LLM gravitates to 1e7 because that was the
        // previous default cap). The old check used `>` strict, so 1e7
        // slipped through. The new check uses `>=` AND a per-key
        // spatial cap of 1e6, so the boundary is caught and clamped to
        // a sane value.
        let mut world = World::new("Test");
        let dragon = WorldEntity::new("creature", "Vaelthrix the Endless", 0.0, 0.0);
        let dragon_id = world.add_entity(dragon);
        let d = world.entities.get_mut(&dragon_id).unwrap();
        d.set_float("location_x", 1.0e7);
        d.set_float("location_y", 1.0e7);
        d.set_float("shadow_reach", 1.0e7);

        let (_int, float_repairs) = world.sanitize_non_system_entity_properties();
        assert_eq!(
            float_repairs.len(),
            3,
            "all three 1e7 spatial floats should be flagged"
        );

        let d = world.entities.get(&dragon_id).unwrap();
        let lx = d.get_float("location_x").unwrap();
        assert!(
            (1.0e6 - 1.0..=1.0e6).contains(&lx),
            "location_x must clamp to exactly the spatial cap (1e6), got {}",
            lx
        );
        assert_eq!(d.get_float("location_y").unwrap(), 1.0e6);
        assert_eq!(d.get_float("shadow_reach").unwrap(), 1.0e6);
    }

    #[test]
    fn test_sanitize_non_system_clamps_artifacts_eight_million_influence_radius() {
        // The other prod pattern: an artifact with influence_radius
        // around 8.5e6 — well below the old 1e7 default cap, but
        // unrealistic for a "radius" key in a fantasy world. The
        // per-key spatial cap (1e6) catches it.
        let mut world = World::new("Test");
        let crown = WorldEntity::new("artifact", "The Shadow Crown", 0.0, 0.0);
        let crown_id = world.add_entity(crown);
        world
            .entities
            .get_mut(&crown_id)
            .unwrap()
            .set_float("influence_radius", 8_508_917.82);

        let (_int, float_repairs) = world.sanitize_non_system_entity_properties();
        assert_eq!(float_repairs.len(), 1, "8.5e6 influence_radius must be flagged");
        let (_id, key, old, new) = &float_repairs[0];
        assert_eq!(key, "influence_radius");
        assert_eq!(*old, 8_508_917.82);
        assert_eq!(*new, 1.0e6, "must clamp to the spatial cap, not the general 1e7 cap");
    }

    #[test]
    fn test_sanitize_non_system_spatial_cap_does_not_affect_non_spatial_floats() {
        // Non-spatial floats use the general 1e7 cap, NOT the 1e6
        // spatial cap. A mana value of 5e6 is above the spatial cap
        // but below the general cap, so it must be preserved.
        let mut world = World::new("Test");
        let mage = WorldEntity::new("character", "Archmage", 0.0, 0.0);
        let mage_id = world.add_entity(mage);
        let m = world.entities.get_mut(&mage_id).unwrap();
        m.set_float("mana", 5_000_000.0); // 5e6, above spatial but below general
        m.set_float("view_radius", 5_000_000.0); // 5e6 spatial key, must clamp
        m.set_float("piety", 5_000_000.0); // 5e6 non-spatial, must preserve

        let (_int, float_repairs) = world.sanitize_non_system_entity_properties();
        assert_eq!(float_repairs.len(), 1, "only the spatial view_radius should be flagged");
        let (_id, key, _old, _new) = &float_repairs[0];
        assert_eq!(key, "view_radius");

        let m = world.entities.get(&mage_id).unwrap();
        // mana: 5e6 < 1e7 general cap, must be preserved
        assert_eq!(m.get_float("mana").unwrap(), 5_000_000.0);
        // view_radius: clamped to ±1e6 spatial cap
        assert_eq!(m.get_float("view_radius").unwrap(), 1.0e6);
        // piety: 5e6 < 1e7 general cap, must be preserved
        assert_eq!(m.get_float("piety").unwrap(), 5_000_000.0);
    }

    #[test]
    fn test_sanitize_non_system_preserves_normal_location_values() {
        // A character with normal location coordinates (within a few
        // hundred units) must not be touched. This guards against the
        // spatial cap being too tight for the normal game world.
        let mut world = World::new("Test");
        let hero = WorldEntity::new("character", "Wanderer", 0.0, 0.0);
        let hero_id = world.add_entity(hero);
        let h = world.entities.get_mut(&hero_id).unwrap();
        h.set_float("location_x", 487.7);
        h.set_float("location_y", -123.4);
        h.set_float("view_radius", 50.0);
        h.set_float("influence_radius", 12.5);

        let (_int, float_repairs) = world.sanitize_non_system_entity_properties();
        assert!(float_repairs.is_empty(), "normal spatial values must not be touched");
        let h = world.entities.get(&hero_id).unwrap();
        assert_eq!(h.get_float("location_x").unwrap(), 487.7);
        assert_eq!(h.get_float("location_y").unwrap(), -123.4);
        assert_eq!(h.get_float("view_radius").unwrap(), 50.0);
        assert_eq!(h.get_float("influence_radius").unwrap(), 12.5);
    }

    #[test]
    fn test_sanitize_non_system_resets_nan_and_infinity() {
        // Non-finite floats (NaN, +inf, -inf) must be replaced with 0.0 —
        // they would propagate NaN through every subsequent computation.
        let mut world = World::new("Test");
        let ghost = WorldEntity::new("creature", "Ghost", 0.0, 0.0);
        let ghost_id = world.add_entity(ghost);
        let g = world.entities.get_mut(&ghost_id).unwrap();
        g.set_float("presence", f64::NAN);
        g.set_float("corruption", f64::INFINITY);
        g.set_float("purity", f64::NEG_INFINITY);

        let (_int, float_repairs) = world.sanitize_non_system_entity_properties();
        assert_eq!(float_repairs.len(), 3);
        let g = world.entities.get(&ghost_id).unwrap();
        assert_eq!(g.get_float("presence").unwrap(), 0.0);
        assert_eq!(g.get_float("corruption").unwrap(), 0.0);
        assert_eq!(g.get_float("purity").unwrap(), 0.0);
    }

    #[test]
    fn test_sanitize_non_system_preserves_sane_values() {
        // Normal, in-range int and float values on a regular entity must
        // not be touched.
        let mut world = World::new("Test");
        let hero = WorldEntity::new("character", "Kira", 5.0, 5.0);
        let hero_id = world.add_entity(hero);
        let h = world.entities.get_mut(&hero_id).unwrap();
        h.set_int("power", 500);
        h.set_int("army_size", 12_345);
        h.set_float("view_radius", 250.0);
        h.set_float("location_x", 42.5);

        let (int_repairs, float_repairs) = world.sanitize_non_system_entity_properties();
        assert!(int_repairs.is_empty());
        assert!(float_repairs.is_empty());
        let h = world.entities.get(&hero_id).unwrap();
        assert_eq!(h.get_int("power").unwrap(), 500);
        assert_eq!(h.get_int("army_size").unwrap(), 12_345);
        assert_eq!(h.get_float("view_radius").unwrap(), 250.0);
        assert_eq!(h.get_float("location_x").unwrap(), 42.5);
    }

    #[test]
    fn test_sanitize_non_system_skips_system_entities() {
        // The non-system sanitizer must not touch system entities —
        // those are the system sanitizer's job. This is the same
        // division-of-labor as the existing test for the system
        // sanitizer.
        let mut world = World::new("Test");
        let clock_id = clock_entity_id();
        world
            .entities
            .get_mut(&clock_id)
            .unwrap()
            .set_int("day", i64::MAX);
        world
            .entities
            .get_mut(&clock_id)
            .unwrap()
            .set_float("total_years", f64::INFINITY);

        let (int_repairs, float_repairs) = world.sanitize_non_system_entity_properties();
        assert!(
            int_repairs.is_empty() && float_repairs.is_empty(),
            "non-system sanitizer must leave system entities alone"
        );
        // The corruption is still there — it's the system sanitizer's job.
        assert_eq!(
            world.entities.get(&clock_id).unwrap().get_int("day").unwrap(),
            i64::MAX
        );
    }

    // -- action_count + last_world_action (observability) --
    // Regression guard: the world has had `action_count: u64` and
    // `last_world_action: Option<DateTime<Utc>>` fields since the
    // load/save schema was designed, but no code path was ever setting
    // them — both stayed at their defaults (0 / None) forever. The
    // /api/ endpoint always reported `"action_count": 0` and
    // `"last_world_action": null` regardless of activity. process_action
    // now bumps them on every successful response; this test pins
    // the new contract from the data side: the field exists on a
    // fresh world and the typical bump pattern is safe.
    #[test]
    fn test_action_count_defaults_to_zero_on_new_world() {
        let world = World::new("Fresh");
        assert_eq!(world.action_count, 0);
        assert!(world.last_world_action.is_none());
    }

    #[test]
    fn test_action_count_saturates_instead_of_overflowing() {
        // saturating_add guards against a hypothetical u64::MAX wrap.
        let mut world = World::new("Saturate");
        world.action_count = u64::MAX;
        world.action_count = world.action_count.saturating_add(1);
        assert_eq!(world.action_count, u64::MAX);
    }

    #[test]
    fn test_last_world_action_can_be_set_and_read() {
        let mut world = World::new("Ts");
        let now = chrono::Utc::now();
        world.last_world_action = Some(now);
        assert_eq!(world.last_world_action, Some(now));
    }
}
