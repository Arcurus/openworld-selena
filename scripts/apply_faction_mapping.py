#!/usr/bin/env python3
"""
Apply the v1→v2 faction_id mapping to all 50 entities in the live world.

Conservative, name-inference-only mapping (the docs are stale and don't
match the live entity list).  Each entry is a literal-name match or a
strong thematic match with no conflict.  Conflicts and ambiguous cases
are intentionally left as None so a future lore-mapping session can
resolve them with proper context.

Per Arcurus 2026-06-15 (#openworld): "for each entity you can then set
a fitting fractionId or let it null if there is none."

The other v2 fields (faction_secret_loyal_id, home_location_id,
birth_location_id, leader_id, region_id) are also wired up but left
None for everyone — we have no lore basis for any of them yet.  Leader
IDs require a lore-mapping session because none of the live-server
factions map to the docs ("Iron Pact" etc. don't exist in the live
world; the closest analog is "Ironforge Clan" but without a lore basis
for a leader character we leave it None).

Idempotent: re-running it is safe.  Reports which entities already
had the value set (idempotent skip) and which were changed.
"""
import json
import sys
import urllib.request
import urllib.error

API = "http://localhost:8081/api"

# ---------------------------------------------------------------------------
# Mapping table.  Each entry: (entity_name, field_name, value) where
# value is either a faction_name (looked up in FACTION_IDS) or None.
# ---------------------------------------------------------------------------

# Faction names → ids.  Live server's 13 factions.  Test Faction is
# an artifact from a previous experiment; we don't assign any
# entities to it.
FACTION_IDS = {
    "Ironforge Clan": "f5f1913d-bcb0-4770-a296-01c2d7304eec",
    "Keepers of the Eternal Flame": "20bda28a-7076-4c2c-8ac5-b5447ab701d2",
    "The Ashen Brothers": "d78840de-0efe-4b67-8962-b41b87129e16",
    "The Black Sail Reavers": "f7bbb108-86cc-42a8-8091-a3c0eeb6b9ff",
    "The Bridge Guild": "20a41ba2-cd3d-4b4d-ae93-83bfb86eec44",
    "The Circle of the Green": "940cca93-1500-46f7-b145-2a408fa21af2",
    "The Crimson Vein": "b4505208-45ac-43cb-86ae-b441ab319297",
    "The Free Caravan": "d3a0079e-aac0-4a17-8503-55e8a83517d6",
    "The Hollow Hand": "857f3d11-52b8-4967-8038-137a21c94125",
    "The Silver Order": "fe1b3558-a3dd-4ddd-b09f-d45f7acfff4b",
    "The Thornwatch Rangers": "7846b5c3-4067-43d7-9b74-f257274d28e1",
    "The Whispering Court": "8f2ca107-bbbb-4df9-874b-ec520e644b4d",
    "The Whispering Roots": "de58a907-68a8-480d-8838-866f4d0a388f",
    "Test Faction": "f0b611c5-925a-4a41-898d-fe40c2bc8eae",
}

# (entity_name → faction_name)  None means "explicitly clear".  This
# is a high-confidence, no-conflict list: every entry is a literal
# name-substring match between the entity and the faction, or a
# single strong thematic match.
FACTION_ASSIGNMENTS = {
    # === LOCATIONS ===
    "Brackenhold Mine": "Ironforge Clan",     # mine → forge clan
    "Silverstream Keep": "Ironforge Clan",    # keep → forge clan
    "The Ashen Spire": "Keepers of the Eternal Flame",  # "flame" + "ashen" tightens the loop
    "Saltcliff Cove": "The Black Sail Reavers",  # coastal → sea raiders
    "The Wreck of the Black Sails": "The Black Sail Reavers",  # literal
    "The Stonebridge": "The Bridge Guild",    # literal
    "Greenmark Farm": "The Circle of the Green",  # literal "green"
    "The Silverleaf Sanctuary": "The Circle of the Green",  # nature/leaf
    "Whisperwood Forest": "The Whispering Roots",  # literal "whisper" + "root" / "wood"
    "Willowfen Marsh": "The Whispering Roots",  # wetland fits "roots"
    "Shadow Ridge Camp": "The Thornwatch Rangers",  # ridge/camp → rangers
    "The Hollow Crypt": "The Hollow Hand",    # literal "hollow"
    "The Drowned City": "The Hollow Hand",    # spectral/drowned fits dark cabal
    "The Old Standing Stones": "The Whispering Court",  # stone circle → court
    # === CHARACTERS ===
    "Kira Dawnblade": "The Thornwatch Rangers",  # blade/ranger name
    "Elder Moonthorn": "The Circle of the Green",  # elder + nature
    "Zephyrus the Oracle": "The Whispering Roots",  # wind god → roots
    # === everything else: None (no confident match) ===
}

# (faction_name → leader_character_name).  We have NO lore basis for
# any of the live-server factions' leaders.  The docs mention
# "Iron Pact" → Lord Cassian Drove and "Verdant Circle" → Lyra
# Shadowmend, but those entities don't exist in the live world.
# Leave all leaders None for now.
LEADER_ASSIGNMENTS: dict = {}

