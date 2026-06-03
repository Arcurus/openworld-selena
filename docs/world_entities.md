# World Entities

*This file documents all entities in the world as a backup and reference.*
*Restored: 2026-06-03 20:30 (after 1134B single-entity save → 2514B 8-entity save).*
*Source of truth: `world_data/save.owbl` + this MD. The OW API is the live editor.*

---

## Current World Roster (8 entities)

| # | Name | Type | Power | Position | Description |
|---|------|------|-------|----------|-------------|
| 1 | **World Clock** | `world_clock` | 1500 (Legendary) | (0, 0) | Meta-entity that tracks world time |
| 2 | **Silverstream Keep** | `location` | 100 (Uncommon) | (320, 180) | Fortified castle overlooking the river |
| 3 | **Ironforge Clan** | `faction` | 75 (Uncommon) | (420, 350) | Mighty dwarven smiths and warriors |
| 4 | **Shadow Ridge Camp** | `location` | 45 (Common) | (280, 320) | Hidden bandit encampment |
| 5 | **Whisperwood Forest** | `location` | 30 (Common) | (140, 220) | Ancient forest with strange magic |
| 6 | **Elder Moonthorn** | `character` | 25 (Common) | (145, 245) | Wise guardian of the forest (elf) |
| 7 | **Mira the Merchant** | `character` | 20 (Common) | (200, 290) | Traveling merchant with exotic goods |
| 8 | **Oak Valley Village** | `location` | 15 (Common) | (150, 250) | A peaceful farming village |

**Power tiers** (from `entity_action` prompt builder, main.rs:1145-1155):
- Legendary: total_power ≥ 1000
- Epic:      total_power ≥ 500
- Rare:      total_power ≥ 200
- Uncommon:  total_power ≥ 50
- Common:    total_power < 50

`total_power` is the sum of these `properties_int` keys: `power`, `strength`, `army_size`, `wealth`, `influence`, plus all `properties_float` values.

---

## Canonical Definitions (from `open-world/src/main.rs:2041-2117`)

These 7 are the original "sample world" entities that should always exist.
The World Clock (8th) is created automatically by the server on first load.

### Oak Valley Village (Settlement)
- **Type:** location
- **Location:** (150, 250) — Eastern forests
- **Tags:** `village`, `peaceful`, `farming`
- **Power:** 15
- **Description:** A peaceful farming village.

### Shadow Ridge Camp (Bandit Encampment)
- **Type:** location
- **Location:** (280, 320) — Northern passes
- **Tags:** `bandit`, `dangerous`, `mountain`
- **Power:** 45, wealth: 200, black_mana: 80
- **Description:** Hidden bandit encampment.

### Elder Moonthorn (Elf Elder)
- **Type:** character
- **Location:** (145, 245) — Oak Valley
- **Tags:** `elf`, `wise`, `guardian`
- **Power:** 25
- **Description:** Wise guardian of the forest.

### Whisperwood Forest (Magical Forest)
- **Type:** location
- **Location:** (140, 220) — Western ancient forest
- **Tags:** `forest`, `magical`, `ancient`
- **Power:** 30
- **Description:** Ancient forest with strange magic.

### Silverstream Keep (Royal Castle)
- **Type:** location
- **Location:** (320, 180) — Northern river valley
- **Tags:** `castle`, `royal`
- **Power:** 100, wealth: 500, military_level: 80, influence_radius: 120
- **Description:** Fortified castle overlooking the river.

### Ironforge Clan (Dwarven Faction)
- **Type:** faction
- **Location:** (420, 350) — Mountain stronghold
- **Tags:** `dwarven`, `clan`, `smiths`
- **Power:** 75, industry_rating: 85, population: 420
- **Description:** Mighty dwarven smiths and warriors.

### Mira the Merchant (NPC)
- **Type:** character
- **Location:** (200, 290) — Wandering
- **Tags:** `merchant`, `trader`
- **Power:** 20, wealth: 250, reputation: 60, connections: 75
- **Description:** Traveling merchant with exotic goods.

### World Clock (Meta Entity)
- **Type:** world_clock
- **Auto-created** on first server start (`World::create_clock_entity`).
- **Power:** 1500 (kept Legendary so the scheduler considers it significant)
- **Owns:** the world's time bookkeeping (actions_today, day, hour, has_history, history_entries, is_recording, last_recorded_day)

---

## Restoration Procedure

If the world is ever wiped (only `World Clock` left, or zero entities), recreate
the 7 canonical entities via `POST /api/entities`:

```bash
COOKIE="Cookie: openworld_auth=1"
URL="http://localhost:8081/api/entities"

create() {
  curl -s -X POST "$URL" -H "$COOKIE" -H "Content-Type: application/json" -d "$1"
}

create '{"entity_type":"location","name":"Oak Valley Village","description":"A peaceful farming village.","x":150,"y":250,"tags":["village","peaceful","farming"]}'
create '{"entity_type":"location","name":"Shadow Ridge Camp","description":"Hidden bandit encampment.","x":280,"y":320,"tags":["bandit","dangerous","mountain"]}'
create '{"entity_type":"character","name":"Elder Moonthorn","description":"Wise guardian of the forest.","x":145,"y":245,"tags":["elf","wise","guardian"]}'
create '{"entity_type":"location","name":"Whisperwood Forest","description":"Ancient forest with strange magic.","x":140,"y":220,"tags":["forest","magical","ancient"]}'
create '{"entity_type":"location","name":"Silverstream Keep","description":"Fortified castle overlooking the river.","x":320,"y":180,"tags":["castle","royal"]}'
create '{"entity_type":"faction","name":"Ironforge Clan","description":"Mighty dwarven smiths and warriors.","x":420,"y":350,"tags":["dwarven","clan","smiths"]}'
create '{"entity_type":"character","name":"Mira the Merchant","description":"Traveling merchant with exotic goods.","x":200,"y":290,"tags":["merchant","trader"]}'
```

Then set the `power` property on each:

```bash
# Replace <id> with the entity ID from the create response
curl -s -X PUT -H "$COOKIE" -H "Content-Type: application/json" \
  -d '{"value": 15}' "$URL/<id>/properties/int/power"
# Repeat with:  Silverstream Keep=100, Ironforge Clan=75, Shadow Ridge Camp=45,
#              Whisperwood Forest=30, Elder Moonthorn=25, Mira the Merchant=20
#              World Clock=1500
```

Finally force a save: `POST /api/world/save`.

---

## Backup Policy (added 2026-06-03 per Arcurus)

We never want to lose these entities again. Three layers of protection:

1. **This MD file** — human-readable, version-controlled, documents the canonical
   roster + restoration procedure.
2. **Binary save snapshot** — `world_data/backups/save-pre-restore-<TS>.owbl`
   and `world_data/backups/save-post-restore-<TS>.owbl` are auto-created
   around any restore operation.
3. **Live JSON export** — `world_data/backups/entities-live-<TS>.json` is the
   current state of `GET /api/entities?limit=100` at backup time, useful for
   diffing across time.

See `world_data/backups/README.md` (to be created) for the snapshot catalog.

---

*Last canonical restore: 2026-06-03 20:30 (cf6c529) — 7 entities recreated
from open-world/src/main.rs:2041, plus World Clock's power bumped to 1500.*
