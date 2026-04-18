# World.rs - World Container and Entity Management

## Purpose

Holds all entities and world state.
Provides methods for entity CRUD, world-level operations, persistence, and action system.

## Core Struct

```rust
World {
    name: String,                              // World name
    entities: HashMap<Uuid, WorldEntity>,      // All entities
    paths: Vec<Path>,                          // Connections between entities
    settings: WorldSettings,                    // World configuration
    last_world_action: Option<DateTime>,       // When action system last ran
    action_count: u64,                         // Total actions performed
}
```

## Paths

Connections between entities for non-Euclidean travel.

```rust
Path {
    id: Uuid,
    from_id: Uuid,                    // First entity
    to_id: Uuid,                      // Second entity
    blocked: bool,                    // Is path traversable
    blocked_reason: Option<String>,   // Why blocked
    distance_modifier: f64,          // Multiplier (1.0 = normal)
    path_type: String,               // "road", "river", "mountain", etc.
}
```

## WorldSettings

```rust
WorldSettings {
    actions_per_hour: u32,           // How often actions occur
    time_weight_factor: f64,          // Time affects selection
    proximity_weight_factor: f64,     // Distance affects selection
    power_weight_factor: f64,         // Power affects selection
    resource_weight_factor: f64,      // Resources affect selection
    auto_save_interval_secs: u64,     // Auto-save interval
}
```

## Entity Management Methods

| Method | Description |
|--------|-------------|
| `add_entity()` / `remove_entity()` | Add/remove entities |
| `get_entity()` / `get_entity_mut()` | Get entity by ID |
| `get_entities_by_type()` | Filter by entity_type |
| `get_entities_with_tags()` | Filter by tags (AND) |
| `get_entities_with_any_tag()` | Filter by tags (OR) |
| `search_by_name()` | Name contains search |
| `get_entities_in_radius()` | Spatial query |
| `entity_ids()` / `entity_count()` | Count/size |
| `transfer_ownership()` | Change parent entity |

## Path Methods

| Method | Description |
|--------|-------------|
| `add_path()` | Connect two entities |
| `find_path()` | Get path between entities |
| `get_paths_from()` | Get paths from entity |
| `path_distance()` | Distance considering paths |

## Persistence (Implemented)

- Binary format (.owbl) with auto-backup (.bak)
- Auto-save after entity updates
- World save/load/restatus API endpoints

## Not Yet Implemented

- [ ] Path-finding algorithms (A*, Dijkstra)
- [ ] Entity spawning system
- [ ] Time simulation (world clock)
- [ ] Event/action scheduling (auto-save is implemented, but automated entity actions are not)
- [ ] Bulk operations
- [ ] Entity groups/clusters
- [ ] Spatial indexing (R-tree, quadtree)
- [ ] World reset/rollback
- [ ] Multi-world support
- [ ] Transaction support

## Future Enhancements

- [ ] Action queue and scheduling
- [ ] Concurrent world updates
- [ ] World templates
- [ ] Import/export functionality
- [ ] Database backend (SQLite/PostgreSQL)
- [ ] Replication for multiplayer
- [ ] Undo/redo system
- [ ] Change notifications
- [ ] Entity relationship graph
