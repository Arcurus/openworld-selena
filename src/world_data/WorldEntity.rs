use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
use chrono::{DateTime, Utc};

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
    
    /// Add a tag
    pub fn add_tag(&mut self, tag: &str) {
        if !self.has_tag(tag) {
            self.tags.push(tag.to_string());
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
}
