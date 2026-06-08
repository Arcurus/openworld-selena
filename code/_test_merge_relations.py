#!/usr/bin/env python3
"""Smoke test for the offline-testable parts of merge_relations.py.

Mirrors the pattern of _test_regex_repair_bucket.py (smoke-test for
ow_sanity_check's bucket routing). Runs the planning logic against
synthesized entity data — no live server needed.

Verifies end-to-end that:
  1. split_relations handles the LLM's `→ X: text` convention,
     including empty narrative, missing colon (→ without text),
     and trailing whitespace.
  2. normalize_rel_name does case-insensitive, whitespace- and
     trailing-punctuation-insensitive comparison (so "Velora" /
     "velora." / "  Velora  " all collapse to one group).
  3. choose_winner correctly picks per strategy:
     - keep-first → lines[0]
     - keep-last → lines[-1]
     - longest → max by text length
     - combine → dedup identical texts, join with " | "
  4. find_duplicates groups by normalized name, preserves original
     canonical (first seen) in `lines[0]`, and only flags names
     that appear more than once.
  5. build_replacement_plan emits an old_line that matches the
     FULL "→ X: text" sequence the server expects, filters by
     --entity, and computes expected_new_summary_chars as a
     reasonable upper bound.
"""
import os
import sys
import json

sys.path.insert(0, '/home/openclaw/openclaw/workspace/open-world-selena/code')

from merge_relations import (
    split_relations,
    normalize_rel_name,
    choose_winner,
    find_duplicates,
    build_replacement_plan,
    STRATEGIES,
)


# ---------------------------------------------------------------------------
# 1) split_relations
# ---------------------------------------------------------------------------
narr, rels = split_relations("Hello world.\n→ Velora: First meeting at the shrine.\n→ Kira: Joined the company.")
assert narr == "Hello world.", f"narrative should be 'Hello world.', got {narr!r}"
assert len(rels) == 2, f"should be 2 relations, got {len(rels)}: {rels!r}"
assert rels[0] == ("Velora", "First meeting at the shrine."), f"first rel wrong: {rels[0]!r}"
assert rels[1] == ("Kira", "Joined the company."), f"second rel wrong: {rels[1]!r}"
print(f"OK: split_relations basic (narrative={narr!r}, {len(rels)} rels)")

# Empty / no relations
narr, rels = split_relations("")
assert narr == "" and rels == [], f"empty input should give empty result, got {narr!r}/{rels!r}"
print("OK: split_relations empty input")

# Relation without colon (malformed) falls into ('?', text)
narr, rels = split_relations("intro\n→ orphan text")
assert rels == [("?", "orphan text")], f"malformed should fall to ('?', text), got {rels!r}"
print("OK: split_relations malformed (no colon) → ('?', text)")

# Whitespace around → is collapsed (the regex strips it)
narr, rels = split_relations("intro  →   Velora :  spaced  ")
assert rels == [("Velora", "spaced")], f"whitespace should be collapsed, got {rels!r}"
print("OK: split_relations strips whitespace around → and :")


# ---------------------------------------------------------------------------
# 2) normalize_rel_name
# ---------------------------------------------------------------------------
# Match ow_sanity_check._normalize_rel_name: only trailing punct is stripped,
# not leading. (LLM names don't usually come quoted; if they ever do, the
# fix lives in both scripts at once.)
assert normalize_rel_name("Velora") == "velora"
assert normalize_rel_name("velora") == "velora"
assert normalize_rel_name("VELORA.") == "velora"
assert normalize_rel_name("  Velora the Undying  ") == "velora the undying"
assert normalize_rel_name("Velora,") == "velora"
assert normalize_rel_name("Velora?!") == "velora"
expected = chr(34) + "velora"  # leading quote preserved, trailing quote stripped
assert normalize_rel_name(chr(34) + "Velora" + chr(34)) == expected, (
    f"leading quote is NOT stripped (matches ow_sanity_check._normalize_rel_name); "
    f"expected={expected!r}"
)
print("OK: normalize_rel_name case+trailing-punct+ws insensitive (matches ow_sanity_check)")


# ---------------------------------------------------------------------------
# 3) choose_winner
# ---------------------------------------------------------------------------
lines = [("Velora", "short"), ("Velora", "this is a longer text"), ("Velora", "mid")]
assert choose_winner(lines, "keep-first") == ("Velora", "short")
print("OK: choose_winner keep-first → lines[0]")

