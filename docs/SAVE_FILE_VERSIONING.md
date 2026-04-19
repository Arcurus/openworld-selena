# Save File Versioning

This document describes how Open World handles save file versioning for backward compatibility.

## Version Numbers

### World Version (`VERSION`)
- Stored at the start of every save file (after magic bytes)
- Current value: **2**
- Increment when making breaking changes to the world file format
- When loading, if `file_version > VERSION`, loading fails (future format unsupported)

### Entity Version (`ENTITY_VERSION`)
- Stored once per world file, after entity count
- Current value: **1**
- Increment when making breaking changes to how individual entities are serialized
- Used by `read_entity` to determine which deserialization format to use

## Binary File Format

```
[4 bytes: magic "OWBL"]
[4 bytes: world VERSION (u32)]
[4 bytes: class name length (u32)]
[N bytes: class name]
[4 bytes: data length (u32)]
[N bytes: world data]
```

## World Data Format (within data section)

```
[4 bytes: world name length + N bytes: world name]
[4 bytes: entity count (u32)]
[4 bytes: ENTITY_VERSION (u32)]  -- Only if VERSION >= 2
[For each entity:]
  [16 bytes: entity UUID]
  [N bytes: entity data]
[4 bytes: path count]
[For each path:]
  [N bytes: path data]
[Settings data...]
```

## Version History

### Version 1 (Legacy)
- No entity_version field after entity count
- Entity format is implicit v1
- Old saves from before this system was implemented

### Version 2 (Current)
- Added entity_version field after entity count
- Allows for entity format versioning independent of world format
- Supports backward compatibility when entity format changes

## Adding New Fields

When adding new fields to entities that need to be persisted:

1. **Do NOT break existing field order** - Add new fields at the END of the serialization
2. **Increment ENTITY_VERSION** when making breaking changes to entity format
3. **Handle missing fields** in `read_entity` by providing defaults for older versions

### Example: Adding a new field to WorldEntity

1. In `write_entity`, add the new field at the end:
```rust
Self::write_string(&entity.new_field, data);
```

2. In `read_entity`, handle both old and new versions:
```rust
if entity_version >= 2 {
    entity.new_field = Self::read_string(data, pos);
} else {
    entity.new_field = String::new(); // Default for old saves
}
```

3. Increment `ENTITY_VERSION` from 1 to 2

## Handling Old Saves

When loading an old save file:

1. The world `VERSION` is read from the file header
2. If `VERSION == 1`, entity_version field doesn't exist in the file - assume ENTITY_VERSION = 1
3. If `VERSION >= 2`, read entity_version after entity_count
4. Use entity_version to determine how to deserialize each entity

## Best Practices

1. **Never remove fields** - just stop reading/writing them if not needed
2. **Always add new fields at the end** of serialization
3. **Provide sensible defaults** for fields not present in old saves
4. **Document changes** in this file when incrementing versions
5. **Test with old saves** before deploying format changes

## Migration Strategy

For major format changes, consider:
1. Auto-migrate on load: read old format, write new format, save
2. Or: support both formats in code with version checks
3. Never break loading of existing saves without warning
