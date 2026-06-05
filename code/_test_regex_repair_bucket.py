#!/usr/bin/env python3
"""Smoke test for the new `regex_repair` warning bucket in ow_sanity_check.

Verifies end-to-end that:
  1. The "regex_repair" bucket is registered in the WARN_BUCKETS list
     (so it shows up in reports even if a future reorder changes the
     iteration order).
  2. The exact warning string that commit 78ea1ac's parse_llm_action_response
     emits ("parse_llm_action_response: LLM response matched a known...")
     routes to the "regex_repair" bucket when fed through scan_llm_log.
  3. Other buckets (e.g. "system_entity_targeted", "old_part_not_found")
     still route correctly (regression guard for the bucket-list ordering).
"""
import os
import sys
import tempfile

sys.path.insert(0, '/home/openclaw/openclaw/workspace/open-world-selena/code')

from ow_sanity_check import scan_llm_log

# Build a minimal LLM log block whose --- Parsing --- line carries the
#    real warning string produced by the live server (commit 78ea1ac). One
#    block is enough — scan_llm_log splits on '=== LLM Call ==='.
def _block(parsing_warnings: str) -> str:
    body = (
        '{"action":"test_smoke","outcome":"smoke","effects":{"reputation":1},'
        '"narrative":"x","history_summary":"x"}'
    )
    parsing = f"--- Parsing ---\nApplied 1 effects. Warnings: {parsing_warnings}\n"
    return (
        "[2026-06-05 12:00:00] === LLM Call ===\n"
        "--- Response ---\n"
        f"{body}\n"
        f"{parsing}"
    )


W_REPAIR = (
    '"parse_llm_action_response: LLM response matched a known '
    'malformed pattern and was repaired (regex fixup of the '
    '\\"\\":\\"old_part\\"|\\"new_part\\" empty-key bug seen in '
    'history_summary_replace)."'
)
W_OTHER = '"history_summary_replace[0]: old_part not found in current summary; skipped"'
W_SYS = '"Entity is a system entity (type=world_clock, tags=[\\"meta\\"]); LLM effect writes blocked."'

with tempfile.TemporaryDirectory() as td:
    log_path = os.path.join(td, "synth.log")
    with open(log_path, "w") as f:
        f.write(_block(f"[{W_REPAIR}, {W_OTHER}, {W_SYS}]"))
    result = scan_llm_log(log_path)

wa = result.get("warning_counts_all", {})
ws = result.get("warning_samples", {})

# 1) The "regex_repair" bucket fires for the live warning string.
#    If the bucket were missing, the warning would fall through to
#    "other" — so this assertion also covers bucket-list registration.
assert wa.get("regex_repair", 0) == 1, (
    f"regex_repair bucket should fire once, got warning_counts_all={wa!r}"
)
print(f"OK: warning_counts_all['regex_repair'] == 1 (full result: {wa})")

# 2) Older buckets still route correctly (regression guard against
#    accidental bucket-list reorder breaking substring matching).
assert wa.get("old_part_not_found", 0) == 1, (
    f"old_part_not_found bucket should still fire (regression), got {wa!r}"
)
print(f"OK: warning_counts_all['old_part_not_found'] == 1 (regression guard)")

assert wa.get("system_entity_targeted", 0) == 1, (
    f"system_entity_targeted bucket should still fire (regression), got {wa!r}"
)
print(f"OK: warning_counts_all['system_entity_targeted'] == 1 (regression guard)")

# 3) The sample string is the actual live warning (not a stub), so we
#    know the bucket is keyed to the real text, not a hand-typed copy.
assert "parse_llm_action_response" in (ws.get("regex_repair") or [""])[0], (
    f"regex_repair sample should contain the actual warning string, got {ws.get('regex_repair')!r}"
)
print(f"OK: regex_repair sample starts with the live warning string")

print("ALL OK")
