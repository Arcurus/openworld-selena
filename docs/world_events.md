# World Events

*Active world events that influence entity behavior. Live state: `GET /api/world/events`. Persisted to `world_data/save.owbl` and `docs/world_lore.md`.*

*Last update: 2026-06-06 03:23 CEST — 6 active events.*

---

## Current Active Events (6)

### 1. The Shadow Awakens
**ID:** `e10f8432-2dbe-4b73-9826-2366c7772c9f`
**Status:** ACTIVE

**Description:**
For centuries dismissed as myth, the Scrolls of the First Age spoke of a darkness that would return when the realm forgot its vigilance. Now the signs are undeniable: shadows in the Northern Pass stretch longer than the mountains themselves, animals flee southward, and the Moonwell at Elder Moonthorn reflects something other than the future. The Prophecy of the Shadow Crown has begun to unfold.

**How this affects entities:**
Entities grow suspicious, militaristic, and watchful. Silverstream Keep mobilizes. Ironforge forges weapons day and night. Whisperwood closes its borders. Trade becomes riskier. Trust between factions erodes. Power-hungry actors see opportunity. The realm is tense with approaching doom.

---

### 2. Velora Walks Again
**ID:** `c7eca4b6-8dc8-45ba-ba5a-337803de3019`
**Status:** ACTIVE

**Description:**
A knight in corroded silver armor has been sighted on the roads at night. Her helm reflects no light and she leaves no shadow. Velora the Undying, who held the Northern Pass alone for seven days during the Demon Wars, has returned. She seeks the Forgotten Heir mentioned in the prophecy and trades secrets with those brave enough to meet her gaze.

**How this affects entities:**
Heroes and knights feel a stirring of destiny. Some seek Velora out for blessings. Others fear her appearance as a sign of the worst. Kira Dawnblade in particular feels the prophecy pulling at her. Mira the Merchant has rare tales to sell. The Silver Wardens of Silverstream Keep sense the return of their founder.

---

### 3. The Shadowmaw Stirs
**ID:** `a23dac23-4fd1-4936-9696-059cae6ce77d`
**Status:** ACTIVE

**Description:**
Ironforge miners report tremors deep beneath Frostpeak. The forges have grown hot without fuel. The clan elders whisper of bad dreams — impossible dreams of black wings and a heartbeat that shakes the world. Vaelthrix the Endless, the ancient dragon who slept beneath the Frostpeak Mountains before the First Age, has begun to dream. Her dreams leak into the world as visions and earthquakes.

**How this affects entities:**
Dwarves of Ironforge grow fearful but resolute. Miners dig deeper in search of ancient weapons. Mountain-dwelling entities feel the tremors. The wandering bard hears songs about dragons returning. Some interpret the dreams as omens; others as opportunities. The realm feels heavier, charged with waiting.

---

### 4. The Silver Wardens Mobilize
**ID:** `88f129bd-2c08-4f23-9969-4818d3858bfd`
**Status:** ACTIVE

**Description:**
The banners of Silverstream Keep fly from every tower. Knights ride out in pairs along the northern roads. A formal decree has been issued: every traveler must declare their business or be turned back. The Silver Wardens — Silverstream Keep's elite order — believe themselves the prophesied defenders of the realm. They have begun recruiting among the common folk, and the cost of admission is a secret they will not share.

**How this affects entities:**
Knights and warriors grow bold. Refugees and villagers consider joining. Bandits and outlaws grow more cautious. The Keep itself grows in power, but at the cost of internal suspicion. The mobilization of one faction pressures all others — should they also prepare for war? Trade slows. Tensions rise along every road.

---

### 5. The Bells of the Sunken Temple
**ID:** `46a976d2-c2a7-46b5-903f-1a04ae751058`
**Status:** ACTIVE

**Description:**
Travelers near the southern marshlands report hearing bells at dusk. The Sunken Temple — half-submerged since the Second Age and abandoned for a thousand years — has begun to ring. No one has yet dared enter. The Wandering Bard claims to have heard a voice singing along with the bells, in a language no scholar recognizes. Mira the Scribe is taking notes.

**How this affects entities:**
Scholars and sages grow curious. Adventurers plan expeditions. Locals avoid the marshlands. Zephyrus the Oracle speaks in riddles about it, which everyone interprets differently. The realm feels as if something is waking that was meant to stay asleep. The Drowned City, said to be the temple sister, has grown quieter — its silence more ominous than its noise.

---

### 6. The Spring Festival of Renewal
**ID:** `88ee73fc-69cb-4366-a5ea-481aa175cfab`
**Status:** ACTIVE

**Description:**
Despite the spreading shadow, the villages of the realm gather in the Oak Valley green at the height of spring to celebrate survival itself. For three days and nights, the folk of Oak Valley, Silverstream, and the Ironforge trade roads open their gates, lay down old grudges, and remember that hope is something you must tend like a fire. Bards sing, children run free, and even the Shadow Crown's reach seems—impossibly—a little lighter when every hearth in the valley burns at once. Mira the Scribe calls it the only honest currency: shared bread, shared song, shared laughter in the dark.

**How this affects entities:**
Trade flows more freely for the festival's duration. Factional suspicion eases; the Silver Wardens soften their patrols and even share a cup with passing rangers. Children play without fear, and the realm's surviving heroes feel their burdens lifted. Entities grow reflective rather than reactive, planning for a future they had nearly given up on. The Wandering Bard calls it 'the stubborn ember.' Ironforge forges glow warmer for the celebration. Kira Dawnblade attends for the first time in years.

---

## How Events Influence the LLM

Events are inserted into the LLM context for every entity action via `build_world_events_str` in `src/world_data/context_builder.rs:151`. The `influence` field is the key driver: it tells the LLM how the event should shape entity decisions.

Example (rendered for a Silverstream Keep action):
```
## Active World Events

### The Shadow Awakens
For centuries dismissed as myth...

**How this affects entities:** Entities grow suspicious, militaristic...
```

See `src/world_data/context_builder.rs` for the exact rendering.

## Operations

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/world/events` | List all events (no auth) |
| POST | `/api/world/events` | Add a new event (auth) |
| PUT | `/api/world/events/:id` | Update an event (auth) |
| DELETE | `/api/world/events/:id` | Delete an event (auth) |
| POST | `/api/world/save` | Persist to save.owbl (auth) |

Auth: `Cookie: openworld_auth=1` (set after `POST /api/env/verify-password` with the WEB_PASSWORD).

## Future Ideas

- Event chaining: one event can trigger/silence another when conditions are met.
- Event decay: events can fade over time (active=false after N world days).
- Event-driven quest hooks: a `WorldEvent` with `triggers_quest: true` could spawn related entity actions.
- Lore cross-references: link events to entities via `related_entities: [entity_id, ...]`.
