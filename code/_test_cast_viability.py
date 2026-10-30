#!/usr/bin/env python3
"""Smoke test for the `cast_viability` scan in ow_sanity_check.

Verifies end-to-end that:
  1. is_system_entity() mirrors the server's predicate (world_clock type,
     abstract type, 'meta' tag) — the same rule src/scheduler.rs uses to
     pick actors.
  2. scan_cast_viability() reports a healthy world as viable.
  3. scan_cast_viability() reports the 2026-09-07 DEAD WORLD shape
     (only the World Clock) as NOT viable — the condition that went
     unnoticed for seven weeks.
  4. An empty entity list is NOT viable.
  5. render_report() emits the DEAD WORLD banner for a dead world and the
     "cast viable" line for a healthy one — so the finding actually
     reaches a human.
"""
import os
import sys

sys.path.insert(0, "/home/openclaw/openclaw/workspace/open-world-selena/code")

from ow_sanity_check import (
    is_system_entity,
    scan_cast_viability,
    render_report,
)

FAILS = []


def check(name, cond, detail=""):
    if cond:
        print(f"  ✅ {name}")
    else:
        print(f"  ❌ {name} — {detail}")
        FAILS.append(name)


CLOCK = {
    "id": "clock-1", "name": "World Clock", "entity_type": "abstract",
    "tags": ["meta"], "history_summary": "x" * 100,
    "history": [{"action": "tick", "timestamp": "2026-10-30T00:00:00Z"}],
}
KIRA = {
    "id": "e1", "name": "Kira Dawnblade", "entity_type": "character",
    "tags": ["hero"], "history_summary": "y" * 100,
    "history": [{"action": "duel", "timestamp": "2026-10-30T00:00:00Z"}],
}
VAEL = {
    "id": "e2", "name": "Vaelthrix the Endless", "entity_type": "dragon",
    "tags": [], "history_summary": "z" * 100, "history": [],
}

print("== 1. is_system_entity() ==")
check("world_clock type is system", is_system_entity({"entity_type": "world_clock"}))
check("abstract type is system", is_system_entity({"entity_type": "abstract"}))
check("meta tag is system", is_system_entity({"entity_type": "character", "tags": ["meta"]}))
check("meta tag as bare string is system",
      is_system_entity({"entity_type": "character", "tags": "meta"}))
check("character is NOT system", not is_system_entity(KIRA))
check("dragon is NOT system", not is_system_entity(VAEL))

print("== 2. healthy world is viable ==")
v = scan_cast_viability([CLOCK, KIRA, VAEL])
check("n_total == 3", v["n_total"] == 3, str(v))
check("n_system == 1", v["n_system"] == 1, str(v))
check("n_eligible == 2", v["n_eligible"] == 2, str(v))
check("viable is True", v["viable"] is True, str(v))

print("== 3. the 2026-09-07 dead-world shape is NOT viable ==")
v = scan_cast_viability([CLOCK])
check("n_total == 1", v["n_total"] == 1, str(v))
check("n_system == 1", v["n_system"] == 1, str(v))
check("n_eligible == 0", v["n_eligible"] == 0, str(v))
check("viable is False", v["viable"] is False, str(v))
check("system_examples names the clock",
      v["system_examples"] == ["World Clock"], str(v))

print("== 4. empty entity list is NOT viable ==")
v = scan_cast_viability([])
check("n_eligible == 0", v["n_eligible"] == 0, str(v))
check("viable is False", v["viable"] is False, str(v))

print("== 5. the finding reaches the report ==")


def _lengths(n):
    return {"n_entities": n, "n_with_summary": n, "n_without_summary": 0,
            "n_over_cap": 0, "median_chars": 100,
            "longest": {"name": "World Clock", "type": "abstract", "chars": 100},
            "over_cap_examples": [], "no_summary_examples": []}


dead = render_report([], {"error": "no log"}, _lengths(1), [],
                      scan_cast_viability([CLOCK]))
check("dead world banner present", "DEAD WORLD" in dead)
check("dead world names the clock", "World Clock" in dead)

live = render_report([], {"error": "no log"}, _lengths(2), [],
                      scan_cast_viability([CLOCK, KIRA]))
check("healthy world shows the viable line", "Cast viable" in live)
check("healthy world has no DEAD WORLD banner", "DEAD WORLD" not in live)

print("")
if FAILS:
    print(f"❌ {len(FAILS)} check(s) failed: {FAILS}")
    sys.exit(1)
print("✅ all checks passed")
