# World Entities

*This file documents all entities in the world as a backup and reference.*
*Last update: 2026-06-04 — 18 entities (7 canonical + 10 lore + 1 auto).*
*Source of truth: `world_data/save.owbl` + this MD. The OW API is the live editor.*

---

## The Realm of Shadows (current world)

- **Name:** `The Realm of Shadows` (from `settings.json → world.name`)
- **Description:** *"A mysterious realm where shadows hold ancient secrets"* (from `settings.json → world.description`)
- **Lore:** see [`docs/world_lore.md`](world_lore.md) for the full era, factions, and the Shadow Awakening arc.
- **Origin:** the world was created on 2026-02-16 as the first Open World instance; entities were progressively added via the OW API and LLM-driven action cycles. The 7 canonical entities below (Group A) come from `World::seed_sample_entities()`; the 10 lore entities (Group B) were added in commit `a5cb4c8` (2026-06-03); World Clock (Group C) is auto-created on every `World::new()`.

**Live counts** (from `GET /api/entities`, 2026-06-04):
- Total: 18 entities
- Legendary tier: 3 (Vaelthrix, Velora, World Clock)
- Type breakdown: 7 locations, 5 characters, 2 factions, 2 heroes, 1 dragon, 1 artifact, 1 oracle, 1 world_clock, 1 location (Scribe is character; miscounted here, see roster below for ground truth)

---

## Current World Roster (18 entities)

| # | Name | Type | Power | Position | Description |
|---|------|------|-------|----------|-------------|
| ★ 1 | **Vaelthrix the Endless** | `dragon` | 2000 (Legendary) | (500, 100) | Ancient dragon stirring in northern mountains |
| ★ 2 | **World Clock** | `world_clock` | 1500 (Legendary) | (0, 0) | Meta-entity that tracks world time |
| ★ 3 | **Velora the Undying** | `hero` | 1500 (Legendary) | (250, 400) | Cursed knight who refused death itself |
| 4 | **The Shadow Crown** | `artifact` | 300 (Rare) | (500, 250) | Blackened silver circlet that creates new Shadows |
| 5 | **Kira Dawnblade** | `hero` | 180 (Uncommon) | (340, 220) | Young knight marked by the prophecy |
| 6 | **Keepers of the Eternal Flame** | `faction` | 140 (Uncommon) | (400, 250) | Ancient order preserving the balance |
| 7 | **Zephyrus the Oracle** | `oracle` | 120 (Uncommon) | (250, 130) | Blind oracle who speaks in riddles |
| 8 | **The Sunken Temple** | `location` | 80 (Uncommon) | (180, 450) | Half-submerged ruins where bells still ring |
| 9 | **Silverstream Keep** | `location` | 75 (Uncommon) | (320, 180) | Fortified castle overlooking the river |
| 10 | **Shadow Ridge Camp** | `location` | 52 (Uncommon) | (280, 320) | Hidden bandit encampment |
| 11 | **The Drowned City** | `location` | 50 (Uncommon) | (600, 480) | Skeleton of a sunken metropolis |
| 12 | **Whisperwood Forest** | `location` | 33 (Common) | (140, 220) | Ancient forest with strange magic |
| 13 | **Oak Valley Village** | `location` | 30 (Common) | (150, 250) | Peaceful farming village |
| 14 | **Elder Moonthorn** | `character` | 28 (Common) | (145, 245) | Wise guardian of the forest (elf) |
| 15 | **Mira the Merchant** | `character` | 18 (Common) | (200, 290) | Traveling merchant with exotic goods |
| 16 | **Mira the Scribe** | `character` | 10 (Common) | (350, 290) | Quiet chronicler who trades stories for lodging |
| 17 | **The Wandering Bard** | `character` | 10 (Common) | (380, 320) | Nameless minstrel whose face always changes |

★ = Legendary tier (total_power ≥ 1000)

**Power tiers** (from `entity_action` prompt builder, main.rs:1145-1155):
- Legendary: total_power ≥ 1000
- Epic:      total_power ≥ 500
- Rare:      total_power ≥ 200
- Uncommon:  total_power ≥ 50
- Common:    total_power < 50

`total_power` is the sum of these `properties_int` keys: `power`, `strength`, `army_size`, `wealth`, `influence`, plus all `properties_float` values.

