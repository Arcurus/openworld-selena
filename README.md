# Open World - Dynamic Evolving World

```
═══════════════════════════════════════════════════════════════════════════════
                           OPEN WORLD
                     Dynamic Evolving World
═══════════════════════════════════════════════════════════════════════════════
```

## Description

A Rust-based server for a dynamic world simulation where entities evolve, organize, and make decisions autonomously. Players can interact as characters or as GameMasters with editing capabilities.

The world is powered by LLMs that drive entity behavior - generating options, selecting factors, and narrating outcomes - while deterministic game logic handles the actual state changes.

**Website:** http://159.195.43.108:8080

---

## 🚀 Quick Start

### Prerequisites

- **Rust** (latest stable)
- **Cargo** (comes with Rust)

### Run the Server

```bash
cd open-world
cargo run
```

The server starts on `http://localhost:8080`.

On first run, the world is created with 7 sample entities (Oak Valley Village, Shadow Ridge Camp, Whisperwood Forest, Silverstream Keep, Elder Moonthorn, Ironforge Clan, Mira the Merchant).

---

## 💾 Persistence

**Entities ARE saved to disk!** The world auto-loads on startup and auto-saves after updates.

- Save file: `world_data/save.owbl` (binary format)
- Backup on save: `world_data/save.owbl.bak`
- Manual save: `POST /api/world/save`
- Check status: `GET /api/world/status`

---

## Build Commands

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# Run tests
cargo test

# Check code without building
cargo check

# Clean build artifacts
cargo clean
```

### Project Structure

```
open-world/
├── settings.json          # Configuration (port, world name, LLM, UI settings)
├── .env                   # Secrets (LLM_API_KEY, etc.)
├── Cargo.toml            # Rust dependencies
├── README.md             # This file
├── web-client/
│   └── index.html        # Web UI (map + entity list + action form)
├── src/
│   ├── main.rs           # API server, settings, routes, handlers
│   └── world_data/
│       ├── mod.rs        # Module exports
│       ├── WorldEntity.rs # Entity data model
│       ├── World.rs      # World container and entity management
│       └── persistence.rs # Binary save/load (BinaryPersistence)
├── docs/                 # Per-file documentation
├── ai_templates/         # LLM prompt templates
└── logs/                 # Daily rotating logs (error + LLM)
```

### Configuration

Edit `settings.json` to customize:

```json
{
  "server": { "host": "0.0.0.0", "port": 8080 },
  "world": { "name": "Your World Name" },
  "llm": { "model": "...", "max_output_tokens": 50000 },
  "ui": { "title": "Open World", "map_size": { "width": 800, "height": 600 } }
}
```

---

## 💾 Backup

```bash
# Plain copy
cp -r open-world open-world-backup

# Compressed archive
tar czf open-world-backup-$(date +%Y%m%d-%H%M%S).tar.gz open-world
```

---

## 📁 Documentation Files

| File | Description |
|------|-------------|
| README.md | This file — overview, quick start, roadmap |
| src/main.rs.md | API server, routes, handlers, LLM integration |
| src/world_data/WorldEntity.rs.md | Entity data model |
| src/world_data/World.rs.md | World container and entity management |
| src/world_data/persistence.rs.md | Binary save/load |

---

## 🏗️ Architecture

### Tech Stack

| Component | Technology |
|-----------|------------|
| Backend | Rust + Axum |
| Frontend | Vanilla HTML/CSS/JS |
| Storage | Binary (.owbl) with auto-save |
| LLM | MiniMax Anthropic-compatible API |
| Logging | Daily rotating files (errors + LLM calls) |

### Persistence (Binary Format)

World data saved in `.owbl` format:
- Auto-loads on startup
- Auto-saves after every entity change
- Backup at `world_data/save.owbl.bak` before each save

**API:** `POST /api/world/save`, `GET /api/world/status`

### Effect Value Types

The LLM action system supports three property types:
- **int** — JSON integers like `5`, `-3`, `10` (no decimal point)
- **float** — JSON numbers WITH a decimal point like `0.5`, `1.0`, `0.75`
- **string** — quoted text like `"King Aldric"`
- **bool** — encoded as `1` (true) or `0` (false), NOT `true`/`false`

### Logging

Two daily-rotating log files in `logs/`:
- `error-log-YYYY-MM-DD.log` — all errors and warnings
- `llm-log-YYYY-MM-DD.log` — every LLM call with context, response, timing, parsing outcome

---

## 🌐 API Endpoints

### World
```
GET  /api/                    - World info (name, entity count)
POST /api/world/save          - Manual save
GET  /api/world/status        - Save status and file info
```

### Entities
```
GET    /api/entities          - List (q=search, entity_type=x, tags=a,b)
POST   /api/entities          - Create entity
GET    /api/entities/:id      - Get single entity
PUT    /api/entities/:id      - Update entity
DELETE /api/entities/:id      - Delete entity
```

### Entity Actions (LLM-powered)
```
POST /api/entities/:id/action/llm     - Call LLM with action prompt
POST /api/entities/:id/action/process - Parse and apply LLM response effects
```

### Properties
```
PUT    /api/entities/:id/properties/int/:key    - Set integer
DELETE /api/entities/:id/properties/int/:key    - Delete integer
PUT    /api/entities/:id/properties/float/:key  - Set float
DELETE /api/entities/:id/properties/float/:key  - Delete float
PUT    /api/entities/:id/properties/string/:key  - Set string
DELETE /api/entities/:id/properties/string/:key - Delete string
```

### Web UI
```
GET /                          - Serve web client
```

---

## 📋 Roadmap

### ✅ Implemented

- [x] Entity CRUD with properties (int, float, string)
- [x] Tag-based filtering and proximity search
- [x] x,y coordinate system with map display
- [x] Ownership hierarchy (owner_id)
- [x] History tracking per entity
- [x] Binary persistence (.owbl) with auto-save
- [x] Web UI (map + entity list + action form)
- [x] LLM-powered entity actions (3-step form)
- [x] Type-aware effect parsing (int/float/string/bool)
- [x] Daily rotating logs (error + LLM)
- [x] Backup/restore system
- [x] World statistics

### ⏳ In Progress

- [ ] Automated world action scheduling (cron-based)

### 📋 Planned

- [ ] GM web interface for world editing
- [ ] World creation tools
- [ ] Player accounts
- [ ] Real-time updates (WebSocket)
- [ ] SQLite persistence (upgrade from binary)
- [ ] Vector search (meilisearch/pinecone)
- [ ] World sharing
- [ ] OHOL world display integration

---

## 🔮 Future Integration

Aims to integrate with OpenLife/OHOL:
- Share entity definitions
- OHOL world display could show Open World entities
- Players could influence OHOL through Open World interface
- AI NPCs could be powered by Open World's action system

---

*Built with Rust and curiosity* 🌙