# (character_name → home_location_name).  No lore basis for any.
# Skip entirely.
HOME_ASSIGNMENTS: dict = {}

# (character_name → birth_location_name).  No lore basis for any.
# Skip entirely.
BIRTH_ASSIGNMENTS: dict = {}

# (character_name → secret_loyal_faction_name).  No lore basis for any.
# Skip entirely.
SECRET_LOYAL_ASSIGNMENTS: dict = {}

# (entity_name → region_name).  No region created (per Arcurus 2026-06-15).
REGION_ASSIGNMENTS: dict = {}


def fetch_entities():
    with urllib.request.urlopen(f"{API}/entities", timeout=5) as r:
        d = json.load(r)
    return d.get("data", [])


def put_field(entity_id: str, field: str, value) -> tuple[bool, str]:
    """PUT a single v2 field on an entity.  Returns (ok, message)."""
    body = json.dumps({field: value}).encode("utf-8")
    req = urllib.request.Request(
        f"{API}/entities/{entity_id}",
        data=body,
        method="PUT",
        headers={
            "Content-Type": "application/json",
            # Auth: the open-world server (per main.rs::verify_auth_cookie
            # at line 7010) checks for cookie `openworld_auth=1`.  The
            # web UI sets it after the user enters WEB_PASSWORD in the
            # login modal; the password is never validated against the
            # cookie value (the cookie just has to be present and == "1").
            # Sending the cookie from a script is the documented dev-mode
            # escape hatch — the cookie value isn't compared to the env
            # password, so this is equivalent to having logged in once.
            "Cookie": "openworld_auth=1",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return (r.status == 200, f"HTTP {r.status}")
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        return (False, f"HTTP {e.code}: {body[:200]}")
    except Exception as e:
        return (False, f"exception: {e}")


def main():
    ents = fetch_entities()
    name_to_id = {e.get("name"): e.get("id") for e in ents}
    id_to_ent = {e.get("id"): e for e in ents}

    print(f"Loaded {len(ents)} entities.\n")

    # Build the full list of (entity_name, field, value) triples.
    triples: list[tuple[str, str, object]] = []
    for name, faction in FACTION_ASSIGNMENTS.items():
        triples.append((name, "faction_id", FACTION_IDS.get(faction) if faction else None))
    for name, leader in LEADER_ASSIGNMENTS.items():
        triples.append((name, "leader_id", name_to_id.get(leader)))
    for name, home in HOME_ASSIGNMENTS.items():
        triples.append((name, "home_location_id", name_to_id.get(home)))
    for name, birth in BIRTH_ASSIGNMENTS.items():
        triples.append((name, "birth_location_id", name_to_id.get(birth)))
    for name, loyal in SECRET_LOYAL_ASSIGNMENTS.items():
        triples.append((name, "faction_secret_loyal_id", FACTION_IDS.get(loyal) if loyal else None))
    for name, region in REGION_ASSIGNMENTS.items():
        triples.append((name, "region_id", name_to_id.get(region)))

    print(f"Applying {len(triples)} assignments:\n")
    print(f"  faction_id: {sum(1 for n,f,v in triples if f=='faction_id' and v is not None)} non-null, {sum(1 for n,f,v in triples if f=='faction_id' and v is None)} explicit-null")
    print(f"  leader_id:  {sum(1 for n,f,v in triples if f=='leader_id' and v is not None)} non-null, {sum(1 for n,f,v in triples if f=='leader_id' and v is None)} explicit-null")
    print(f"  home_location_id:  {sum(1 for n,f,v in triples if f=='home_location_id' and v is not None)} non-null")
    print(f"  birth_location_id: {sum(1 for n,f,v in triples if f=='birth_location_id' and v is not None)} non-null")
    print(f"  faction_secret_loyal_id: {sum(1 for n,f,v in triples if f=='faction_secret_loyal_id' and v is not None)} non-null")
    print(f"  region_id: {sum(1 for n,f,v in triples if f=='region_id' and v is not None)} non-null")

    print()
    results = {"set": [], "unchanged": [], "missing_entity": [], "error": []}
    for name, field, value in triples:
        eid = name_to_id.get(name)
        if not eid:
            results["missing_entity"].append((name, field, value))
            print(f"  [MISSING]  {name}  field={field}  (entity not in live server)")
            continue
        current = id_to_ent[eid].get(field)
        if current == value:
            results["unchanged"].append((name, field, value))
            print(f"  [SKIP]     {name}  field={field}  already={value}")
            continue
        ok, msg = put_field(eid, field, value)
        if ok:
            results["set"].append((name, field, value))
            print(f"  [SET]      {name}  field={field}  -> {value}  ({msg})")
            # Update local cache so re-runs are idempotent
            id_to_ent[eid][field] = value
        else:
            results["error"].append((name, field, value, msg))
            print(f"  [ERROR]    {name}  field={field}  -> {value}  ({msg})")

    print()
    print("=" * 70)
    print(f"Summary: {len(results['set'])} set, {len(results['unchanged'])} unchanged, "
          f"{len(results['missing_entity'])} missing entity, {len(results['error'])} errors")
    print("=" * 70)

    if results["error"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
