#!/usr/bin/env python3
"""
Render `docs/world_lore.md` from the lore fields on each entity in the
live world. The MD is a *render* of `properties_string.lore_*`, not a
hand-maintained doc.

Source schema: docs/lore-fields.md
Population:    scripts/populate_lore.py
This script:   scripts/generate_lore_md.py  ← you are here

Re-runnable. Idempotent. Replaces the file in full (not a diff/merge).

Output structure:
  # World Lore: <world name>
  ## Overview
  ## ⚔️ THE SHADOW AWAKENING (preserved from the prior hand-written MD)
  ## Factions
  ### <name>
  ...
  ## Locations
  ## Characters
  ## Dragon
  ## Artifacts
  ## TBD / no lore yet
  ## Last generated

Entities with no `lore_*` fields are listed under "TBD / no lore yet"
so the doc makes the gap visible. Add lore to an entity by setting
its `properties_string.lore_*` keys (use populate_lore.py or curl)
and re-run this script.
"""
import json
import sys
import urllib.request
import datetime
from pathlib import Path

API = "http://localhost:8081/api"
OUTPUT = Path(__file__).resolve().parent.parent / "docs" / "world_lore.md"

# Order of entity types in the rendered MD (matches lore-fields.md spec).
TYPE_ORDER = ["faction", "location", "character", "dragon", "artifact", "abstract"]

# Type heading labels for the rendered MD.
TYPE_HEADING = {
    "faction": "⚔️ Factions",
    "location": "🗺️ Locations",
    "character": "👤 Characters",
    "dragon": "🐉 The Dragon",
    "artifact": "💎 Artifacts",
    "abstract": "🌐 Abstract",
}

# Per-type importance order: factions first (most narrative weight),
# then characters, then locations, etc. Within a type the entities
# with lore_summary come first (in API-returned order), then entities
# without lore fall into the TBD section.
#
# Per-type importance lists. These override the API-returned order
# for the entities with lore, so the MD reads in narrative-priority
# order. Entities not in these lists keep their API order.
# Names match docs/lore-fields.md "what's in properties_string today?"
# section.
FACTION_ORDER = [
    "Ironforge Clan",
    "Keepers of the Eternal Flame",
    "The Ashen Brothers",
    "The Crimson Vein",
    "The Bridge Guild",
    "The Free Caravan",
    "The Silver Order",
    "The Thornwatch Rangers",
    "The Whispering Court",
    "The Whispering Roots",
    "The Circle of the Green",
    "The Hollow Hand",
    "The Black Sail Reavers",
]
CHARACTER_ORDER = [
    "Vaelthrix the Endless",  # dragon, listed under dragon
    "The Shadow Crown",        # artifact, listed under artifact
    "Mira the Merchant",
    "Mira the Scribe",
    "Zephyrus the Oracle",
    "Kira Dawnblade",
    "Elder Moonthorn",
    "The Wandering Bard",
    "Velora the Undying",
]
# Locations are not in the initial lore_data pass; the design said
# "the 10 most lore-worthy locations" — when populate_lore grows
# entries for them, add them here in importance order.
#
# First pass (2026-06-16, selena-open-world-worker): Silverstream Keep
# and Whisperwood Forest were explicitly named by Arcurus 2026-06-15.
# We added 3 more narrative-anchor locations (The Ashen Spire, The
# Drowned City, The Old Standing Stones) — all of which are referenced
# across many existing faction/character secrets — for a total of 5
# in the first location pass. The remaining 21 locations stay in the
# TBD section until a later pass.
LOCATION_ORDER = [
    # Batch 1 (2026-06-15) — 5 narrative-anchor locations
    "Silverstream Keep",
    "Whisperwood Forest",
    "The Ashen Spire",
    "The Drowned City",
    "The Old Standing Stones",
    # Batch 2 (2026-06-16) — 21 remaining locations, in narrative importance order
    "The Stonebridge",
    "Saltcliff Cove",
    "The Old Battlefield",
    "Hollowmere Village",
    "Brackenhold Mine",
    "Shadow Ridge Camp",
    "The Hollow Crypt",
    "Crossroads Market",
    "Willowfen Marsh",
    "Greenmark Farm",
    "The Wanderer's Rest",
    "The Silverleaf Sanctuary",
    "The Sunken Temple",
    "The Dawnwatch Lighthouse",
    "The Wreck of the Black Sails",
    "The Howling Ravine",
    "The Bone Orchard",
    "Oak Valley Village",
    "The Witchfen",
    "Pinegrove Logging Camp",
    "The Bleakmoor",
]
DRAGON_ORDER = ["Vaelthrix the Endless"]
ARTIFACT_ORDER = ["The Shadow Crown"]


