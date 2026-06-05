# main.rs - API Server and Application Entry Point

## Purpose

- Entry point for the Open World server
- Defines all HTTP API endpoints using Axum
- Loads configuration from `settings.json`
- Serves the web client (static files from `web-client/`)
- Manages daily rotating log files (`logs/error-log-*.log`, `logs/llm-log-*.log`)

## Dependencies

- `axum` — HTTP framework
- `serde` — JSON serialization
- `tokio` — async runtime
- `reqwest` — HTTP client for LLM API calls
- `uuid` — entity IDs
- `tower_http` — static file serving

## AppState

Shared application state wrapped in `Arc<RwLock<World>>`:
```rust
struct AppState {
    world: Arc<RwLock<World>>,
    settings: Settings,
    save_path: String,       // e.g. "world_data/save.owbl"
    env_path: String,        // e.g. ".env"
    logger: Arc<Mutex<DailyLogger>>, // Daily rotating log files
}
```

## Settings

Loaded from `settings.json`. Key sections:
- `server` — host/port
- `world` — world name, creation settings
- `llm` — API URL, model, timeout, max_output_tokens
- `security` — cookie-based auth password
- `ui` — title, map size, colors

## API Routes

### World
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/` | World info (name, entity count) |
| POST | `/api/world/save` | Manual save to .owbl |
| GET | `/api/world/status` | Save file status and info |
| POST | `/api/world/create` | Create new world with 7 sample entities |

### Entities
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/entities` | List with search/filter |
| POST | `/api/entities` | Create entity |
| GET | `/api/entities/:id` | Get single entity |
| PUT | `/api/entities/:id` | Update entity |
| DELETE | `/api/entities/:id` | Delete entity |
| POST | `/api/entities/:id/history-summary/replace` | Surgical history edit (see [History Summary Replace Conventions](#history-summary-replace-conventions)) |

### Entity Actions (LLM-powered)
| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/entities/:id/action/llm` | Call LLM with action prompt |
| POST | `/api/entities/:id/action/process` | Parse and apply LLM response effects |

### Properties
| Method | Endpoint | Description |
|--------|----------|-------------|
| PUT | `/api/entities/:id/properties/int/:key` | Set integer |
| DELETE | `/api/entities/:id/properties/int/:key` | Delete integer |
| PUT | `/api/entities/:id/properties/float/:key` | Set float |
| DELETE | `/api/entities/:id/properties/float/:key` | Delete float |
| PUT | `/api/entities/:id/properties/string/:key` | Set string |
| DELETE | `/api/entities/:id/properties/string/:key` | Delete string |

### World Stats
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/world/stats` | Statistics by entity type |

### World Events
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/world/events` | List all world events |
| POST | `/api/world/events` | Add a new world event |
| PUT | `/api/world/events/:id` | Update a world event |
| DELETE | `/api/world/events/:id` | Delete a world event |

#### WorldEvent Structure
```json
{
  "id": "uuid",
  "name": "The Shadow Awakens",
  "description": "Ancient darkness stirs in the north...",
  "influence": "Entities become more paranoid and militaristic",
  "active": true
}
```

**Note:** Active world events are automatically included in LLM action prompts, influencing entity decisions and behaviors based on the event's influence description.

### Backup
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/world/save/download` | Download save file |
| GET | `/api/backups` | List backups |
| GET | `/api/backups/:filename` | Download backup |
| POST | `/api/backup/create` | Create tarball backup |
| DELETE | `/api/backups/:filename` | Delete backup |

### Web UI
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/` | Web client UI (fallback) |

## History Summary Replace Conventions

`POST /api/entities/:id/history-summary/replace` and the LLM-emit
`history_summary_replace` field in the action response both go through
the shared function `apply_history_summary_replaces` in `src/main.rs`,
so the conventions are identical for API, CLI, and LLM callers. Added
2026-06-04, extended with the `!ALL!` full-replace convention 2026-06-05.

| `old_part` value | Behavior |
|------------------|----------|
| `"!ALL!"` | **Full replace** — discard the current summary, set it to `new_part`. Use this when a full restructure is needed and you want to make sure important things don't get lost. No warning. |
| `""` (empty) | **Append** — `new_part` is added to the end of the current summary, or becomes the new summary if there isn't one yet. No warning. |
| non-empty, found | **Find-replace** — replace the first occurrence of `old_part` with `new_part`. |
| non-empty, not found | **Skip + warning** (the chain continues). With the API endpoint, pass `"not_found_is_error": true` in the body to get a 404 instead of a 200-with-warning. |

After the replace, the result is truncated to the entity's effective
`max_history_summary_chars` cap if it goes over, with a warning logged
and a `…` appended at the truncation boundary. The LLM template
(`ai_templates/EntityAction.md`) is the user-facing reference; the
behavior matrix above is the authoritative one for operators and
debugging.

## LLM Integration

Uses MiniMax Anthropic-compatible API format:
- **URL:** `https://api.minimax.io/anthropic/v1/messages`
- **Headers:** `x-api-key`, `anthropic-version: 2023-06-01`
- **Request:** `{"model": "...", "max_tokens": 50000, "messages": [{"role": "system", "content": "..."}, {"role": "user", "content": "..."}]}`
- **Response:** parses `content[]` array for `type: "text"` and `type: "thinking"` blocks
- **Error check:** `base_resp.status_code == 0`
- **Timeout:** 180 seconds (configurable via `llm_timeout_secs`)

### Effect Parsing (type-aware)

LLM responses must include `action`, `outcome`, `effects`, `narrative`:
- **int** — JSON integers like `5`, `-3` (no decimal point)
- **float** — JSON numbers WITH a decimal point like `0.5`, `1.0`, `0.75`
- **string** — quoted text like `"King Aldric"`
- **bool** — encoded as `1` (true) or `0` (false)

If type mismatch (e.g., setting a float property with an int), the effect is skipped with a warning.

## Authentication

Password-based using `.env` file (`AUTH_PASSWORD`):
- Set via `POST /api/env/configure`
- Stored in cookie (`openworld_auth`)
- Sessions expire after 1 hour
- Required for: LLM calls, entity deletion, property changes, world save

## Logging System

`DailyLogger` with two daily-rotating files:
- `logs/error-log-YYYY-MM-DD.log` — errors and warnings from all handlers
- `logs/llm-log-YYYY-MM-DD-DD.log` — every LLM call (context, response, time_ms, success/failure, parsing outcome)

The date is checked on every log write; if the day has changed, new files are created.

## Implemented

- Entity CRUD with properties (int, float, string)
- Tag-based filtering and proximity search
- x,y coordinate system
- Ownership hierarchy (owner_id)
- History tracking per entity
- Binary persistence (.owbl) with auto-save
- LLM-powered entity actions (3-step: context → call → process)
- Type-aware effect parsing (int/float/string/bool)
- Cookie-based authentication
- Daily rotating logs (errors + LLM calls)
- World statistics by entity type
- Backup/restore system (tarball + direct file download)

## Not Yet Implemented

- [ ] WebSocket support for real-time updates
- [ ] Automated world action scheduling (cron-based)
- [ ] GM web interface for world editing
- [ ] World creation tools
- [ ] Player accounts
- [ ] Request rate limiting
- [ ] CORS configuration
- [ ] Metrics/health endpoints
- [ ] Better error handling with specific error types
