# persistence.rs - Binary World Persistence

## Purpose

Provides binary save/load functionality for the world using a custom `.owbl` (Open World Binary) format.

## Format

```
[4 bytes] Magic: "OWBL" (0x4F57424C)
[4 bytes] Version: u32 (little-endian, currently version 1)
[4 bytes] Class name length: u32
[N bytes] Class name: "World"
[4 bytes] Data length: u32
[N bytes] Binary data (ron-encoded World struct)
```

## Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `MAGIC` | `b"OWBL"` | File magic bytes |
| `VERSION` | `1` | Current format version |

## Public API

```rust
pub struct BinaryPersistence;
```

### Methods

| Method | Description |
|--------|-------------|
| `save_world(world, path)` | Save world to binary file |
| `load_world(path)` | Load world from binary file |
| `save_exists(path)` | Check if save file exists |

## save_world(world, path)

Saves the world to a binary file with automatic backup.

**Process:**
1. Create backup at `{path}.bak` (copies existing file if present)
2. Open/create the target file
3. Write magic bytes (`"OWBL"`)
4. Write version (`1`)
5. Write class name (`"World"`)
6. Serialize world data using RON
7. Write data length + binary data

**Returns:** `Ok(())` on success, `Err(String)` on failure

**Error cases:**
- Failed to create backup → error
- Failed to create/open file → error
- Serialization error → error
- Write error → error

## load_world(path)

Loads the world from a binary file.

**Process:**
1. Open the file
2. Read and verify magic bytes (`"OWBL"`)
3. Read version — fails if version > current VERSION
4. Read class name
5. Read data length
6. Read binary data
7. Deserialize from RON

**Returns:** `Ok(World)` on success, `Err(String)` on failure

**Error cases:**
- File doesn't exist → error
- Invalid magic bytes → "Invalid file format"
- Unsupported version → "Unsupported file version"
- Corrupted data → RON deserialization error
- Read error → error

## save_exists(path)

Simple check for file existence.

**Returns:** `true` if file exists, `false` otherwise

## Interaction with main.rs

- `main()` calls `BinaryPersistence::save_exists()` to decide whether to load or create new world
- `main()` calls `BinaryPersistence::load_world()` to restore saved state
- All entity-modifying handlers call `world.auto_save()` after changes
- `auto_save()` (in World.rs) calls `BinaryPersistence::save_world()`

## Auto-backup

Before every save, the previous save file is copied to `{path}.bak`. This means:
- Only one backup is kept (previous .bak is overwritten)
- To restore from backup, manually copy `{path}.bak` to `{path}`

## Serialization

Uses RON (Rusty Object Notation) for the binary data encoding. The `World` struct and all nested types must implement `Serialize` and `Deserialize`.

## Implemented

- Binary save/load with magic/version headers
- Automatic backup before save
- Auto-load on server startup
- Version checking for forward compatibility

## Not Yet Implemented

- [ ] Backup rotation (keep N backups)
- [ ] Incremental/differential saves
- [ ] Checksum verification (SHA-256)
- [ ] Compression (zstd/lz4)
- [ ] Migration between format versions
