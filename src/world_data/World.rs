use crate::WorldEntity;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

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
        Self {
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
    /// Number of world actions per hour
    pub actions_per_hour: u32,
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
}

impl Default for WorldSettings {
    fn default() -> Self {
        Self {
            actions_per_hour: 10,
            time_weight_factor: 1.0,
            proximity_weight_factor: 1.0,
            power_weight_factor: 1.0,
            resource_weight_factor: 1.0,
            auto_save_interval_secs: 300,
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
