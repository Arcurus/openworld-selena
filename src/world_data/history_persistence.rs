//! History Persistence Module
//! 
//! Handles saving and loading of entity histories separately from main entity data.
//! This allows:
//! - Editor to work without loading full history
//! - Smaller main save files
//! - Selective history loading

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write, BufReader, BufWriter};
use std::path::Path;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::world_data::world_entity::{WorldEntity, HistoryEntry};

/// Stores history for all entities, mapped by entity ID
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryStore {
    /// Map from entity ID to their history entries
    histories: HashMap<String, Vec<HistoryEntry>>,
    
    /// Metadata about the history store
    pub metadata: HistoryMetadata,
}

/// Metadata about the history store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMetadata {
    /// Total number of history entries across all entities
    pub total_entries: usize,
    /// Number of entities with history
    pub entities_with_history: usize,
    /// Last time history was updated
    pub last_updated: String,
}

impl Default for HistoryMetadata {
    fn default() -> Self {
        Self {
            total_entries: 0,
            entities_with_history: 0,
            last_updated: chrono::Utc::now().to_rfc3339(),
        }
    }
}

impl HistoryStore {
    /// Create a new empty history store
    pub fn new() -> Self {
        Self {
            histories: HashMap::new(),
            metadata: HistoryMetadata::default(),
        }
    }
    
    /// Create a history store from all entities in a world
    pub fn from_entities(entities: &HashMap<Uuid, WorldEntity>) -> Self {
        let mut store = Self::new();
        
        for (id, entity) in entities {
            if !entity.history.is_empty() {
                store.histories.insert(id.to_string(), entity.history.clone());
            }
        }
        
        store.update_metadata();
        store
    }
    
    /// Update metadata based on current histories
    fn update_metadata(&mut self) {
        self.metadata.total_entries = self.histories.values().map(|v| v.len()).sum::<usize>();
        self.metadata.entities_with_history = self.histories.len();
        self.metadata.last_updated = chrono::Utc::now().to_rfc3339();
    }
    
    /// Get history for a specific entity
    pub fn get_history(&self, entity_id: &Uuid) -> Option<&Vec<HistoryEntry>> {
        self.histories.get(&entity_id.to_string())
    }
    
    /// Get mutable history for a specific entity
    pub fn get_history_mut(&mut self, entity_id: &Uuid) -> Option<&mut Vec<HistoryEntry>> {
        self.histories.get_mut(&entity_id.to_string())
    }
    
    /// Add a history entry to an entity (creates entry if not exists)
    pub fn add_entry(&mut self, entity_id: &Uuid, entry: HistoryEntry) {
        let key = entity_id.to_string();
        // Use entry() API to avoid moving key
        let history = self.histories.entry(key).or_insert_with(Vec::new);
        history.push(entry);
        self.update_metadata();
    }
    
    /// Get the most recent N entries for an entity
    pub fn get_recent(&self, entity_id: &Uuid, count: usize) -> Vec<&HistoryEntry> {
        if let Some(history) = self.get_history(entity_id) {
            history.iter().rev().take(count).collect()
        } else {
            Vec::new()
        }
    }
    
    /// Get all entity IDs that have history
    pub fn entities_with_history(&self) -> Vec<&String> {
        self.histories.keys().collect()
    }
    
