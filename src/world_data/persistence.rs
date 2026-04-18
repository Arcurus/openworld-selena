// Binary Persistence Module
// Provides save/load functionality for the world with binary format

use super::{WorldEntity, World, Path, HistoryEntry};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write, BufReader, BufWriter};

/// Binary file format:
/// [4 bytes: magic "OWBL"]
/// [4 bytes: version (u32)]
/// [4 bytes: class name length (u32)]
/// [N bytes: class name]
/// [4 bytes: data length (u32)]
/// [N bytes: binary data]

const MAGIC: &[u8; 4] = b"OWBL"; // Open World Binary
const VERSION: u32 = 1;

pub struct BinaryPersistence;

impl BinaryPersistence {
    /// Save world to binary file
    pub fn save_world(world: &World, path: &str) -> Result<(), String> {
        // Create backup first
        let bak_path = format!("{}.bak", path);
        if std::path::Path::new(path).exists() {
            fs::copy(path, &bak_path).map_err(|e| format!("Failed to create backup: {}", e))?;
        }

        let file = File::create(path).map_err(|e| format!("Failed to create file: {}", e))?;
        let mut writer = BufWriter::new(file);

        // Write magic
        writer.write_all(MAGIC).map_err(|e| e.to_string())?;

        // Write version
        writer.write_all(&VERSION.to_le_bytes()).map_err(|e| e.to_string())?;

        // Write class name
        let class_name = "World";
        let class_bytes = class_name.as_bytes();
        writer.write_all(&(class_bytes.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
        writer.write_all(class_bytes).map_err(|e| e.to_string())?;

        // Serialize world data
        let world_data = Self::serialize_world(world);
        writer.write_all(&(world_data.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
        writer.write_all(&world_data).map_err(|e| e.to_string())?;

        writer.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Load world from binary file
    pub fn load_world(path: &str) -> Result<World, String> {
        let file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
        let mut reader = BufReader::new(file);

        // Read and verify magic
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic).map_err(|e| format!("Failed to read magic: {}", e))?;
        if &magic != MAGIC {
            return Err("Invalid file format - magic bytes don't match".to_string());
        }

        // Read version
        let mut version_bytes = [0u8; 4];
        reader.read_exact(&mut version_bytes).map_err(|e| format!("Failed to read version: {}", e))?;
        let version = u32::from_le_bytes(version_bytes);
        if version > VERSION {
            return Err(format!("Unsupported file version: {}", version));
        }

        // Read class name
        let mut class_len_bytes = [0u8; 4];
        reader.read_exact(&mut class_len_bytes).map_err(|e| format!("Failed to read class name length: {}", e))?;
        let class_len = u32::from_le_bytes(class_len_bytes) as usize;
        let mut class_name = vec![0u8; class_len];
        reader.read_exact(&mut class_name).map_err(|e| format!("Failed to read class name: {}", e))?;
        let class_name = String::from_utf8(class_name).map_err(|e| format!("Invalid class name: {}", e))?;

        // Read data length
        let mut data_len_bytes = [0u8; 4];
        reader.read_exact(&mut data_len_bytes).map_err(|e| format!("Failed to read data length: {}", e))?;
        let data_len = u32::from_le_bytes(data_len_bytes) as usize;

        // Read data
        let mut data = vec![0u8; data_len];
        reader.read_exact(&mut data).map_err(|e| format!("Failed to read data: {}", e))?;

        // Deserialize world
        Self::deserialize_world(&data, &class_name)
    }

    /// Check if save file exists
    pub fn save_exists(path: &str) -> bool {
        std::path::Path::new(path).exists()
    }

    /// Serialize world to binary
    fn serialize_world(world: &World) -> Vec<u8> {
        let mut data = Vec::new();

        // World name
        Self::write_string(&world.name, &mut data);

        // Entity count
        Self::write_u32(world.entities.len() as u32, &mut data);

        // Each entity
        for (id, entity) in &world.entities {
            Self::write_uuid(id, &mut data);
            Self::write_entity(entity, &mut data);
        }

        // Paths
        Self::write_u32(world.paths.len() as u32, &mut data);
        for path in &world.paths {
            Self::write_path(path, &mut data);
        }

        // Settings
        Self::write_f64(world.settings.actions_per_year as f64, &mut data);
        Self::write_bool(world.settings.tick_action_enabled, &mut data);
        Self::write_f64(world.settings.time_weight_factor, &mut data);
        Self::write_f64(world.settings.proximity_weight_factor, &mut data);
        Self::write_f64(world.settings.power_weight_factor, &mut data);
        Self::write_f64(world.settings.resource_weight_factor, &mut data);
        Self::write_u64(world.settings.auto_save_interval_secs as u64, &mut data);
        Self::write_f64(world.settings.history_entries_fully_displayed as f64, &mut data);
        Self::write_f64(world.settings.history_entries_shortened as f64, &mut data);

        // Last world action
        Self::write_option_datetime(world.last_world_action, &mut data);

        // Action count
        Self::write_u64(world.action_count, &mut data);

        // World properties int
        Self::write_hashmap_i64(&world.properties_int, &mut data);

        // World properties float
        Self::write_hashmap_f64(&world.properties_float, &mut data);

        // World properties string
        Self::write_hashmap_string(&world.properties_string, &mut data);

        data
    }

    /// Serialize entity to binary
    fn write_entity(entity: &WorldEntity, data: &mut Vec<u8>) {
        Self::write_string(&entity.entity_type, data);
        Self::write_string(&entity.name, data);
        Self::write_string(&entity.description, data);
        Self::write_f64(entity.x, data);
        Self::write_f64(entity.y, data);

        // Properties int
        Self::write_u32(entity.properties_int.len() as u32, data);
        for (k, v) in &entity.properties_int {
            Self::write_string(k, data);
            Self::write_i64(*v, data);
        }

        // Properties float
        Self::write_u32(entity.properties_float.len() as u32, data);
        for (k, v) in &entity.properties_float {
            Self::write_string(k, data);
            Self::write_f64(*v, data);
        }

        // Properties string
        Self::write_u32(entity.properties_string.len() as u32, data);
        for (k, v) in &entity.properties_string {
            Self::write_string(k, data);
            Self::write_string(v, data);
        }

        // Tags
        Self::write_u32(entity.tags.len() as u32, data);
        for tag in &entity.tags {
            Self::write_string(tag, data);
        }

        // Owner
        Self::write_option_uuid(entity.owner_id, data);

        // Owned entities
        Self::write_u32(entity.owned_entities.len() as u32, data);
        for owned_id in &entity.owned_entities {
            Self::write_uuid(owned_id, data);
        }

        // History
        Self::write_u32(entity.history.len() as u32, data);
        for entry in &entity.history {
            Self::write_string(&entry.action, data);
            Self::write_string(&entry.details, data);
            Self::write_string(&entry.outcome, data);
            Self::write_datetime(&entry.timestamp, data);
        }

        // History summary
        Self::write_option_string(entity.history_summary.clone(), data);

        // Last action at
        Self::write_option_datetime(entity.last_action_at, data);

        // Timestamps
        Self::write_datetime(&entity.created_at, data);
        Self::write_datetime(&entity.updated_at, data);
    }

    /// Write UUID
    fn write_uuid(id: &uuid::Uuid, data: &mut Vec<u8>) {
        data.extend_from_slice(id.as_bytes());
    }

    /// Write option UUID
    fn write_option_uuid(opt: Option<uuid::Uuid>, data: &mut Vec<u8>) {
        match opt {
            Some(id) => {
                data.push(1);
                Self::write_uuid(&id, data);
            }
            None => data.push(0),
        }
    }

    /// Write string (length + bytes)
    fn write_string(s: &str, data: &mut Vec<u8>) {
        let bytes = s.as_bytes();
        Self::write_u32(bytes.len() as u32, data);
        data.extend_from_slice(bytes);
    }

    /// Write option string
    fn write_option_string(opt: Option<String>, data: &mut Vec<u8>) {
        match opt {
            Some(s) => {
                data.push(1);
                Self::write_string(&s, data);
            }
            None => data.push(0),
        }
    }

    /// Write i64
    fn write_i64(val: i64, data: &mut Vec<u8>) {
        data.extend_from_slice(&val.to_le_bytes());
    }

    /// Write u32
    fn write_u32(val: u32, data: &mut Vec<u8>) {
        data.extend_from_slice(&val.to_le_bytes());
    }

    /// Write u64
    fn write_u64(val: u64, data: &mut Vec<u8>) {
        data.extend_from_slice(&val.to_le_bytes());
    }

    /// Write f64
    fn write_f64(val: f64, data: &mut Vec<u8>) {
        data.extend_from_slice(&val.to_le_bytes());
    }

    /// Write bool as single byte (0 or 1)
    fn write_bool(val: bool, data: &mut Vec<u8>) {
        data.push(if val { 1 } else { 0 });
    }

    /// Write DateTime
    fn write_datetime(dt: &chrono::DateTime<chrono::Utc>, data: &mut Vec<u8>) {
        let timestamp = dt.timestamp();
        let nanos = dt.timestamp_subsec_nanos();
        Self::write_i64(timestamp, data);
        Self::write_u32(nanos, data);
    }

    /// Write option DateTime
    fn write_option_datetime(opt: Option<chrono::DateTime<chrono::Utc>>, data: &mut Vec<u8>) {
        match opt {
            Some(dt) => {
                data.push(1);
                Self::write_datetime(&dt, data);
            }
            None => data.push(0),
        }
    }

    /// Write path
    fn write_path(path: &Path, data: &mut Vec<u8>) {
        Self::write_uuid(&path.id, data);
        Self::write_uuid(&path.from_id, data);
        Self::write_uuid(&path.to_id, data);
        data.push(if path.blocked { 1 } else { 0 });
        Self::write_option_string(path.blocked_reason.clone(), data);
        Self::write_f64(path.distance_modifier, data);
        Self::write_string(&path.path_type, data);
    }

    fn write_hashmap_i64(map: &std::collections::HashMap<String, i64>, data: &mut Vec<u8>) {
        Self::write_u32(map.len() as u32, data);
        for (key, value) in map {
            Self::write_string(key, data);
            Self::write_i64(*value, data);
        }
    }

    fn write_hashmap_f64(map: &std::collections::HashMap<String, f64>, data: &mut Vec<u8>) {
        Self::write_u32(map.len() as u32, data);
        for (key, value) in map {
            Self::write_string(key, data);
            Self::write_f64(*value, data);
        }
    }

    fn write_hashmap_string(map: &std::collections::HashMap<String, String>, data: &mut Vec<u8>) {
        Self::write_u32(map.len() as u32, data);
        for (key, value) in map {
            Self::write_string(key, data);
            Self::write_string(value, data);
        }
    }

    fn read_hashmap_i64(data: &[u8], pos: &mut usize) -> std::collections::HashMap<String, i64> {
        let len = Self::read_u32(data, pos) as usize;
        let mut map = std::collections::HashMap::new();
        for _ in 0..len {
            let key = Self::read_string(data, pos);
            let value = Self::read_i64(data, pos);
            map.insert(key, value);
        }
        map
    }

    fn read_hashmap_f64(data: &[u8], pos: &mut usize) -> std::collections::HashMap<String, f64> {
        let len = Self::read_u32(data, pos) as usize;
        let mut map = std::collections::HashMap::new();
        for _ in 0..len {
            let key = Self::read_string(data, pos);
            let value = Self::read_f64(data, pos);
            map.insert(key, value);
        }
        map
    }

    fn read_hashmap_string(data: &[u8], pos: &mut usize) -> std::collections::HashMap<String, String> {
        let len = Self::read_u32(data, pos) as usize;
        let mut map = std::collections::HashMap::new();
        for _ in 0..len {
            let key = Self::read_string(data, pos);
            let value = Self::read_string(data, pos);
            map.insert(key, value);
        }
        map
    }

    /// Deserialize world from binary
    fn deserialize_world(data: &[u8], class_name: &str) -> Result<World, String> {
        let mut pos = 0;
        
        let name = Self::read_string(data, &mut pos);
        let entity_count = Self::read_u32(data, &mut pos) as usize;

        let mut entities = HashMap::new();
        for _ in 0..entity_count {
            let id = Self::read_uuid(data, &mut pos);
            let entity = Self::read_entity(data, &mut pos, id)?;
            entities.insert(id, entity);
        }

        let path_count = Self::read_u32(data, &mut pos) as usize;
        let mut paths = Vec::new();
        for _ in 0..path_count {
            let path = Self::read_path(data, &mut pos)?;
            paths.push(path);
        }

        use super::WorldSettings;
        let settings = WorldSettings {
            actions_per_year: Self::read_f64(data, &mut pos) as u32,
            tick_action_enabled: Self::read_bool(data, &mut pos),
            time_weight_factor: Self::read_f64(data, &mut pos),
            proximity_weight_factor: Self::read_f64(data, &mut pos),
            power_weight_factor: Self::read_f64(data, &mut pos),
            resource_weight_factor: Self::read_f64(data, &mut pos),
            auto_save_interval_secs: Self::read_u64(data, &mut pos),
            history_entries_fully_displayed: Self::read_f64(data, &mut pos) as u32,
            history_entries_shortened: Self::read_f64(data, &mut pos) as u32,
        };

        let last_world_action = Self::read_option_datetime(data, &mut pos);
        let action_count = Self::read_u64(data, &mut pos);

        // Read world properties (for backwards compatibility, default if not present)
        let properties_int = Self::read_hashmap_i64(data, &mut pos);
        let properties_float = Self::read_hashmap_f64(data, &mut pos);
        let properties_string = Self::read_hashmap_string(data, &mut pos);

        let mut world = World {
            name,
            description: String::new(), // Legacy - description not in old saves
            entities,
            paths,
            settings,
            properties_int,
            properties_float,
            properties_string,
            last_world_action,
            action_count,
            world_time: crate::world_data::time_system::WorldTime::new(),
        };

        Ok(world)
    }

    /// Read entity from binary
    fn read_entity(data: &[u8], pos: &mut usize, id: uuid::Uuid) -> Result<WorldEntity, String> {
        use chrono::Utc;

        let entity_type = Self::read_string(data, pos);
        let name = Self::read_string(data, pos);
        let description = Self::read_string(data, pos);
        let x = Self::read_f64(data, pos);
        let y = Self::read_f64(data, pos);

        let mut entity = WorldEntity {
            id,  // Use the ID passed in instead of creating a new one
            entity_type,
            name,
            description,
            long_description: String::new(),  // Default empty, can be updated later
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
            time_preferences: crate::world_data::time_system::EntityTimePreferences::new(),
        };

        // Properties int
        let int_count = Self::read_u32(data, pos) as usize;
        for _ in 0..int_count {
            let key = Self::read_string(data, pos);
            let val = Self::read_i64(data, pos);
            entity.properties_int.insert(key, val);
        }

        // Properties float
        let float_count = Self::read_u32(data, pos) as usize;
        for _ in 0..float_count {
            let key = Self::read_string(data, pos);
            let val = Self::read_f64(data, pos);
            entity.properties_float.insert(key, val);
        }

        // Properties string
        let string_count = Self::read_u32(data, pos) as usize;
        for _ in 0..string_count {
            let key = Self::read_string(data, pos);
            let val = Self::read_string(data, pos);
            entity.properties_string.insert(key, val);
        }

        // Tags
        let tag_count = Self::read_u32(data, pos) as usize;
        for _ in 0..tag_count {
            entity.tags.push(Self::read_string(data, pos));
        }

        // Owner
        entity.owner_id = Self::read_option_uuid(data, pos);

        // Owned entities
        let owned_count = Self::read_u32(data, pos) as usize;
        for _ in 0..owned_count {
            entity.owned_entities.push(Self::read_uuid(data, pos));
        }

        // History
        let history_count = Self::read_u32(data, pos) as usize;
        for _ in 0..history_count {
            let action = Self::read_string(data, pos);
            let details = Self::read_string(data, pos);
            let outcome = Self::read_string(data, pos);
            let timestamp = Self::read_datetime(data, pos);
            entity.history.push(HistoryEntry {
                timestamp,
                action,
                details,
                outcome,
            });
        }

        // History summary
        entity.history_summary = Self::read_option_string(data, pos);

        // Last action at
        entity.last_action_at = Self::read_option_datetime(data, pos);

        // Timestamps
        entity.created_at = Self::read_datetime(data, pos);
        entity.updated_at = Self::read_datetime(data, pos);

        Ok(entity)
    }

    /// Read UUID
    fn read_uuid(data: &[u8], pos: &mut usize) -> uuid::Uuid {
        let bytes: [u8; 16] = data[*pos..*pos + 16].try_into().unwrap();
        *pos += 16;
        uuid::Uuid::from_bytes(bytes)
    }

    /// Read option UUID
    fn read_option_uuid(data: &[u8], pos: &mut usize) -> Option<uuid::Uuid> {
        let has_value = data[*pos];
        *pos += 1;
        if has_value == 1 {
            Some(Self::read_uuid(data, pos))
        } else {
            None
        }
    }

    /// Read string
    fn read_string(data: &[u8], pos: &mut usize) -> String {
        let len = Self::read_u32(data, pos) as usize;
        let s = String::from_utf8(data[*pos..*pos + len].to_vec()).unwrap_or_default();
        *pos += len;
        s
    }

    /// Read option string
    fn read_option_string(data: &[u8], pos: &mut usize) -> Option<String> {
        let has_value = data[*pos];
        *pos += 1;
        if has_value == 1 {
            Some(Self::read_string(data, pos))
        } else {
            None
        }
    }

    /// Read i64
    fn read_i64(data: &[u8], pos: &mut usize) -> i64 {
        let bytes: [u8; 8] = data[*pos..*pos + 8].try_into().unwrap();
        *pos += 8;
        i64::from_le_bytes(bytes)
    }

    /// Read u32
    fn read_u32(data: &[u8], pos: &mut usize) -> u32 {
        let bytes: [u8; 4] = data[*pos..*pos + 4].try_into().unwrap();
        *pos += 4;
        u32::from_le_bytes(bytes)
    }

    /// Read u64
    fn read_u64(data: &[u8], pos: &mut usize) -> u64 {
        let bytes: [u8; 8] = data[*pos..*pos + 8].try_into().unwrap();
        *pos += 8;
        u64::from_le_bytes(bytes)
    }

    /// Read f64
    fn read_f64(data: &[u8], pos: &mut usize) -> f64 {
        let bytes: [u8; 8] = data[*pos..*pos + 8].try_into().unwrap();
        *pos += 8;
        f64::from_le_bytes(bytes)
    }

    /// Read bool (single byte, 0 = false, 1 = true)
    fn read_bool(data: &[u8], pos: &mut usize) -> bool {
        let val = data[*pos];
        *pos += 1;
        val != 0
    }

    /// Read DateTime
    fn read_datetime(data: &[u8], pos: &mut usize) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        let timestamp = Self::read_i64(data, pos);
        let nanos = Self::read_u32(data, pos);
        chrono::Utc.timestamp_opt(timestamp, nanos).unwrap()
    }

    /// Read option DateTime
    fn read_option_datetime(data: &[u8], pos: &mut usize) -> Option<chrono::DateTime<chrono::Utc>> {
        let has_value = data[*pos];
        *pos += 1;
        if has_value == 1 {
            Some(Self::read_datetime(data, pos))
        } else {
            None
        }
    }

    /// Read path
    fn read_path(data: &[u8], pos: &mut usize) -> Result<Path, String> {
        Ok(Path {
            id: Self::read_uuid(data, pos),
            from_id: Self::read_uuid(data, pos),
            to_id: Self::read_uuid(data, pos),
            blocked: data[*pos] == 1,
            blocked_reason: Self::read_option_string(data, pos),
            distance_modifier: Self::read_f64(data, pos),
            path_type: Self::read_string(data, pos),
        })
    }
}
