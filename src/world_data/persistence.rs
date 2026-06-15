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
const VERSION: u32 = 3; // Increment when world format changes
                         // v2 → v3: added max_history_summary_chars to WorldSettings
// Entity format version history (ENTITY_VERSION):
//   1: original entity layout (no faction_id / secret_loyal /
//      home_location / birth_location / leader / region /
//      last_processed_other_tick field — those lived in
//      `properties_int` for the marker, and the rest didn't exist).
//   2 (2026-06-15 per Arcurus #openworld): added seven new top-
//      level fields to `WorldEntity`:
//        - faction_id, faction_secret_loyal_id, home_location_id,
//          birth_location_id, leader_id, region_id (all
//          `Option<Uuid>`)
//        - last_processed_other_tick (i64, the unprocessed-other-
//          actions marker that previously lived in
//          `properties_int["last_processed_other_tick"]`).
//      v1→v2 migration on load: read the marker from
//      `properties_int`, seed the new field. The old key in
//      `properties_int` is KEPT (per Arcurus "let the old ...
//      field in for now just update the code to use the new one.
//      once all works we can delet it"). The code path uses
//      ONLY the new field; the old property is dead data pending
//      a future cleanup pass.
const ENTITY_VERSION: u32 = 2;

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

        // Deserialize world (pass version for backward compatibility handling)
        Self::deserialize_world(&data, &class_name, version)
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

        // Entity format version (for backward compatibility when reading entities)
        Self::write_u32(ENTITY_VERSION, &mut data);

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
        Self::write_u64(world.settings.max_history_summary_chars as u64, &mut data);

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
        // Note: entity version is stored globally in the world header, not per-entity
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

        // --- v2 (2026-06-15): seven new top-level fields ---
        // The order MUST match `read_entity` exactly. Bumping
        // ENTITY_VERSION to 2 means every new save carries these;
        // v1 readers cannot decode this byte stream (their cursor
        // would not match the file layout) and would error out
        // before reaching this point.
        //
        // 1) Hardcoded game-relationship fields: six Option<Uuid>.
        Self::write_option_uuid(entity.faction_id, data);
        Self::write_option_uuid(entity.faction_secret_loyal_id, data);
        Self::write_option_uuid(entity.home_location_id, data);
        Self::write_option_uuid(entity.birth_location_id, data);
        Self::write_option_uuid(entity.leader_id, data);
        Self::write_option_uuid(entity.region_id, data);
        // 2) The marker that moved out of `properties_int`.
        Self::write_i64(entity.last_processed_other_tick, data);
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
    /// version: the world file format version (1 = old, 2+ = new with entity_version field)
    fn deserialize_world(data: &[u8], class_name: &str, version: u32) -> Result<World, String> {
        let mut pos = 0;
        
        let name = Self::read_string(data, &mut pos);
        let entity_count = Self::read_u32(data, &mut pos) as usize;

        // Read entity format version (for backward compatibility)
        // Version 1 saves don't have this field - entity format is implicit v1
        // Version 2+ saves have entity_version field after entity_count
        let entity_version = if version >= 2 {
            Self::read_u32(data, &mut pos)
        } else {
            1 // Default to version 1 for old saves
        };

        let mut entities = HashMap::new();
        for _ in 0..entity_count {
            let id = Self::read_uuid(data, &mut pos);
            let entity = Self::read_entity(data, &mut pos, id, entity_version)?;
            entities.insert(id, entity);
        }

        let path_count = Self::read_u32(data, &mut pos) as usize;
        let mut paths = Vec::new();
        for _ in 0..path_count {
            let path = Self::read_path(data, &mut pos)?;
            paths.push(path);
        }

        use super::WorldSettings;
        // v3 added max_history_summary_chars to WorldSettings. For
        // older saves (v1/v2) we fall back to the default so a
        // downgrade is graceful.
        //
        // The v3 field's semantics have since changed: 0 now means
        // "use the global default from settings.json". A v3 save that
        // predates this change typically stored 500 (the old default).
        // Per Arcurus 2026-06-04: "make sure world uses defaults" — so
        // we deliberately reset existing v3 saves to 0 on load. The
        // bytes are read to keep the cursor in sync with the file
        // layout; the value is then discarded.
        //
        // IMPORTANT: the file layout (write order in serialize_world)
        // writes the other settings fields first and `max_history_
        // summary_chars` LAST. The reads below must mirror that
        // order exactly, or the cursor drifts and subsequent reads
        // return garbage (which is exactly what broke
        // `save_load_roundtrip_preserves_settings` and
        // `save_load_with_paths_roundtrips` before this fix).
        let actions_per_year = Self::read_f64(data, &mut pos) as u32;
        let tick_action_enabled = Self::read_bool(data, &mut pos);
        let time_weight_factor = Self::read_f64(data, &mut pos);
        let proximity_weight_factor = Self::read_f64(data, &mut pos);
        let power_weight_factor = Self::read_f64(data, &mut pos);
        let resource_weight_factor = Self::read_f64(data, &mut pos);
        let auto_save_interval_secs = Self::read_u64(data, &mut pos);
        let history_entries_fully_displayed = Self::read_f64(data, &mut pos) as u32;
        let history_entries_shortened = Self::read_f64(data, &mut pos) as u32;
        let max_history_summary_chars = if version >= 3 {
            let _stored = Self::read_u64(data, &mut pos) as u32;
            0u32  // v3 stored value no longer meaningful; use global default
        } else {
            WorldSettings::default().max_history_summary_chars
        };
        let settings = WorldSettings {
            actions_per_year,
            tick_action_enabled,
            time_weight_factor,
            proximity_weight_factor,
            power_weight_factor,
            resource_weight_factor,
            auto_save_interval_secs,
            history_entries_fully_displayed,
            history_entries_shortened,
            max_history_summary_chars,
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
            action_count_24h: 0, // Recomputed on every /api/ read from action_history.jsonl (rolling 24h window, see count_actions_in_last_24h in main.rs).
            movement_count_today: 0, // Re-seeded from today's log file in main.rs after load (see init_movement_counter_from_log).
            movement_log_date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
            world_time: crate::world_data::time_system::WorldTime::new(),
            active_events: Vec::new(), // Legacy - active_events not in old saves
        };

        Ok(world)
    }

    /// Read entity from binary
    /// entity_version: the entity format version
    ///   1 = original layout (no new top-level fields, marker in
    ///       `properties_int["last_processed_other_tick"]`)
    ///   2+ = adds seven top-level fields at the end of the entity
    ///       record (see the v2 block in `write_entity` for the
    ///       exact byte order).
    fn read_entity(data: &[u8], pos: &mut usize, id: uuid::Uuid, entity_version: u32) -> Result<WorldEntity, String> {
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
            // v2 fields: default to None / 0 here. The actual
            // values are filled in below, after we've finished
            // reading the rest of the entity, so the field order
            // in this struct literal doesn't need to mirror the
            // file layout byte-for-byte (only the read order in
            // the v2 block below does).
            faction_id: None,
            faction_secret_loyal_id: None,
            home_location_id: None,
            birth_location_id: None,
            leader_id: None,
            region_id: None,
            last_processed_other_tick: 0,
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

        // --- v2 (2026-06-15): seven new top-level fields ---
        // Read order MUST match `write_entity` exactly. v1
        // saves (entity_version == 1) do NOT have these
        // bytes, so we skip the reads and use the migration
        // path to fill the field from the legacy
        // `properties_int["last_processed_other_tick"]` key.
        if entity_version >= 2 {
            entity.faction_id = Self::read_option_uuid(data, pos);
            entity.faction_secret_loyal_id = Self::read_option_uuid(data, pos);
            entity.home_location_id = Self::read_option_uuid(data, pos);
            entity.birth_location_id = Self::read_option_uuid(data, pos);
            entity.leader_id = Self::read_option_uuid(data, pos);
            entity.region_id = Self::read_option_uuid(data, pos);
            entity.last_processed_other_tick = Self::read_i64(data, pos);
        } else {
            // v1 → v2 migration: seed the marker field from
            // the legacy `properties_int` key.  The six
            // relationship fields (faction / secret_loyal /
            // home / birth / leader / region) default to
            // None; the operator populates them via the API
            // after the migration runs (or via a one-time
            // `apply_mapping` script in this commit).
            //
            // IMPORTANT: we do NOT remove the old key from
            // `properties_int` here — per Arcurus 2026-06-15
            // ("let the old ... field in for now just update
            // the code to use the new one. once all works
            // we can delet it"), the old key stays on disk
            // for now.  A future cleanup pass will remove it.
            entity.last_processed_other_tick = entity
                .properties_int
                .get("last_processed_other_tick")
                .copied()
                .unwrap_or(0);
        }

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
            // `write_path` pushes a single byte for `blocked`; mirror
            // that here by advancing the cursor after reading it.
            // The previous `data[*pos] == 1` form did not advance
            // `pos`, which made every subsequent path read drift by
            // one byte and eventually blow up in `read_string` (the
            // 1485-of-565 panic in `save_load_with_paths_roundtrips`).
            blocked: Self::read_bool(data, pos),
            blocked_reason: Self::read_option_string(data, pos),
            distance_modifier: Self::read_f64(data, pos),
            path_type: Self::read_string(data, pos),
        })
    }
}