    /// Load history store from a file
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::new());
        }
        
        let file = File::open(path).map_err(|e| format!("Failed to open history file: {}", e))?;
        let reader = BufReader::new(file);
        
        serde_json::from_reader(reader)
            .map_err(|e| format!("Failed to parse history file: {}", e))
    }
    
    /// Save history store to a file
    pub fn save_to_file(&self, path: &Path) -> Result<(), String> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| format!("Failed to open history file for writing: {}", e))?;
        
        let writer = BufWriter::new(file);
        
        serde_json::to_writer_pretty(writer, self)
            .map_err(|e| format!("Failed to write history file: {}", e))?;
        
        Ok(())
    }
    
    /// Apply history to entities (merge into existing entities)
    pub fn apply_to_entities(&self, entities: &mut HashMap<Uuid, WorldEntity>) {
        for (id_str, history) in &self.histories {
            if let Ok(id) = Uuid::parse_str(id_str) {
                if let Some(entity) = entities.get_mut(&id) {
                    // Merge history (don't overwrite existing, just append new)
                    let existing_ids: std::collections::HashSet<_> = entity.history.iter()
                        .map(|e| (e.timestamp, e.action.clone()))
                        .collect();
                    
                    for entry in history {
                        let key = (entry.timestamp, entry.action.clone());
                        if !existing_ids.contains(&key) {
                            entity.history.push(entry.clone());
                        }
                    }
                }
            }
        }
    }
    
    /// Extract history from entities and create a history store
    pub fn extract_from_entities(entities: &mut HashMap<Uuid, WorldEntity>) -> Self {
        let mut store = Self::new();
        
        for (id, entity) in entities.iter() {
            if !entity.history.is_empty() {
                // Clear history from entity and store it separately
                store.histories.insert(id.to_string(), entity.history.clone());
            }
        }
        
        // Clear history from entities (they'll be restored from the store)
        for entity in entities.values_mut() {
            entity.history.clear();
        }
        
        store.update_metadata();
        store
    }
    
    /// Get storage statistics
    pub fn stats(&self) -> HistoryStats {
        HistoryStats {
            entities_with_history: self.histories.len(),
            total_entries: self.metadata.total_entries,
            estimated_size_bytes: serde_json::to_string(self)
                .map(|s| s.len())
                .unwrap_or(0),
        }
    }
}

/// Statistics about the history store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryStats {
    pub entities_with_history: usize,
    pub total_entries: usize,
    pub estimated_size_bytes: usize,
}

/// Format history entries for LLM context
pub fn format_history_for_context(
    entries: &[&HistoryEntry],
    fully_displayed: usize,
    shortened: usize,
) -> String {
    let total = entries.len();
    if total == 0 {
        return String::new();
    }
    
    let mut output = String::new();
    
    // Recent entries (fully displayed)
    let start_idx = if total > fully_displayed { total - fully_displayed } else { 0 };
    
    for (i, entry) in entries.iter().enumerate().skip(start_idx) {
        if i < total - shortened {
            continue;
        }
        
        if i >= total - fully_displayed {
            // Full display
            output.push_str(&format!(
                "- [{}] {}: {} → {}\n",
                entry.timestamp.format("%Y-%m-%d"),
                entry.action,
                entry.details,
                entry.outcome
            ));
        } else {
            // Shortened
            output.push_str(&format!(
                "- [{}] {}: {}\n",
                entry.timestamp.format("%Y-%m-%d"),
                entry.action,
                entry.outcome
            ));
        }
    }
    
    if total > fully_displayed + shortened {
        let truncated = total - fully_displayed - shortened;
        output.push_str(&format!("- ... ({} older entries)\n", truncated));
    }
    
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    
    #[test]
    fn test_history_store_new() {
        let store = HistoryStore::new();
        assert_eq!(store.histories.len(), 0);
    }
    
    #[test]
    fn test_add_entry() {
        let mut store = HistoryStore::new();
        let id = Uuid::new_v4();
        let entry = HistoryEntry::new("test_action", "test_details", "test_outcome");
        
        store.add_entry(&id, entry);
        
        assert_eq!(store.histories.len(), 1);
        assert_eq!(store.get_history(&id).unwrap().len(), 1);
    }
    
    #[test]
    fn test_get_recent() {
        let mut store = HistoryStore::new();
        let id = Uuid::new_v4();
        
        for i in 0..10 {
            let entry = HistoryEntry::new(
                &format!("action_{}", i),
                "details",
                "outcome"
            );
            store.add_entry(&id, entry);
        }
        
        let recent = store.get_recent(&id, 3);
        assert_eq!(recent.len(), 3);
    }
}