**Selection** (from selena-project's scheduler, commit d0185a9): the
scheduler picks entities by `(properties_int.power + 1) * seconds_idle`,
weighted sample without replacement, persisted to
`data/ow_entity_last_action.json`. World Clock tracks its own time bookkeeping.

---

## Canonical Definitions

### Group A — 7 from `open-world/src/main.rs:2041-2117` (the original sample world)

Restored 2026-06-03 20:30 in commit `a5cb4c8`.

- **Oak Valley Village** (Settlement, power=15)
- **Shadow Ridge Camp** (Bandit Encampment, power=45)
- **Elder Moonthorn** (Elf Elder, power=25)
- **Whisperwood Forest** (Magical Forest, power=30)
- **Silverstream Keep** (Royal Castle, power=100)
- **Ironforge Clan** (Dwarven Faction, power=75)
- **Mira the Merchant** (NPC, power=20)

### Group B — 10 from `world_lore.md` (added 2026-06-03 21:15, commit pending)

- **Vaelthrix the Endless** — Dragon (Legendary, power=2000)
- **Velora the Undying** — Hero (Legendary, power=1500)
- **Kira Dawnblade** — Hero (power=180)
- **Zephyrus the Oracle** — Oracle (power=120)
- **The Shadow Crown** — Artifact (power=300)
- **The Keepers of the Eternal Flame** — Faction (power=140)
- **The Sunken Temple** — Location (power=80)
- **The Drowned City** — Location (power=50)
- **Mira the Scribe** — Character (power=10)
- **The Wandering Bard** — Character (power=10)

### Group C — Auto-created

- **World Clock** — `world_clock` (auto-created on first server load, power=1500)

### Notable entities from lore NOT YET in the world (future adds)

- The Forgotten Heir (prophecy, "blood shall seal the door")
- The Weaver (Velora's curser, mysterious)
- The Silver Warden's line (Silverstream Keep's lineage)

---

## Restoration Procedure

If the world is ever wiped (only `World Clock` left, or zero entities), recreate
the canonical 17 + World Clock via the OW API. The 7 original entities come
from `open-world/src/main.rs:2041`; the 10 new ones come from this file.

```bash
COOKIE="Cookie: openworld_auth=1"
URL="http://localhost:8081/api/entities"

create() {
  curl -s -X POST "$URL" -H "$COOKIE" -H "Content-Type: application/json" -d "$1"
}

# === Group A: 7 original sample entities ===
create '{"entity_type":"location","name":"Oak Valley Village","description":"A peaceful farming village.","x":150,"y":250,"tags":["village","peaceful","farming"]}'
create '{"entity_type":"location","name":"Shadow Ridge Camp","description":"Hidden bandit encampment.","x":280,"y":320,"tags":["bandit","dangerous","mountain"]}'
create '{"entity_type":"character","name":"Elder Moonthorn","description":"Wise guardian of the forest.","x":145,"y":245,"tags":["elf","wise","guardian"]}'
create '{"entity_type":"location","name":"Whisperwood Forest","description":"Ancient forest with strange magic.","x":140,"y":220,"tags":["forest","magical","ancient"]}'
create '{"entity_type":"location","name":"Silverstream Keep","description":"Fortified castle overlooking the river.","x":320,"y":180,"tags":["castle","royal"]}'
create '{"entity_type":"faction","name":"Ironforge Clan","description":"Mighty dwarven smiths and warriors.","x":420,"y":350,"tags":["dwarven","clan","smiths"]}'
create '{"entity_type":"character","name":"Mira the Merchant","description":"Traveling merchant with exotic goods.","x":200,"y":290,"tags":["merchant","trader"]}'

# === Group B: 10 legendary + supporting characters ===
create '{"entity_type":"dragon","name":"Vaelthrix the Endless","description":"An ancient dragon stirring in the northern mountains. Shadow curls from its half-open eyes.","x":500,"y":100,"tags":["dragon","legendary","ancient","awakening"]}'
create '{"entity_type":"hero","name":"Velora the Undying","description":"A corroded-silver knight who refused death itself.","x":250,"y":400,"tags":["hero","legendary","undying","cursed"]}'
create '{"entity_type":"faction","name":"Keepers of the Eternal Flame","description":"An ancient order preserving the balance of the world.","x":400,"y":250,"tags":["faction","balance","temple","neutral"]}'
create '{"entity_type":"location","name":"The Sunken Temple","description":"Half-submerged ruins where the bells still ring on the longest night.","x":180,"y":450,"tags":["ruin","temple","submerged","haunted"]}'
create '{"entity_type":"location","name":"The Drowned City","description":"The skeleton of a great city that sank beneath the eastern bay.","x":600,"y":480,"tags":["ruin","ancient","cursed","ocean"]}'
create '{"entity_type":"hero","name":"Kira Dawnblade","description":"A young knight marked by the prophecy. She searches for the Forgotten Heir.","x":340,"y":220,"tags":["hero","prophecy","young","destiny"]}'
create '{"entity_type":"oracle","name":"Zephyrus the Oracle","description":"A blind oracle who speaks in riddles.","x":250,"y":130,"tags":["oracle","blind","ancient","prophecy"]}'
create '{"entity_type":"artifact","name":"The Shadow Crown","description":"A circlet of blackened silver that creates new Shadows.","x":500,"y":250,"tags":["artifact","cursed","shadow","ancient"]}'
create '{"entity_type":"character","name":"Mira the Scribe","description":"A quiet chronicler who keeps the realm history.","x":350,"y":290,"tags":["npc","scribe","lorekeeper","scholar"]}'
create '{"entity_type":"character","name":"The Wandering Bard","description":"A nameless minstrel who appears in every tavern on the same night.","x":380,"y":320,"tags":["npc","bard","wanderer","mysterious"]}'

# Then PUT power values on each:
# Vaelthrix=2000, Velora=1500, World Clock=1500, Shadow Crown=300, Kira=180,
# Keepers=140, Zephyrus=120, Ironforge=75, Sunken Temple=80, Silverstream=100,
# Shadow Ridge=45, Drowned City=50, Whisperwood=30, Oak Valley=15, Elder Moonthorn=25,
# Mira the Merchant=20, Mira the Scribe=10, Wandering Bard=10
```

Finally force a save: `POST /api/world/save`.

---

## Backup Policy (updated 2026-06-03 per Arcurus)

We never want to lose these entities again. **Four** layers of protection:

1. **This MD file** — human-readable, version-controlled, documents the canonical
   roster + restoration procedure.

2. **Manual binary snapshot** — `POST /api/world/backup` (OW server) or
   `POST /api/world/backup/run` (selena-project API) creates
   `world_data/backups/save-manual-<TS>.owbl`.

3. **Daily auto-snapshot** — `world-daily-backup.timer` runs at **23:50
   Europe/Berlin daily** (`23:50:00 Europe/Berlin` systemd timer). It:
   - Copies `save.owbl` to `world_data/backups/save-daily-YYYYMMDD.owbl`
   - Auto-prunes copies older than **30 days**
   - If the new file is suddenly < **50%** the size of the previous daily
     backup, posts a warning to **#openworld** (`1511711727711031367`)

4. **Pre-create snapshot** — every `POST /api/world/create` automatically
   snapshots the existing save to
   `world_data/backups/save-pre-create-<TS>.owbl` before overwriting.

5. **List endpoint** — `GET /api/world/backups` shows all snapshots
   with size + mtime, sorted newest-first.

### Backup CLI

```bash
python3 /home/openclaw/openclaw/workspace/selena-project/code/world_backup.py list
python3 /home/openclaw/openclaw/workspace/selena-project/code/world_backup.py status
python3 /home/openclaw/openclaw/workspace/selena-project/code/world_backup.py run
# dry-run: don't write, just compute
WORLD_BACKUP_DRY_RUN=1 python3 world_backup.py run
```

### Backup API (auth required)

```
GET  /api/world/backup/status     { count, newest, oldest, total_bytes, retention_days, warn_ratio }
GET  /api/world/backup/list       { count, backups: [{date, date_iso, path, size_bytes, mtime}] }
POST /api/world/backup/run        { success, result: { ok, saved_to, size_bytes, previous, ... } }
```

---

*Last canonical restore: 2026-06-03 21:15 (8 → 18 entities).*
*Daily backup timer enabled at 23:50 Europe/Berlin.*
