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
        world
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
    pub history_entries_fully_displayed: u32,
    /// History entries to show in shortened form
    pub history_entries_shortened: u32,
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
            history_entries_fully_displayed: 5,
            history_entries_shortened: 10,
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
        assert_eq!(world.entity_count(), 0);
    }
    
    #[test]
    fn test_add_remove_entity() {
        let mut world = World::new("Test");
        let entity = WorldEntity::new("location", "Village", 0.0, 0.0);
        let id = world.add_entity(entity);
        
        assert_eq!(world.entity_count(), 1);
        assert!(world.get_entity(&id).is_some());
        
        world.remove_entity(&id);
        assert_eq!(world.entity_count(), 0);
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
}