def fetch_entities() -> list:
    with urllib.request.urlopen(f"{API}/entities", timeout=5) as r:
        d = json.load(r)
    return d.get("data", [])


def fetch_world() -> dict:
    with urllib.request.urlopen(f"{API}/world", timeout=5) as r:
        return json.load(r).get("data", {})


def has_lore(ent: dict) -> bool:
    ps = ent.get("properties_string") or {}
    return any(k.startswith("lore_") for k in ps.keys())


def get_lore(ent: dict) -> dict:
    """Return the lore_* fields that are set on this entity."""
    ps = ent.get("properties_string") or {}
    return {k: v for k, v in ps.items() if k.startswith("lore_") and v}


def render_entity(ent: dict) -> str:
    """Render a single entity's section in the MD."""
    name = ent.get("name", "Unnamed")
    lore = get_lore(ent)
    et = ent.get("entity_type", "?")

    out = [f"### {name}", ""]
    out.append(f"**Type:** {et.title()}")
    if ent.get("x") is not None and ent.get("y") is not None:
        out.append(f" | **Location:** ({ent['x']:.0f}, {ent['y']:.0f})")
    out.append("")

    if "lore_summary" in lore:
        out.append(f"*{lore['lore_summary']}*")
        out.append("")

    if "lore_description" in lore:
        out.append(lore["lore_description"])
        out.append("")

    if "lore_relationships" in lore:
        out.append(f"**Relationships:** {lore['lore_relationships']}")
        out.append("")

    if "lore_secrets" in lore:
        # "Secrets" for most types, "Dangers" for locations.
        section_name = "Dangers" if et == "location" else "Secrets"
        out.append(f"**{section_name}:** {lore['lore_secrets']}")
        out.append("")

    return "\n".join(out)


def sort_by_order(ents: list, order: list) -> list:
    """Sort entities so that names in `order` come first (in that order),
    with any not-in-order names following in their original order."""
    by_name = {e.get("name"): e for e in ents}
    sorted_ents = []
    seen = set()
    for n in order:
        if n in by_name:
            sorted_ents.append(by_name[n])
            seen.add(n)
    for e in ents:
        if e.get("name") not in seen:
            sorted_ents.append(e)
    return sorted_ents