assert choose_winner(lines, "keep-last") == ("Velora", "mid")
print("OK: choose_winner keep-last → lines[-1]")

assert choose_winner(lines, "longest") == ("Velora", "this is a longer text")
print("OK: choose_winner longest → max by text length")

# combine: dedup identical, join with ' | '
assert choose_winner([("X", "a"), ("X", "a"), ("X", "b")], "combine") == ("X", "a | b")
print("OK: choose_winner combine dedupes identical and joins with ' | '")

# combine: empty texts are skipped
assert choose_winner([("X", "a"), ("X", ""), ("X", "b")], "combine") == ("X", "a | b")
print("OK: choose_winner combine skips empty texts")

# combine: case-insensitive dedup
assert choose_winner([("X", "Hello"), ("X", "hello"), ("X", "world")], "combine") == ("X", "Hello | world")
print("OK: choose_winner combine is case-insensitive on text dedup")

# unknown strategy raises
try:
    choose_winner(lines, "bogus")
    raise AssertionError("expected ValueError for unknown strategy")
except ValueError as e:
    assert "bogus" in str(e)
    print(f"OK: choose_winner unknown strategy raises ValueError ({e})")

# All strategies are exposed
assert set(STRATEGIES) == {"keep-first", "keep-last", "longest", "combine"}, (
    f"STRATEGIES should be the 4 known strategies, got {STRATEGIES!r}"
)
print(f"OK: STRATEGIES == {sorted(STRATEGIES)}")


# ---------------------------------------------------------------------------
# 4) find_duplicates
# ---------------------------------------------------------------------------
# Entity with no dup → no finding
entities = [
    {"id": "e1", "name": "Clean", "history_summary": "intro → A: x → B: y"},
]
assert find_duplicates(entities) == [], "no-dup entity should produce no findings"
print("OK: find_duplicates no-dup entity → no findings")

# Entity with 1 dup → 1 finding, 1 duplicate group
entities = [
    {"id": "e1", "name": "Dup",
     "history_summary": "intro → Velora: a → Velora: b"},
]
finds = find_duplicates(entities)
assert len(finds) == 1, f"should be 1 finding, got {len(finds)}: {finds!r}"
assert finds[0]["entity_id"] == "e1"
assert len(finds[0]["duplicates"]) == 1
grp = finds[0]["duplicates"][0]
assert grp["canonical"] == "Velora", f"canonical should be first-seen 'Velora', got {grp['canonical']!r}"
assert len(grp["lines"]) == 2
print("OK: find_duplicates preserves first-seen name as canonical")

# Normalization groups case-insensitive duplicates
entities = [
    {"id": "e1", "name": "Norm",
     "history_summary": "intro → VELORA.: a → velora: b → Velora: c"},
]
finds = find_duplicates(entities)
assert len(finds) == 1 and len(finds[0]["duplicates"]) == 1, (
    f"case-insensitive names should collapse to 1 group, got {finds!r}"
)
grp = finds[0]["duplicates"][0]
assert grp["canonical"] == "VELORA.", f"canonical should be first-seen (with original casing), got {grp['canonical']!r}"
assert len(grp["lines"]) == 3
print("OK: find_duplicates case-insensitive name grouping (canonical preserves original casing)")

# Multiple dup groups in one entity
entities = [
    {"id": "e1", "name": "Multi",
     "history_summary": "intro → A: 1 → A: 2 → B: 3 → B: 4 → C: 5"},
]
finds = find_duplicates(entities)
assert len(finds) == 1
dup_keys = sorted([normalize_rel_name(d["canonical"]) for d in finds[0]["duplicates"]])
assert dup_keys == ["a", "b"], f"should have 2 dup groups (A and B), got {dup_keys!r}"
print("OK: find_duplicates multiple dup groups per entity")

# Empty summary is silently skipped
entities = [
    {"id": "e1", "name": "Empty", "history_summary": ""},
    {"id": "e2", "name": "None", "history_summary": None},
]
assert find_duplicates(entities) == []
print("OK: find_duplicates skips empty/None summary")


# ---------------------------------------------------------------------------
# 5) build_replacement_plan
# ---------------------------------------------------------------------------
entities = [
    {"id": "e1", "name": "Test", "history_summary": "intro → Velora: short → Velora: this is a longer text"},
]
finds = find_duplicates(entities)

