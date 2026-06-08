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

**Website:** http://159.195.43.108:8081

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

The server starts on `http://localhost:8081`.

On first run, the world is created with the **world clock** (time bookkeeping) + the canonical lore events from `docs/world_lore.md`. Per Arcurus 2026-06-04, fresh worlds do **not** auto-seed sample entities — call `World::seed_sample_entities()` (or the web UI's "Generate sample entities" button, or `POST /api/world/create?generate_sample=true`) to add the 7 canonical sample entities. See [`docs/world_entities.md`](docs/world_entities.md) for the full example roster (18 entities, with UUIDs, descriptions, tags, and properties).

> **Heads up (added 2026-06-06):** worlds created before `World::seed_default_events()` was wired into `World::new()` (i.e. before the 18-entity re-seed on 2026-06-03 21:17) have **0 active events** even though the function exists now. If you load such a world and `GET /api/world/events` returns `[]`, run the helper script to push the 5 canonical "Shadow Awakening" events via the existing POST endpoint:
>
> ```bash
> python3 scripts/seed_default_world_events.py
> ```
>
> The script is **idempotent** (skips events that already exist by name+description match), safe to re-run, and only uses the public API (cookie `openworld_auth=1`).

---

## 💾 Persistence

**Entities ARE saved to disk!** The world auto-loads on startup and auto-saves after updates.

- Save file: `world_data/save.owbl` (binary format)
- Backup on save: `world_data/save.owbl.bak`
- Manual save: `POST /api/world/save`
- Check status: `GET /api/world/status`

### 📜 Action History (durable JSONL log)

Every entity action is appended to a separate, durable, append-only JSONL log:

- File: `world_data/action_history.jsonl` (one JSON object per line)
- Survives a corrupted or replaced `save.owbl` (independent of the binary save)
- Cheap to grep / parse / export without loading the whole world
- Query per-entity: `GET /api/entities/:id/history?limit=N` (most recent first)
- Schema per line: `entity_id`, `entity_name`, `timestamp`, `action`, `outcome`, `details`, `effects`, `warnings`

Read API lives in `src/world_data/action_history_log.rs` (`append_entry`, `load_for_entity`, `count_for_entity`).

### 🌍 Recent World Actions (cross-entity feed in the LLM prompt)

Per `Arcurus 2026-06-07 (#openworld)`: "add to the world action llm call an insertion of not yet processed world actions".

Every LLM action call now sees a `## Recent World Actions (since your last action)` block in its prompt, listing the most recent cross-entity actions the actor has not yet seen.

- **Source:** `world_data/action_history.jsonl`, filtered to `timestamp > entity.last_action_at` (or all entries if the actor has never acted).
- **Cap:** 10 most-recent entries, most-recent-first.
- **Filters:** drops the actor's own most-recent entry (the one it's generating) and any system-entity actions (World Clock / `abstract` / `meta`-tagged).
- **Renderer:** `build_recent_world_actions_str` in `src/world_data/context_builder.rs`. Reader: `load_recent_world_actions` / `load_recent_world_actions_at` in `src/world_data/action_history_log.rs`. Placeholder: `{recent_world_actions}` in `ai_templates/EntityAction.md`.

### 📋 History Summary + Anti-Repetition (LLM-owned)

Per `Arcurus 2026-06-04 (#openworld)`: every world action's LLM call also updates a rolling per-entity `history_summary`. Same call, no extra LLM budget.

- **Setting:** `world.settings.max_history_summary_chars` (default `500`) — soft cap, server truncates with `…` if the LLM goes over and adds a warning to the response.
- **Setting:** `world.settings.history_entries_fully_displayed` (default `10`) — the LLM sees this many most-recent actions in full and is explicitly told **not** to pick a semantically identical action from that window.
- **Storage:** `entity.history_summary: Option<String>` on the WorldEntity (saved in `save.owbl`).
- **LLM contract:** the response JSON now includes a required `history_summary` string. If the LLM omits it the existing summary is left untouched (lenient).
- **UI:** the entity modal shows a `📋 History Summary` card above the `📜 Action History` section, with a configurable history-limit dropdown (10 / 25 / 50 / 100 / 250 / 500, default 10, persisted in `localStorage`).
- **Save format:** v3 (was v2). Loads of older saves fall back to default `max_history_summary_chars`.

Context assembly in `src/world_data/context_builder.rs`. Template in `ai_templates/EntityAction.md`.

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
  "server": { "host": "0.0.0.0", "port": 8081 },
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
| [docs/world-mechanics.md](docs/world-mechanics.md) | **Action selector formula, history formatting, log files, auth, persistence, project topology** (start here) |
| [docs/property-catalog.md](docs/property-catalog.md) | **Operator-facing property reference** — per-property summary, impact mechanics, auto-tag rules, internal/bookkeeping properties. Not loaded into the LLM prompt (use [ai_templates/property_docs.md](ai_templates/property_docs.md) for that). |
| [docs/llm-context.md](docs/llm-context.md) | What's in the LLM action context (template variables, anti-repetition guidance) |
| [docs/world_entities.md](docs/world_entities.md) | Current entity roster |
| [docs/world_events.md](docs/world_events.md) | Active world events |
| [docs/world_lore.md](docs/world_lore.md) | Realm lore: factions, Shadow Awakening arc, era context |
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

### 🛡️ Sanity Check