def render_md(ents: list, world: dict) -> str:
    """Render the full world_lore.md."""
    world_name = world.get("name", "The Realm of Aethermoor")
    world_desc = world.get("description", "")
    now = datetime.datetime.now().strftime("%Y-%m-%d %H:%M CEST")
    counts = {t: 0 for t in TYPE_ORDER}
    has_lore_counts = {t: 0 for t in TYPE_ORDER}
    for e in ents:
        et = e.get("entity_type", "abstract")
        if et in counts:
            counts[et] += 1
            if has_lore(e):
                has_lore_counts[et] += 1

    out = []
    out.append(f"# World Lore: {world_name}")
    out.append("")

    # ----- Overview -----
    out.append("## Overview")
    out.append("")
    if world_desc:
        out.append(world_desc.strip())
        out.append("")
    out.append(
        "This document is **auto-generated** from each entity's "
        "`properties_string.lore_*` fields. To update the lore, edit "
        "the entity's `properties_string` and re-run "
        "`scripts/generate_lore_md.py`."
    )
    out.append("")

    # ----- Entity counts (visible gap) -----
    out.append("**Entity counts:**")
    for t in TYPE_ORDER:
        if counts[t] > 0:
            out.append(
                f"- **{TYPE_HEADING[t].split(' ', 1)[-1]}**: "
                f"{counts[t]} total, {has_lore_counts[t]} with lore"
            )
    out.append("")

    # ----- The Shadow Awakening (preserved section, hand-written) -----
    # NOTE: this section is hand-maintained and lives in a fixed
    # "shadow_awakening.md" stub. We only emit a header pointer and
    # a short summary here. The full text is rendered separately by
    # whoever owns it. If you need to update the canonical text, edit
    # scripts/shadow_awakening_stub.md and re-run.
    out.append("## ⚔️ THE SHADOW AWAKENING (Current World Event)")
    out.append("")
    out.append(
        "*Year 850 — The Present Day.* The realm stands at the edge of a "
        "long-prophesied darkness. The full narrative of the Shadow "
        "Awakening is hand-maintained; this generator preserves a pointer "
        "rather than a render. See `scripts/shadow_awakening_stub.md` for "
        "the source of the canonical text."
    )
    out.append("")

    # ----- Per-type sections -----
    tbd_section = []
    for t in TYPE_ORDER:
        type_ents = [e for e in ents if e.get("entity_type") == t]
        type_ents_with_lore = [e for e in type_ents if has_lore(e)]
        type_ents_without = [e for e in type_ents if not has_lore(e)]
        if not type_ents_with_lore and not type_ents_without:
            continue

        out.append(f"## {TYPE_HEADING[t]}")
        out.append("")

        if t == "faction":
            type_ents_with_lore = sort_by_order(type_ents_with_lore, FACTION_ORDER)
        elif t == "character":
            type_ents_with_lore = sort_by_order(type_ents_with_lore, CHARACTER_ORDER)
        elif t == "location":
            type_ents_with_lore = sort_by_order(type_ents_with_lore, LOCATION_ORDER)
        elif t == "dragon":
            type_ents_with_lore = sort_by_order(type_ents_with_lore, DRAGON_ORDER)
        elif t == "artifact":
            type_ents_with_lore = sort_by_order(type_ents_with_lore, ARTIFACT_ORDER)

        for e in type_ents_with_lore:
            out.append(render_entity(e))

        # Entities without lore fall into the TBD section.
        for e in type_ents_without:
            tbd_section.append((t, e))

    # ----- TBD section -----
    if tbd_section:
        out.append("## TBD / no lore yet")
        out.append("")
        out.append(
            "The following entities have no `lore_*` fields yet. "
            "Add lore to them by setting `properties_string.lore_*` "
            "and re-running `scripts/generate_lore_md.py`."
        )
        out.append("")
        # Group by type for readability
        by_type = {}
        for t, e in tbd_section:
            by_type.setdefault(t, []).append(e)
        for t in TYPE_ORDER:
            if t in by_type:
                out.append(f"### {TYPE_HEADING[t].split(' ', 1)[-1]} ({len(by_type[t])})")
                out.append("")
                for e in by_type[t]:
                    out.append(f"- {e.get('name', '?')}")
                out.append("")

    # ----- Footer -----
    out.append("---")
    out.append("")
    out.append(
        f"*Last generated: {now} — by `scripts/generate_lore_md.py`. "
        f"Source: {sum(1 for e in ents if has_lore(e))}/{len(ents)} entities "
        f"with lore.*"
    )
    out.append("")

    return "\n".join(out)


def main():
    ents = fetch_entities()
    world = fetch_world()
    print(f"Loaded {len(ents)} entities and world '{world.get('name', '?')}'.")

    md = render_md(ents, world)
    OUTPUT.write_text(md, encoding="utf-8")
    with_lore = sum(1 for e in ents if has_lore(e))
    print(f"Wrote {OUTPUT} ({len(md):,} bytes; {with_lore}/{len(ents)} entities with lore).")


if __name__ == "__main__":
    main()