# keep-last: new_line uses lines[-1] (most recent LLM write wins)
plan = build_replacement_plan(finds, "keep-last")
assert len(plan) == 1
p = plan[0]
assert p["entity_id"] == "e1"
assert len(p["merges"]) == 1
m = p["merges"][0]
assert m["old_line"] == "→ Velora: short", f"old_line should be the first-seen, got {m['old_line']!r}"
assert m["new_line"] == "→ Velora: this is a longer text", f"new_line should be last (keep-last), got {m['new_line']!r}"
print(f"OK: build_replacement_plan keep-last → old={m['old_line']!r}, new={m['new_line']!r}")

# longest
plan = build_replacement_plan(finds, "longest")
m = plan[0]["merges"][0]
assert m["new_line"] == "→ Velora: this is a longer text", f"longest should pick the longer text, got {m['new_line']!r}"
print("OK: build_replacement_plan longest picks the longer variant")

# combine
plan = build_replacement_plan(finds, "combine")
m = plan[0]["merges"][0]
assert m["new_line"] == "→ Velora: short | this is a longer text", f"combine should join with ' | ', got {m['new_line']!r}"
print(f"OK: build_replacement_plan combine → {m['new_line']!r}")

# only_entity_id filter
entities = [
    {"id": "e1", "name": "One", "history_summary": "→ A: x → A: y"},
    {"id": "e2", "name": "Two", "history_summary": "→ B: p → B: q"},
]
finds = find_duplicates(entities)
plan = build_replacement_plan(finds, "keep-last", only_entity_id="e1")
assert len(plan) == 1 and plan[0]["entity_id"] == "e1", (
    f"only_entity_id filter should restrict to e1, got {[p['entity_id'] for p in plan]!r}"
)
print("OK: build_replacement_plan only_entity_id filter restricts the plan")

# char delta math
plan = build_replacement_plan(finds[:1], "keep-last")  # just e1
# old_line "→ A: x" is 7 chars, new_line "→ A: y" is 7 chars → delta 0
assert plan[0]["expected_new_summary_chars"] == plan[0]["current_summary_chars"], (
    f"same-length old/new should give 0 delta, got {plan[0]['expected_new_summary_chars']} vs {plan[0]['current_summary_chars']}"
)
print("OK: build_replacement_plan char-delta math (same length → 0 delta)")

# Bigger delta: long new vs short old
entities = [
    {"id": "e1", "name": "Big",
     "history_summary": "intro → A: short → A: a much longer text variant"},
]
finds = find_duplicates(entities)
plan = build_replacement_plan(finds, "longest")
cur_len = plan[0]["current_summary_chars"]
new_len = plan[0]["expected_new_summary_chars"]
assert new_len > cur_len, f"longest should grow the summary, got cur={cur_len} new={new_len}"
print(f"OK: build_replacement_plan char-delta (longer variant grows summary: {cur_len} → {new_len})")


# ---------------------------------------------------------------------------
# 6) dry-run safety: render_plan_report is a pure formatter
# ---------------------------------------------------------------------------
from merge_relations import render_plan_report
text = render_plan_report(plan, "longest")
assert "merge-relations plan" in text
assert "Big" in text
assert "→ A:" in text
print(f"OK: render_plan_report emits a {len(text)}-char plan report (safe to print)")


# ---------------------------------------------------------------------------
# 7) integration: dry-run against the LIVE server (if reachable)
#     This is best-effort — if the server isn't running we skip, not fail.
# ---------------------------------------------------------------------------
import urllib.request
import urllib.error
try:
    req = urllib.request.Request(
        "http://127.0.0.1:8081/api/entities",
        headers={"Cookie": "openworld_auth=1"},
    )
    with urllib.request.urlopen(req, timeout=3) as r:
        live_raw = json.loads(r.read())
    # Unwrap the API's {count, data} envelope to match fetch_entities()
    if isinstance(live_raw, dict) and "data" in live_raw:
        live = live_raw["data"]
    elif isinstance(live_raw, list):
        live = live_raw
    else:
        live = []
    if live:
        live_finds = find_duplicates(live)
        live_plan = build_replacement_plan(live_finds, "keep-last")
        total = sum(len(p['merges']) for p in live_plan)
        print(f"OK: live-server dry-run — scanned {len(live)} entities, {len(live_finds)} with dups, {total} planned merges")
    else:
        print("SKIP: live-server dry-run (empty /api/entities response)")
except (urllib.error.URLError, ConnectionError, OSError) as e:
    print(f"SKIP: live-server dry-run (server not reachable: {type(e).__name__})")

print("ALL OK")