A read-only sanity check is bundled with the project at `code/ow_sanity_check.py`. It scans the running world server and today's LLM log for things worth reporting on, and prints (or posts) a Markdown report. **It never mutates anything** — there is no `--apply` flag by design.

What it checks (per Arcurus 2026-06-05 #openworld):

- **Duplicate relations in entity summaries** — flags entities whose `history_summary` lists the same relation name more than once (e.g. `Mira the Scribe → …` appearing twice on Velora's card). Names are normalized (case + whitespace + trailing punctuation) before comparison.
- **LLM call counts** for the day, split by `replace_only` / `full_only` / `both` (which corresponds to the LLM returning both `history_summary` and `history_summary_replace` in one response) / `parse_error`, and separated into pre- and post-restart slices (binary restart = commit 3373e0d, 2026-06-04 22:05:52 UTC) so the warnings-vec fix can be measured.
- **Multi-replace calls** (one LLM response containing ≥2 `history_summary_replace` pairs).
- **Truncation events** flagged in the parsing line.
- **Per-call warnings bucketed** by reading the `Warnings: [...]` list that the new binary writes into the `--- Parsing ---` line (commit 3373e0d, 2026-06-04). Buckets: `Both dropped (replace wins)`, `Neither (no update)`, `Truncated (over cap)`, `old_part not found`, `old_part ambiguous (occurs N×)`, `Regex repair (LLM empty-key bug)` (commit 78ea1ac, 2026-06-05 — fired when the LLM emits the `{"old_part":"...","":"new_part":"..."}` malformation and the server's regex fixup rescues it; tracks how often the World Clock's bug actually recurs vs. our repair path), `System entity targeted` + `Skipped effect (system entity)` (commit c7f3bc27, 2026-06-03 — World Clock and other meta entities reject LLM effect writes), `Skipped effect (magnitude cap)`, `Other`. For each non-zero bucket we print a couple of sample warning strings.
- **Summary length distribution** vs the hard cap (`world.settings.max_history_summary_chars`, default 10 000).
- **Stale entities** (no action in 24+ h).

Usage:

```bash
# print the report to stdout
python3 code/ow_sanity_check.py

# machine-readable JSON
python3 code/ow_sanity_check.py --json

# post to #openworld-log (Discord channel 1511696310984773633) via the bot token
python3 code/ow_sanity_check.py --post

# build + show, do not post
python3 code/ow_sanity_check.py --post --dry-run
```

Exit codes: `0` = clean, `1` = findings (non-fatal), `2` = runtime error (couldn't reach server, etc.).

**Scheduling:** the check is **on-demand**, not cron-scheduled. Run it after a service restart, after seeding entities, or whenever you want a one-shot report. The script defaults to the open-world server at `http://127.0.0.1:8081`; override with `--api-url=URL` for a remote box.

### 🧬 Merge Relations

A read-AND-write companion to the sanity check is bundled at `code/merge_relations.py`. The sanity check *reports* duplicate relations; this tool *fixes* them, with human review.

It scans every entity's `history_summary` for the same relation name appearing more than once, builds a merge plan, and (in `--apply` mode) POSTs one first-match find-replace per duplicate group to the existing `POST /api/entities/:id/history-summary/replace` endpoint. Default is **dry-run** — it prints the plan and exits with `1` if there are findings.

**Safety rails** (per Arcurus 2026-06-07 #openworld):
- `--apply` requires `--yes` (typed confirmation token) — there's no silent apply path.
- The script snapshots `world_data/save.owbl` to `world_data/backups/save-pre-merge-{ts}.owbl` before the first apply. Timestamped, never overwritten. Use `--no-backup` to skip (not recommended).
- It reuses the exact `→` relation-split regex from `ow_sanity_check.py` so the two scripts agree on what "duplicate" means.
- All HTTP calls go through the cookie auth; if the server rejects auth, the script exits with `2` and a clear message.

**Strategies** (the merge logic for choosing the survivor text):
- `keep-first` (default for "least change") — take the first occurrence as-is, drop the rest.
- `keep-last` (default in the script) — take the most recent LLM "understanding" of the relation. Loses older nuance.
- `longest` — keep the variant with the most text. Preserves max info, but may include stale wording.
- `combine` — concatenate all unique variants with ` | `. Most info-preserving, can grow the summary.

Usage:

```bash
# dry-run (default): scan + print plan, no changes
python3 code/merge_relations.py

# machine-readable JSON plan
python3 code/merge_relations.py --json

# apply with keep-last (the recommended default for "most recent wins")
python3 code/merge_relations.py --apply --yes

# apply with a different strategy
python3 code/merge_relations.py --strategy=longest --apply --yes
python3 code/merge_relations.py --strategy=combine --apply --yes

# only act on one entity
python3 code/merge_relations.py --entity=<id> --apply --yes

# post the report to #openworld-log (1511696310984773633)
python3 code/merge_relations.py --post                  # plan-only post
python3 code/merge_relations.py --post --apply --yes    # apply + post the result
```

Exit codes: `0` = clean / all applied, `1` = findings (dry-run refused to apply), `2` = runtime error, `3` = partial apply (some merges failed).

**Tests:** `code/_test_merge_relations.py` covers the offline parts (`split_relations`, `normalize_rel_name`, `choose_winner`, `find_duplicates`, `build_replacement_plan`, `render_plan_report`) with synthesized entity data, plus a best-effort live-server integration check. Run: `python3 code/_test_merge_relations.py`.

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
