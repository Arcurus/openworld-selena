# v1→v2 Faction Mapping — applied 2026-06-15

Applied via `scripts/apply_faction_mapping.py` (idempotent, re-runnable).
Source: docs/world_entities.md is **STALE** — it lists entities/factions that
do not exist in the live server.  This mapping uses literal-name and strong
thematic matches against the LIVE entity list.  Conflicts were left as None.

## Stats: 17 of 50 entities assigned, 32 left as None.

## Faction entity count: 13
## Faction IDs (the 4 in docs are wrong; live has these 13):
  - Ironforge Clan  (f5f1913d-bcb0-4770-a296-01c2d7304eec)
  - Keepers of the Eternal Flame  (20bda28a-7076-4c2c-8ac5-b5447ab701d2)
  - Test Faction  (f0b611c5-925a-4a41-898d-fe40c2bc8eae)
  - The Ashen Brothers  (d78840de-0efe-4b67-8962-b41b87129e16)
  - The Black Sail Reavers  (f7bbb108-86cc-42a8-8091-a3c0eeb6b9ff)
  - The Bridge Guild  (20a41ba2-cd3d-4b4d-ae93-83bfb86eec44)
  - The Circle of the Green  (940cca93-1500-46f7-b145-2a408fa21af2)
  - The Crimson Vein  (b4505208-45ac-43cb-86ae-b441ab319297)
  - The Free Caravan  (d3a0079e-aac0-4a17-8503-55e8a83517d6)
  - The Hollow Hand  (857f3d11-52b8-4967-8038-137a21c94125)
  - The Silver Order  (fe1b3558-a3dd-4ddd-b09f-d45f7acfff4b)
  - The Thornwatch Rangers  (7846b5c3-4067-43d7-9b74-f257274d28e1)
  - The Whispering Court  (8f2ca107-bbbb-4df9-874b-ec520e644b4d)
  - The Whispering Roots  (de58a907-68a8-480d-8838-866f4d0a388f)

## Assignments applied:

| Entity | Type | Faction | Rationale |
|---|---|---|---|
| Elder Moonthorn | character | The Circle of the Green | elder + nature |
| Kira Dawnblade | character | The Thornwatch Rangers | blade → ranger |
| Zephyrus the Oracle | character | The Whispering Roots | wind god → roots |
| Brackenhold Mine | location | Ironforge Clan | mine → forge clan |
| Greenmark Farm | location | The Circle of the Green | literal name match |
| Saltcliff Cove | location | The Black Sail Reavers | coastal → sea raiders |
| Shadow Ridge Camp | location | The Thornwatch Rangers | ridge/camp → rangers |
| Silverstream Keep | location | Ironforge Clan | keep → forge clan |
| The Ashen Spire | location | Keepers of the Eternal Flame | flame + ashen tightens the loop |
| The Drowned City | location | The Hollow Hand | spectral/drowned → dark cabal |
| The Hollow Crypt | location | The Hollow Hand | literal name match |
| The Old Standing Stones | location | The Whispering Court | stone circle → court |
| The Silverleaf Sanctuary | location | The Circle of the Green | nature/leaf → green |
| The Stonebridge | location | The Bridge Guild | literal name match |
| The Wreck of the Black Sails | location | The Black Sail Reavers | literal name match |
| Whisperwood Forest | location | The Whispering Roots | whisper + root/wood |
| Willowfen Marsh | location | The Whispering Roots | wetland → roots |

## Not assigned (None) — 33 entities

Conflict cases (location could fit 2+ factions, intentionally left None):
  - Crossroads Market  (Bridge Guild vs. Free Caravan)
  - Saltcliff Cove  (Black Sail Reavers also) — wait, actually assigned above. Skip.
  - The Stonebridge  (Bridge Guild) — assigned above. Skip.

All faction entities themselves have faction_id=None (factions are not members of other factions).
The Hollow Hand, Ironforge Clan, etc. — the faction entity itself does not have a parent faction.

## Other v2 fields — all None for everyone

No lore basis for any of these in the live world:
  - leader_id: docs mention "Lord Cassian Drove" + "Lyra Shadowmend" as leaders,
    but those entities do not exist in the live world.  No clear leaders for the
    live factions.
  - home_location_id, birth_location_id: no lore basis for any character.
  - faction_secret_loyal_id: no lore basis for hidden loyalties.
  - region_id: per Arcurus 2026-06-15, no region created yet.

## TODO: lore-mapping session

Suggested follow-up: a focused 30-min session where we read the live world
entities, decide which faction each belongs to (and which faction leaders are),
and update this file with proper lore-based assignments.
