#!/usr/bin/env python3
"""
Backfill world_data/action_history.jsonl from logs/llm-log-*.log.

Per Arcurus 2026-06-03 (#openworld): "is the history of the world
actions saved yet?" — we now write to action_history.jsonl on every
process_action, but the 8+ actions that happened BEFORE this fix are
only in logs/llm-log-*.log.  This script recovers them so the new
history UI can show the full record.

Schema recovered from the logs:
  * timestamp  -- from the log filename prefix (Unix epoch) or the
                  bracketed datetime at the top of the entry
  * entity_id  -- from the "LLM call for entity <uuid>" header
  * entity_name -- via the OW API lookup (entity by id)
  * action / outcome / details / effects -- from the JSON response

Output: appends to world_data/action_history.jsonl (idempotent if
the script is re-run, because the timestamp+entity_id+action
triple is unique per process_action call).

Usage:
  python3 scripts/backfill_history.py            # dry-run, just report
  python3 scripts/backfill_history.py --apply    # actually append
"""
import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

LOGS_DIR = "logs"
HISTORY_PATH = "world_data/action_history.jsonl"
OW_BASE = "http://localhost:8081"
COOKIE = "Cookie: openworld_auth=1"

# Each llm-log-<bogus_year>-<MM-DD>.log has multiple === LLM Call ===
# blocks.  Note: the "<bogus_year>" in the filename is broken (see below);
# we use the file mtime + the time-of-day from the header for the real
# timestamp.
#
# Schema in the log:
#   [BOGUS_YEAR-MM-DD HH:MM:SS] === LLM Call ===
#   Success: SUCCESS
#   Time: 12345 ms
#   --- Context ---
#   LLM call for entity <UUID> (EntityName)
#   --- Response ---
#   {"action":..., "outcome":..., "effects":..., "narrative":...}
#   --- Parsing ---
#   ...
#   --- Extra ---
#   Effects: {...}
#   ====================
#
# Note: chrono_now_date() in main.rs has a bug — it treats seconds-since-
# epoch as days-since-epoch, so the "year" comes out as ~4.8 million.
# The HH:MM:SS portion of the header IS real, though, and the file mtime
# is real, so we combine: file mtime's date + header's HH:MM:SS.
LLM_CALL_HEADER = re.compile(r"^\[(\d+)-(\d+)-(\d+) (\d+):(\d+):(\d+)\] === LLM Call ===$")
SEP = "===================="
ENTITY_RE = re.compile(r"LLM call for entity ([0-9a-f-]{36})\s*(?:\(([^)]+)\))?")
RESPONSE_RE = re.compile(r"--- Response ---\s*(\{[\s\S]+?\})\s*---")


def parse_log_file(path: str) -> list:
    """Extract (timestamp, entity_id, entity_name, action, outcome,
    details, effects) tuples from one LLM log file."""
    out = []
    try:
        file_mtime_epoch = os.path.getmtime(path)
    except OSError:
        return out
    file_mtime_dt = datetime.fromtimestamp(file_mtime_epoch, tz=timezone.utc)
    with open(path) as f:
        content = f.read()
    # Each LLM call is one chunk separated by =======...
    for chunk in content.split(SEP):
        chunk = chunk.strip()
        if not chunk or "=== LLM Call ===" not in chunk:
            continue
        lines = chunk.splitlines()
        # Find header line + extract HH:MM:SS
        hh = mm = ss = None
        for line in lines:
            m = LLM_CALL_HEADER.match(line)
            if m:
                hh, mm, ss = int(m.group(4)), int(m.group(5)), int(m.group(6))
                break
        if hh is None:
            continue
        # Build a real timestamp: file mtime's date + header's time-of-day.
        # Falls back to file mtime if header time is missing.
        try:
            ts_dt = file_mtime_dt.replace(hour=hh, minute=mm, second=ss, microsecond=0)
        except (ValueError, OverflowError):
            ts_dt = file_mtime_dt
        # Find entity id
        entity_id = None
        entity_name = None
        for line in lines:
            m = ENTITY_RE.search(line)
            if m:
                entity_id = m.group(1)
                entity_name = (m.group(2) or "").strip() or None
                break
        if entity_id is None:
            continue
        # Find response JSON
        m = RESPONSE_RE.search(chunk)
        if not m:
            continue
        try:
            resp = json.loads(m.group(1))
        except json.JSONDecodeError:
            continue
        action = resp.get("action", "")
        outcome = resp.get("outcome", "")
        narrative = resp.get("narrative", "")
        effects = resp.get("effects", {})
        if not action:
            continue
        out.append({
            "epoch": ts_dt.timestamp(),
            "entity_id": entity_id,
            "entity_name_hint": entity_name,
            "action": action,
            "outcome": outcome,
            "details": narrative,
            "effects": effects,
        })
    return out


