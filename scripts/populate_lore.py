#!/usr/bin/env python3
"""
Push the lore data in `scripts/lore_data.py` into the live world's
entities' `properties_string` under the four `lore_*` keys.

Idempotent. Re-running is safe: each field is only PUT if its current
value differs from the lore_data value. Skip-counts and update-counts
are reported at the end.

Auth: the open-world server's `verify_auth_cookie` (main.rs:7010)
checks for cookie `openworld_auth=1`. Sending the cookie from a
script is the documented dev-mode escape hatch (see
scripts/apply_faction_mapping.py for the same pattern).

Source-of-truth ordering: lore_data.py lists factions first, then
the dragon, the artifact, then major characters. Within each group
the order is *narrative importance*, not alphabetical. The auto-
generator (scripts/generate_lore_md.py) uses the same importance
order to render the MD, so this script's PUT order matters only for
log readability, not for correctness.

Conventions: see docs/lore-fields.md for the field schema.

Per Arcurus 2026-06-15 15:51 CEST in #openworld:
  "Lore-as-data integration sounds interesting. you can use our
   entities ability to add named string fields. then you can fill
   them. for sure Silverstream Keep + Whisperwood Forest should
   also be in. ... but please make a backup of the lore md."
"""
import json
import sys
import urllib.request
import urllib.error

# Pull the lore data from the sibling module
sys.path.insert(0, ".")
from lore_data import (
    IRONFORGE_CLAN,
    KEEPERS_OF_THE_ETERNAL_FLAME,
    THE_ASHEN_BROTHERS,
    THE_CRIMSON_VEIN,
    THE_BRIDGE_GUILD,
    THE_FREE_CARAVAN,
    THE_SILVER_ORDER,
    THE_THORNWATCH_RANGERS,
    THE_WHISPERING_COURT,
    THE_WHISPERING_ROOTS,
    THE_CIRCLE_OF_THE_GREEN,
    THE_HOLLOW_HAND,
    THE_BLACK_SAIL_REAVERS,
    VAELTHRIX_THE_ENDLESS,
    THE_SHADOW_CROWN,
    MIRA_THE_MERCHANT,
    MIRA_THE_SCRIBE,
    ZEPHYRUS_THE_ORACLE,
    KIRA_DAWNBLADE,
    SILVERSTREAM_KEEP,
    WHISPERWOOD_FOREST,
    THE_ASHEN_SPIRE,
    THE_DROWNED_CITY,
    THE_OLD_STANDING_STONES,
)

API = "http://localhost:8081/api"

# Variable name (in lore_data.py) -> live entity name.
# Kept explicit so a typo in either side fails loudly.
LORE_TO_ENTITY_NAME = {
    "IRONFORGE_CLAN": "Ironforge Clan",
    "KEEPERS_OF_THE_ETERNAL_FLAME": "Keepers of the Eternal Flame",
    "THE_ASHEN_BROTHERS": "The Ashen Brothers",
    "THE_CRIMSON_VEIN": "The Crimson Vein",
    "THE_BRIDGE_GUILD": "The Bridge Guild",
    "THE_FREE_CARAVAN": "The Free Caravan",
    "THE_SILVER_ORDER": "The Silver Order",
    "THE_THORNWATCH_RANGERS": "The Thornwatch Rangers",
    "THE_WHISPERING_COURT": "The Whispering Court",
    "THE_WHISPERING_ROOTS": "The Whispering Roots",
    "THE_CIRCLE_OF_THE_GREEN": "The Circle of the Green",
    "THE_HOLLOW_HAND": "The Hollow Hand",
    "THE_BLACK_SAIL_REAVERS": "The Black Sail Reavers",
    "VAELTHRIX_THE_ENDLESS": "Vaelthrix the Endless",
    "THE_SHADOW_CROWN": "The Shadow Crown",
    "MIRA_THE_MERCHANT": "Mira the Merchant",
    "MIRA_THE_SCRIBE": "Mira the Scribe",
    "ZEPHYRUS_THE_ORACLE": "Zephyrus the Oracle",
    "KIRA_DAWNBLADE": "Kira Dawnblade",
    "SILVERSTREAM_KEEP": "Silverstream Keep",
    "WHISPERWOOD_FOREST": "Whisperwood Forest",
    "THE_ASHEN_SPIRE": "The Ashen Spire",
    "THE_DROWNED_CITY": "The Drowned City",
    "THE_OLD_STANDING_STONES": "The Old Standing Stones",
}

LORE_DATA_BY_VAR = {
    "IRONFORGE_CLAN": IRONFORGE_CLAN,
    "KEEPERS_OF_THE_ETERNAL_FLAME": KEEPERS_OF_THE_ETERNAL_FLAME,
    "THE_ASHEN_BROTHERS": THE_ASHEN_BROTHERS,
    "THE_CRIMSON_VEIN": THE_CRIMSON_VEIN,
    "THE_BRIDGE_GUILD": THE_BRIDGE_GUILD,
    "THE_FREE_CARAVAN": THE_FREE_CARAVAN,
    "THE_SILVER_ORDER": THE_SILVER_ORDER,
    "THE_THORNWATCH_RANGERS": THE_THORNWATCH_RANGERS,
    "THE_WHISPERING_COURT": THE_WHISPERING_COURT,
    "THE_WHISPERING_ROOTS": THE_WHISPERING_ROOTS,
    "THE_CIRCLE_OF_THE_GREEN": THE_CIRCLE_OF_THE_GREEN,
    "THE_HOLLOW_HAND": THE_HOLLOW_HAND,
    "THE_BLACK_SAIL_REAVERS": THE_BLACK_SAIL_REAVERS,
    "VAELTHRIX_THE_ENDLESS": VAELTHRIX_THE_ENDLESS,
    "THE_SHADOW_CROWN": THE_SHADOW_CROWN,
    "MIRA_THE_MERCHANT": MIRA_THE_MERCHANT,
    "MIRA_THE_SCRIBE": MIRA_THE_SCRIBE,
    "ZEPHYRUS_THE_ORACLE": ZEPHYRUS_THE_ORACLE,
    "KIRA_DAWNBLADE": KIRA_DAWNBLADE,
    "SILVERSTREAM_KEEP": SILVERSTREAM_KEEP,
    "WHISPERWOOD_FOREST": WHISPERWOOD_FOREST,
    "THE_ASHEN_SPIRE": THE_ASHEN_SPIRE,
    "THE_DROWNED_CITY": THE_DROWNED_CITY,
    "THE_OLD_STANDING_STONES": THE_OLD_STANDING_STONES,
}

