# WorldEntity.rs - Entity Data Model

## Purpose

Defines the `WorldEntity` struct - the fundamental unit of the world.
Represents locations, characters, factions, and other world objects.

## Core Struct

```rust
WorldEntity {
    id: Uuid,                           // Unique identifier
    entity_type: String,                // "location", "character", "faction"
    name: String,                        // Display name
    description: String,                 // Text description
    x: f64,                              // X coordinate
    y: f64,                              // Y coordinate
    
    properties_int: HashMap<String, i64>,      // Integer properties
    properties_float: HashMap<String, f64>,    // Float properties
    properties_string: HashMap<String, String>, // String properties
    
    tags: Vec<String>,              // For categorization/filtering
    owner_id: Option<Uuid>,          // Parent entity (hierarchical ownership)
    owned_entities: Vec<Uuid>,       // Entities this entity owns
    
    history: Vec<HistoryEntry>,      // Event log
    history_summary: Option<String>, // Periodic summary
    
    last_action_at: Option<DateTime>, // When entity last acted
    created_at: DateTime,
    updated_at: DateTime,
}
```

## PropertyValue Enum

```rust
enum PropertyValue {
    Int(i64),
    Float(f64),
    String(String),
}
```

## Methods

### Getters/Setters

| Method | Description |
|--------|-------------|
| `get_int(key)` | Get integer property |
| `get_float(key)` | Get float property |
| `get_string(key)` | Get string property |
| `set_int(key, value)` | Set integer property |
| `set_float(key, value)` | Set float property |
| `set_string(key, value)` | Set string property |

### Tags

| Method | Description |
|--------|-------------|
| `has_tag(tag)` | Check if has tag |
| `add_tag(tag)` | Add a tag |

### Calculations

| Method | Description |
|--------|-------------|
| `distance_to(other)` | Euclidean distance to another entity |
| `power_score()` | Calculate power for action selection |
| `wealth_score()` | Calculate wealth for tax/action |
| `mana_score()` | Calculate mana (with black/white bonus) |

### History

| Method | Description |
|--------|-------------|
| `add_history(action, details, outcome)` | Log an event |

## Implemented

- Serialization via serde (Binary format .owbl)
- All getter/setter methods
- Distance and score calculations
- Tag management
- History tracking
- Type-aware property access (int/float/string separated)

## Not Yet Implemented

- [ ] Validation methods for property values
- [ ] Relationship traversal (siblings, children, etc.)
- [ ] Computed properties from owned entities
- [ ] Time-based calculations (decay, growth)
- [ ] Property change listeners/hooks
- [ ] Cloning with new ID

## Future Enhancements

- [ ] Entity templates/blueprints
- [ ] Property inheritance from owner
- [ ] Automatic history summarization (LLM)
- [ ] Property modifiers (buffs, debuffs)
- [ ] Entity state machine