def lookup_entity_name(entity_id: str) -> str:
    """Resolve a UUID to its current name via the OW API."""
    try:
        req = urllib.request.Request(
            f"{OW_BASE}/api/entities/{entity_id}",
            headers={"Cookie": "***"})
        with urllib.request.urlopen(req, timeout=5) as r:
            d = json.loads(r.read().decode())
            if d.get("success") and d.get("data"):
                return d["data"].get("name", entity_id)
    except Exception:
        pass
    return entity_id


def load_existing_keys() -> set:
    """Read the JSONL to dedupe (timestamp, entity_id, action) tuples."""
    keys = set()
    if not os.path.exists(HISTORY_PATH):
        return keys
    with open(HISTORY_PATH) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
                keys.add((d.get("timestamp", ""), d.get("entity_id", ""), d.get("action", "")))
            except json.JSONDecodeError:
                pass
    return keys


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true",
                    help="Actually append to the JSONL (default: dry-run)")
    ap.add_argument("--max", type=int, default=2000,
                    help="Maximum number of entries to backfill "
                         "(most recent first; default 2000).  The LLM "
                         "logs from the first 30+ days of OW running "
                         "contain 50k+ entries; this cap keeps the JSONL "
                         "manageable. Older entries stay in the logs.")
    args = ap.parse_args()

    # Collect parsed entries from all llm-log files
    parsed = []
    for name in sorted(os.listdir(LOGS_DIR)):
        if not name.startswith("llm-log-"):
            continue
        parsed.extend(parse_log_file(os.path.join(LOGS_DIR, name)))

    print(f"Found {len(parsed)} LLM-call entries across all logs")

    # Dedupe: keep first occurrence per (entity_id, action, epoch)
    seen = set()
    unique = []
    for p in parsed:
        key = (p["entity_id"], p["action"], p["epoch"])
        if key in seen:
            continue
        seen.add(key)
        unique.append(p)
    print(f"Unique entries: {len(unique)}")

    # Compare with current JSONL
    existing_keys = load_existing_keys()
    print(f"Existing JSONL entries: {len(existing_keys)}")

    # Sort unique by epoch DESC, then cap.
    unique.sort(key=lambda x: -x["epoch"])
    if len(unique) > args.max:
        print(f"Capping at the {args.max} most recent "
              f"({len(unique) - args.max} oldest will be skipped)")
        unique = unique[:args.max]

    # Build the new entries (with entity_name resolved)
    to_add = []
    for p in unique:
        # Build a stable timestamp
        ts_iso = datetime.fromtimestamp(p["epoch"], tz=timezone.utc).isoformat().replace("+00:00", "Z")
        # Skip if already in JSONL
        if (ts_iso, p["entity_id"], p["action"]) in existing_keys:
            continue
        name = p.get("entity_name_hint") or lookup_entity_name(p["entity_id"])
        to_add.append({
            "entity_id": p["entity_id"],
            "entity_name": name,
            "timestamp": ts_iso,
            "action": p["action"],
            "outcome": p["outcome"],
            "details": p["details"],
            "effects": p["effects"],
            "warnings": [],
        })

    print(f"New entries to append: {len(to_add)}")
    for e in to_add[:5]:
        print(f"  {e['timestamp']} | {e['entity_name']:25s} | {e['action']}")
    if len(to_add) > 5:
        print(f"  ... and {len(to_add) - 5} more")

    if not args.apply:
        print()
        print("DRY-RUN: pass --apply to actually write to the JSONL")
        return

    # Append atomically (write to .tmp, then rename)
    if not to_add:
        print("Nothing to do")
        return
    tmp = HISTORY_PATH + ".tmp"
    with open(tmp, "a") as f:
        for e in to_add:
            f.write(json.dumps(e, ensure_ascii=False) + "\n")
    os.replace(tmp, HISTORY_PATH)
    print(f"✓ Appended {len(to_add)} entries to {HISTORY_PATH}")


if __name__ == "__main__":
    main()