#[cfg(test)]
mod tests {
    //! Tests for BinaryPersistence.
    //!
    //! The persistence module had **zero** tests for its 3 public
    //! functions (save_world, load_world, save_exists) even though
    //! it's the only bridge between the in-memory World and disk.
    //! A regression in this code would silently corrupt or fail to
    //! load saved games.  These tests cover:
    //!
    //!   * Round-trip equality of the fields the binary format
    //!     actually serializes (name, entities, paths, settings,
    //!     properties, action_count, last_world_action).
    //!   * save_exists before/after a save.
    //!   * load_world error paths: missing file, invalid magic,
    //!     unsupported future version.
    //!   * .bak creation when the target save already exists.
    //!
    //! Fields NOT covered by the binary format (description,
    //! active_events, world_time) are intentionally skipped; the
    //! current serializer does not write them and the loader
    //! reinitializes them with defaults.
    use super::*;
    use crate::world_data::world::Path as WorldPath;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    /// Build a unique tmp path for a test save. We use a per-test
    /// suffix so tests can run in parallel (`cargo test`) without
    /// clobbering each other's save files.
    fn tmp_path(suffix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!(
            "ow_persist_test_{}_{}_{}.bin",
            suffix,
            std::process::id(),
            nanos
        ));
        p
    }

    /// Best-effort cleanup so /tmp doesn't fill up after a test run.
    /// Errors are intentionally swallowed — a leaked tmp file is
    /// less interesting than a failing test.
    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let mut bak = path.to_path_buf();
        bak.set_extension("bin.bak");
        let _ = std::fs::remove_file(&bak);
    }

    #[test]
    fn save_load_roundtrip_preserves_name_and_entity_count() {
        // A fresh world has 1 entity (the world clock) and 6
        // active events — see World::new() and the lore-events
        // bootstrap. The binary format doesn't write events, so
        // the loader reinitializes active_events to an empty Vec;
        // it is the CALLER's responsibility (see main.rs load
        // path) to call seed_default_events() after a successful
        // load so the loaded world keeps the canonical narrative
        // context. This test exercises the lower-level loader,
        // which by itself returns 0 events — the post-load seed
        // is verified separately in the test below.
        let world = World::new("Roundtrip World");
        let path_str = tmp_path("name_entities").to_string_lossy().to_string();

        BinaryPersistence::save_world(&world, &path_str)
            .expect("save should succeed for a fresh world");
        let loaded = BinaryPersistence::load_world(&path_str)
            .expect("load should succeed immediately after save");

        assert_eq!(loaded.name, "Roundtrip World");
        assert_eq!(
            loaded.entities.len(),
            world.entities.len(),
            "entity count must survive round-trip"
        );
        // Loader itself yields an empty active_events vec; the
        // service-level load path (main.rs) re-seeds after load.
        assert_eq!(loaded.active_events.len(), 0);
        cleanup(&PathBuf::from(&path_str));
    }

    /// Regression test for the load-path bug observed
    /// 2026-06-07 08:23 CEST: a live world had 18 entities but
    /// 0 active events because the binary save format does not
    /// persist events, and the service-level load path was
    /// dropping the canonical lore seed on every restart. The
    /// fix is to call `seed_default_events()` in the load path;
    /// the test below pins that contract: after a load +
    /// re-seed, the world has the same 6 canonical events that
    /// `World::new()` produces.
    #[test]
    fn load_then_seed_default_events_restores_canonical_lore() {
        let world = World::new("Live World With Eighteen Entities");
        let path_str = tmp_path("reseed").to_string_lossy().to_string();

        BinaryPersistence::save_world(&world, &path_str)
            .expect("save should succeed for a fresh world");
        let mut loaded = BinaryPersistence::load_world(&path_str)
            .expect("load should succeed immediately after save");

        // As documented, the loader returns 0 events.
        assert_eq!(loaded.active_events.len(), 0);

        // The service-level load path calls seed_default_events
        // after a successful load. Doing so must restore the
        // canonical 6 events from World::new() (idempotent: a
        // no-op when the world already has any events).
        loaded.seed_default_events();
        assert_eq!(loaded.active_events.len(), 6);
        assert!(loaded.active_events.iter().all(|e| e.active));
        let names: Vec<&str> = loaded
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

        // Re-seeding must be a no-op (idempotent contract).
        loaded.seed_default_events();
        assert_eq!(loaded.active_events.len(), 6);

        cleanup(&PathBuf::from(&path_str));
    }

    #[test]
    fn save_load_roundtrip_preserves_settings() {
        // Build a world, then mutate every WorldSettings field to a
        // value different from the default, save, load, and assert
        // the loaded world has the mutated values.
        let mut world = World::new("Settings World");
        world.settings.actions_per_year = 42;
        world.settings.tick_action_enabled = true;
        world.settings.time_weight_factor = 1.5;
        world.settings.proximity_weight_factor = 2.5;
        world.settings.power_weight_factor = 3.5;
        world.settings.resource_weight_factor = 4.5;
        world.settings.auto_save_interval_secs = 600;
        world.settings.history_entries_fully_displayed = 12;
        world.settings.history_entries_shortened = 7;
        world.settings.max_history_summary_chars = 2500;

        let path_str = tmp_path("settings").to_string_lossy().to_string();
        let path = PathBuf::from(&path_str);
        BinaryPersistence::save_world(&world, &path_str).expect("save ok");
        let loaded = BinaryPersistence::load_world(&path_str).expect("load ok");

        assert_eq!(loaded.settings.actions_per_year, 42);
        assert!(loaded.settings.tick_action_enabled);
        assert!((loaded.settings.time_weight_factor - 1.5).abs() < 1e-9);
        assert!((loaded.settings.proximity_weight_factor - 2.5).abs() < 1e-9);
        assert!((loaded.settings.power_weight_factor - 3.5).abs() < 1e-9);
        assert!((loaded.settings.resource_weight_factor - 4.5).abs() < 1e-9);
        assert_eq!(loaded.settings.auto_save_interval_secs, 600);
        assert_eq!(loaded.settings.history_entries_fully_displayed, 12);
        assert_eq!(loaded.settings.history_entries_shortened, 7);
        // max_history_summary_chars is deliberately reset to 0 on
        // v3 loads (see deserialize_world comment) — that's the
        // intentional behavior, not a regression.
        assert_eq!(loaded.settings.max_history_summary_chars, 0);
        cleanup(&path);
    }

    #[test]
    fn save_load_roundtrip_preserves_world_properties() {
        // World properties (int/float/string HashMaps) are written
        // by serialize_world; the loader reads them back into the
        // same fields.  This test would catch any silent loss of
        // the property tables.
        let mut world = World::new("Props World");
        world.properties_int.insert("score".to_string(), 999);
        world
            .properties_float
            .insert("latitude".to_string(), 49.47);
        world
            .properties_string
            .insert("motto".to_string(), "Luna renovat".to_string());

        let path_str = tmp_path("props").to_string_lossy().to_string();
        BinaryPersistence::save_world(&world, &path_str).expect("save ok");
        let loaded = BinaryPersistence::load_world(&path_str).expect("load ok");

        assert_eq!(loaded.properties_int.get("score"), Some(&999));
        assert_eq!(
            loaded.properties_float.get("latitude").copied(),
            Some(49.47)
        );
        assert_eq!(
            loaded.properties_string.get("motto").map(|s| s.as_str()),
            Some("Luna renovat")
        );
        let path = PathBuf::from(&path_str);
        cleanup(&path);
    }

    #[test]
    fn save_exists_true_after_save() {
        let world = World::new("Exists World");
        let path_str = tmp_path("exists").to_string_lossy().to_string();

        assert!(
            !BinaryPersistence::save_exists(&path_str),
            "save_exists must be false before any save"
        );

        BinaryPersistence::save_world(&world, &path_str).expect("save ok");
        assert!(
            BinaryPersistence::save_exists(&path_str),
            "save_exists must be true after save_world"
        );
        let path = PathBuf::from(&path_str);
        cleanup(&path);
    }

    #[test]
    fn save_exists_false_for_missing_file() {
        let path_str = tmp_path("missing").to_string_lossy().to_string();
        // Make sure the file is genuinely absent even if a prior
        // run leaked one.
        let _ = std::fs::remove_file(&path_str);
        assert!(!BinaryPersistence::save_exists(&path_str));
    }

    #[test]
    fn load_missing_file_returns_err() {
        let path_str = tmp_path("nope").to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path_str);
        let result = BinaryPersistence::load_world(&path_str);
        assert!(result.is_err(), "loading a missing file must error");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("Failed to open"),
            "expected 'Failed to open' error, got: {}",
            msg
        );
    }

    #[test]
    fn load_invalid_magic_returns_err() {
        // Write 4 bytes that are not the "OWBL" magic, then try to
        // load. The loader must reject the file before reaching
        // the deserializer.
        let path = tmp_path("bad_magic");
        std::fs::write(&path, b"XXXXrest of file content is irrelevant")
            .expect("write ok");
        let path_str = path.to_string_lossy().to_string();
        let result = BinaryPersistence::load_world(&path_str);
        assert!(result.is_err(), "garbage file must not load");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("Invalid file format"),
            "expected 'Invalid file format' error, got: {}",
            msg
        );
        cleanup(&path);
    }

    #[test]
    fn load_unsupported_version_returns_err() {
        // Hand-craft a v3-shaped file but with version=99 in the
        // header.  The loader must reject it as "unsupported".
        let path = tmp_path("bad_version");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"OWBL"); // magic
        bytes.extend_from_slice(&99u32.to_le_bytes()); // version = 99
        std::fs::write(&path, &bytes).expect("write ok");
        let path_str = path.to_string_lossy().to_string();
        let result = BinaryPersistence::load_world(&path_str);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("Unsupported file version"),
            "expected 'Unsupported file version' error, got: {}",
            msg
        );
        cleanup(&path);
    }

    #[test]
    fn save_creates_backup_when_file_exists() {
        // First save creates the save file.  Second save must
        // rename the existing file to .bak before writing the new
        // one.  This is the safety net that lets a player recover
        // from a corrupted save.
        let world1 = World::new("Backup World v1");
        let world2 = World::new("Backup World v2");
        let path = tmp_path("backup");
        let path_str = path.to_string_lossy().to_string();
        let mut bak = path.clone();
        bak.set_extension("bin.bak");

        BinaryPersistence::save_world(&world1, &path_str).expect("first save ok");
        assert!(path.exists(), "first save must create file");
        assert!(
            !bak.exists(),
            "no .bak should exist after the first save"
        );

        BinaryPersistence::save_world(&world2, &path_str)
            .expect("second save ok");
        assert!(path.exists(), "second save must keep the file");
        assert!(bak.exists(), "second save must create .bak");

        // Loading the .bak yields the previous world; loading the
        // main file yields the new world.
        let from_main =
            BinaryPersistence::load_world(&path_str).expect("load main ok");
        let from_bak =
            BinaryPersistence::load_world(&bak.to_string_lossy())
                .expect("load bak ok");
        assert_eq!(from_main.name, "Backup World v2");
        assert_eq!(from_bak.name, "Backup World v1");
        cleanup(&path);
    }

    #[test]
    fn save_does_not_create_backup_for_fresh_path() {
        // The .bak behavior should only kick in when the target
        // file already exists.  A save to a brand-new path must
        // not leave a stray .bak behind.
        let world = World::new("No Backup World");
        let path = tmp_path("no_backup");
        let path_str = path.to_string_lossy().to_string();
        let mut bak = path.clone();
        bak.set_extension("bin.bak");
        let _ = std::fs::remove_file(&bak); // paranoia

        BinaryPersistence::save_world(&world, &path_str).expect("save ok");
        assert!(path.exists());
        assert!(
            !bak.exists(),
            "a fresh-path save must not produce a .bak"
        );
        cleanup(&path);
    }

    #[test]
    fn save_load_with_paths_roundtrips() {
        // The Path struct is a non-trivial nested type (UUID +
        // Option<String> + 2x f64 + 2 UUID endpoints).  Round-trip
        // a world with two paths and check the deserialized path
        // list is structurally equal.
        let mut world = World::new("Pathed World");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        world.paths.push(WorldPath::new(a, b, "road"));
        world.paths.push(WorldPath::new(b, a, "forest"));

        let path_str = tmp_path("paths").to_string_lossy().to_string();
        BinaryPersistence::save_world(&world, &path_str).expect("save ok");
        let loaded = BinaryPersistence::load_world(&path_str).expect("load ok");

        assert_eq!(loaded.paths.len(), 2);
        let types: Vec<&str> = loaded.paths.iter().map(|p| p.path_type.as_str()).collect();
        assert!(types.contains(&"road"));
        assert!(types.contains(&"forest"));
        // Endpoints are preserved too.
        assert!(loaded
            .paths
            .iter()
            .any(|p| p.from_id == a && p.to_id == b && p.path_type == "road"));
        assert!(loaded
            .paths
            .iter()
            .any(|p| p.from_id == b && p.to_id == a && p.path_type == "forest"));
        let path = PathBuf::from(&path_str);
        cleanup(&path);
    }

    #[test]
    fn save_load_with_empty_world_no_paths_no_props() {
        // Edge case: a world that has been constructed but never
        // had anything added to it.  Must still round-trip cleanly.
        let world = World::new("Empty World");
        let path_str = tmp_path("empty").to_string_lossy().to_string();
        BinaryPersistence::save_world(&world, &path_str).expect("save ok");
        let loaded = BinaryPersistence::load_world(&path_str).expect("load ok");

        assert_eq!(loaded.name, "Empty World");
        assert!(loaded.paths.is_empty());
        assert!(loaded.properties_int.is_empty());
        assert!(loaded.properties_float.is_empty());
        assert!(loaded.properties_string.is_empty());
        assert_eq!(loaded.action_count, 0);
        assert!(loaded.last_world_action.is_none());
        let path = PathBuf::from(&path_str);
        cleanup(&path);
    }

    // -------------------------------------------------------------------
    // v1→v2 migration + v2 round-trip tests (2026-06-15).
    // The entity format version was bumped from 1 to 2 to carry
    // the seven new top-level fields (six relationship fields +
    // the marker that moved out of `properties_int`).  These
    // tests pin the migration contract and the v2 round-trip.
    // -------------------------------------------------------------------

    /// Hand-build a v1-shaped binary save file (entity_version
    /// implicit at 1 — no `entity_version` byte in the world
    /// header) with a single entity whose
    /// `properties_int["last_processed_other_tick"]` is set to
    /// a known value.  Loading it must:
    ///   1. Read the entity (entity_version defaults to 1 in
    ///      `deserialize_world` for legacy saves).
    ///   2. Migrate the marker from `properties_int` into the
    ///      new `last_processed_other_tick` field.
    ///   3. KEEP the old `properties_int` key (per Arcurus
    ///      2026-06-15: "let the old ... field in for now
    ///      just update the code to use the new one. once
    ///      all works we can delet it").
    ///   4. Default the six new relationship fields to None.
    fn write_v1_save_with_marker(path: &Path, marker_value: i64) {
        use std::io::Write;
        // World header
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"OWBL");
        bytes.extend_from_slice(&3u32.to_le_bytes()); // world VERSION = 3 (max)
        // class name "World"
        let class = b"World";
        bytes.extend_from_slice(&(class.len() as u32).to_le_bytes());
        bytes.extend_from_slice(class);

        // We'll need a stable test UUID.
        let entity_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

        // Build the entity blob via the live v1 layout: this is
        // what an old save looks like on disk.  We mirror the v1
        // byte order from `serialize_world` exactly, with the
        // marker in `properties_int` (NOT in the new field —
        // there is no new field in v1).
        let mut entity = Vec::new();
        // entity_type
        let t = b"character";
        entity.extend_from_slice(&(t.len() as u32).to_le_bytes());
        entity.extend_from_slice(t);
        // name
        let n = b"Migrated Hero";
        entity.extend_from_slice(&(n.len() as u32).to_le_bytes());
        entity.extend_from_slice(n);
        // description
        let d = b"A hero from a v1 save";
        entity.extend_from_slice(&(d.len() as u32).to_le_bytes());
        entity.extend_from_slice(d);
        // x, y
        entity.extend_from_slice(&1.0f64.to_le_bytes());
        entity.extend_from_slice(&2.0f64.to_le_bytes());
        // properties_int: 1 entry, "last_processed_other_tick" -> marker_value
        entity.extend_from_slice(&1u32.to_le_bytes());
        let k = b"last_processed_other_tick";
        entity.extend_from_slice(&(k.len() as u32).to_le_bytes());
        entity.extend_from_slice(k);
        entity.extend_from_slice(&marker_value.to_le_bytes());
        // properties_float: 0
        entity.extend_from_slice(&0u32.to_le_bytes());
        // properties_string: 0
        entity.extend_from_slice(&0u32.to_le_bytes());
        // tags: 0
        entity.extend_from_slice(&0u32.to_le_bytes());
        // owner: None
        entity.push(0);
        // owned_entities: 0
        entity.extend_from_slice(&0u32.to_le_bytes());
        // history: 0
        entity.extend_from_slice(&0u32.to_le_bytes());
        // history_summary: None
        entity.push(0);
        // last_action_at: None
        entity.push(0);
        // created_at, updated_at
        entity.extend_from_slice(&0i64.to_le_bytes()); // ts seconds
        entity.extend_from_slice(&0u32.to_le_bytes()); // ts nanos
        entity.extend_from_slice(&0i64.to_le_bytes());
        entity.extend_from_slice(&0u32.to_le_bytes());

        // World body
        let mut body = Vec::new();
        // world name
        let wn = b"V1 World";
        body.extend_from_slice(&(wn.len() as u32).to_le_bytes());
        body.extend_from_slice(wn);
        // entity count
        body.extend_from_slice(&1u32.to_le_bytes());
        // entity version field: ALWAYS written by the live
        // serializer (line `Self::write_u32(ENTITY_VERSION, &mut
        // data);` in `serialize_world`).  The current constant
        // is 2, but a v1 save was written when the constant
        // was 1, so the field on disk is `1`.  The deserialize
        // code reads this field unconditionally for any world
        // version >= 2; the WORLD version (not the entity
        // version) is what gates the read.  We write `1` here
        // to simulate a v1-era save.
        body.extend_from_slice(&1u32.to_le_bytes());
        // per-entity: id + entity blob
        body.extend_from_slice(entity_id.as_bytes());
        body.extend_from_slice(&entity);
        // path count
        body.extend_from_slice(&0u32.to_le_bytes());
        // settings (10 f64 / u64 / bool fields, in the v3 layout)
        body.extend_from_slice(&3.0f64.to_le_bytes());     // actions_per_year
        body.push(0);                                       // tick_action_enabled
        body.extend_from_slice(&1.0f64.to_le_bytes());     // time_weight_factor
        body.extend_from_slice(&1.0f64.to_le_bytes());     // proximity_weight_factor
        body.extend_from_slice(&1.0f64.to_le_bytes());     // power_weight_factor
        body.extend_from_slice(&1.0f64.to_le_bytes());     // resource_weight_factor
        body.extend_from_slice(&300u64.to_le_bytes());     // auto_save_interval_secs
        body.extend_from_slice(&10.0f64.to_le_bytes());    // history_entries_fully_displayed
        body.extend_from_slice(&10.0f64.to_le_bytes());    // history_entries_shortened
        body.extend_from_slice(&0u64.to_le_bytes());       // max_history_summary_chars
        // last_world_action: None
        body.push(0);
        // action_count
        body.extend_from_slice(&0u64.to_le_bytes());
        // world properties: all empty
        body.extend_from_slice(&0u32.to_le_bytes()); // int
        body.extend_from_slice(&0u32.to_le_bytes()); // float
        body.extend_from_slice(&0u32.to_le_bytes()); // string

        // World header trailer: data length + data
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);

        let mut f = std::fs::File::create(path).expect("create");
        f.write_all(&bytes).expect("write");
    }

    #[test]
    fn v1_to_v2_migration_seeds_marker_from_properties_int() {
        // The classic v1 save: marker lives in
        // `properties_int["last_processed_other_tick"]`.  On
        // load via the v2 code, the marker must be migrated
        // into the new field.  The old key in `properties_int`
        // is KEPT (per Arcurus 2026-06-15).
        let path = tmp_path("v1_migration");
        let path_str = path.to_string_lossy().to_string();
        write_v1_save_with_marker(&path, 4242);

        let loaded = BinaryPersistence::load_world(&path_str)
            .expect("v1 save must load under the v2 reader");
        assert_eq!(loaded.entities.len(), 1);
        let e = loaded.entities.values().next().unwrap();
        // The marker must have been migrated to the new field.
        assert_eq!(e.last_processed_other_tick, 4242);
        // The old key in `properties_int` is still present
        // (per Arcurus 2026-06-15: "let the old ... field in
        // for now just update the code to use the new one.
        // once all works we can delet it").
        assert_eq!(
            e.properties_int.get("last_processed_other_tick").copied(),
            Some(4242),
            "old key in properties_int must be preserved on v1→v2 migration"
        );
        // The six relationship fields default to None.
        assert!(e.faction_id.is_none());
        assert!(e.faction_secret_loyal_id.is_none());
        assert!(e.home_location_id.is_none());
        assert!(e.birth_location_id.is_none());
        assert!(e.leader_id.is_none());
        assert!(e.region_id.is_none());

        cleanup(&path);
    }

    #[test]
    fn v2_save_load_roundtrips_all_relationship_fields() {
        // Build a v2 entity directly (we use the live save
        // path, which writes v2 because ENTITY_VERSION is 2),
        // set every relationship field, and confirm the
        // round-trip preserves all of them.
        let mut world = World::new("v2 roundtrip");
        let hero_id = uuid::Uuid::new_v4();
        let faction_id = uuid::Uuid::new_v4();
        let region_id = uuid::Uuid::new_v4();
        let home_id = uuid::Uuid::new_v4();
        let birth_id = uuid::Uuid::new_v4();
        let leader_id = uuid::Uuid::new_v4();
        let secret_id = uuid::Uuid::new_v4();

        let mut hero = WorldEntity::new("character", "Kira", 0.0, 0.0);
        hero.id = hero_id;
        hero.faction_id = Some(faction_id);
        hero.faction_secret_loyal_id = Some(secret_id);
        hero.home_location_id = Some(home_id);
        hero.birth_location_id = Some(birth_id);
        hero.leader_id = Some(leader_id);
        hero.region_id = Some(region_id);
        hero.last_processed_other_tick = 7777;
        world.add_entity(hero);

        let path = tmp_path("v2_roundtrip");
        let path_str = path.to_string_lossy().to_string();
        BinaryPersistence::save_world(&world, &path_str).expect("save v2 ok");
        let loaded = BinaryPersistence::load_world(&path_str).expect("load v2 ok");
        let h = loaded
            .entities
            .get(&hero_id)
            .expect("hero must survive v2 round-trip");
        assert_eq!(h.faction_id, Some(faction_id));
        assert_eq!(h.faction_secret_loyal_id, Some(secret_id));
        assert_eq!(h.home_location_id, Some(home_id));
        assert_eq!(h.birth_location_id, Some(birth_id));
        assert_eq!(h.leader_id, Some(leader_id));
        assert_eq!(h.region_id, Some(region_id));
        assert_eq!(h.last_processed_other_tick, 7777);
        cleanup(&path);
    }
}