LORE_FIELDS = ["lore_summary", "lore_description", "lore_relationships", "lore_secrets"]


def fetch_entities() -> list:
    with urllib.request.urlopen(f"{API}/entities", timeout=5) as r:
        d = json.load(r)
    return d.get("data", [])


def put_string_property(entity_id: str, key: str, value: str) -> tuple[bool, str]:
    """PUT a single string property on an entity via the per-key endpoint.

    The bulk update endpoint (PUT /api/entities/<id>) does NOT accept
    properties_string (it's not in UpdateEntityRequest, see main.rs:2102).
    The per-key endpoint is
        PUT /api/entities/<id>/properties/string/<key>
    which expects a JSON-encoded `SetPropertyRequest`:
        {"value": "the string"}
    (the value field is PropertyValueJson::String, see main.rs:5924).
    """
    body = json.dumps({"value": value}).encode("utf-8")
    req = urllib.request.Request(
        f"{API}/entities/{entity_id}/properties/string/{key}",
        data=body,
        method="PUT",
        headers={
            "Content-Type": "application/json",
            "Cookie": "openworld_auth=1",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return (r.status == 200, f"HTTP {r.status}")
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        return (False, f"HTTP {e.code}: {body[:200]}")
    except Exception as e:
        return (False, f"exception: {e}")


def put_properties_string(entity_id: str, properties_string: dict) -> tuple[bool, str]:
    """PUT each key in properties_string via the per-key endpoint.

    The bulk endpoint (PUT /api/entities/<id>) does not accept
    properties_string (it's not in UpdateEntityRequest, see main.rs:2102),
    so we route through the per-key endpoint. Returns the first error
    encountered, or (True, summary) on success.
    """
    errors = []
    for k, v in properties_string.items():
        ok, msg = put_string_property(entity_id, k, v)
        if not ok:
            errors.append((k, msg))
    if errors:
        return (False, f"{len(errors)} field(s) failed: {errors[:3]}")
    return (True, f"PUT {len(properties_string)} field(s) via /properties/string/*")


def main():
    ents = fetch_entities()
    name_to_id = {e.get("name"): e.get("id") for e in ents}
    id_to_ent = {e.get("id"): e for e in ents}
    print(f"Loaded {len(ents)} entities.\n")

    # Sanity check: every lore entry should map to a live entity.
    missing = []
    for var, ent_name in LORE_TO_ENTITY_NAME.items():
        if ent_name not in name_to_id:
            missing.append((var, ent_name))
    if missing:
        print("ERROR: the following lore_data entries do not match any live entity:")
        for var, ent_name in missing:
            print(f"  - {var}  ->  '{ent_name}' (NOT FOUND)")
        sys.exit(1)

    # Sanity check: every lore entry should have all 4 fields.
    bad_fields = []
    for var, data in LORE_DATA_BY_VAR.items():
        for f in LORE_FIELDS:
            if f not in data:
                bad_fields.append((var, f))
    if bad_fields:
        print("ERROR: the following lore entries are missing fields:")
        for var, f in bad_fields:
            print(f"  - {var}  missing  '{f}'")
        sys.exit(1)

    print(f"Applying lore to {len(LORE_TO_ENTITY_NAME)} entities.\n")

    results = {
        "set": [],         # changed at least one field
        "unchanged": [],   # all fields already matched
        "error": [],
        "missing_entity": [],
    }
    for var, ent_name in LORE_TO_ENTITY_NAME.items():
        eid = name_to_id[ent_name]
        ent = id_to_ent[eid]
        current_ps = ent.get("properties_string") or {}
        target_ps = LORE_DATA_BY_VAR[var]

        # Determine which fields actually need updating.
        to_update = {}
        skipped = 0
        for f in LORE_FIELDS:
            current_val = current_ps.get(f)
            target_val = target_ps[f]
            if current_val == target_val:
                skipped += 1
            else:
                to_update[f] = target_val

        if not to_update:
            results["unchanged"].append((ent_name, var))
            print(f"  [SKIP]     {ent_name}  ({var})  -- all 4 fields already match")
            continue

        ok, msg = put_properties_string(eid, to_update)
        if ok:
            results["set"].append((ent_name, var, list(to_update.keys()), skipped))
            print(f"  [SET]      {ent_name}  ({var})  -> {len(to_update)} field(s) updated, {skipped} skipped  ({msg})")
            # Update local cache for re-runs
            id_to_ent[eid]["properties_string"] = {**current_ps, **to_update}
        else:
            results["error"].append((ent_name, var, msg))
            print(f"  [ERROR]    {ent_name}  ({var})  -> {msg}")

    print()
    print("=" * 70)
    print(f"Summary: {len(results['set'])} entities updated, "
          f"{len(results['unchanged'])} unchanged, "
          f"{len(results['error'])} errors")
    print("=" * 70)

    if results["error"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
