#!/usr/bin/env python3
"""extract_properties.py — Enumerate every property in the live world data.

Per Arcurus 2026-06-07 22:07 #openworld (todo 4775a0dc):
  "can you make a script that extract the current properties we have"

What it does:
  - GET /api/entities (auth via the openworld_auth=1 cookie)
  - For every key in `properties_int` and `properties_float` across all
    entities, gather: type, entity_count, sum, mean, min, max, distinct
    entity names (truncated to a few examples if many)
  - Print a Markdown table to stdout (sorted by entity_count desc, then
    property name asc)
  - Save a JSON dump to world_data/property_extract_<UTC-TS>.json
  - Optional: post the Markdown report to #openworld-log (channel
    1511696310984773633) with --post

Read-only / idempotent / safe to re-run at any time while the server is up.

Usage:
  python3 scripts/extract_properties.py             # print table, save JSON
  python3 scripts/extract_properties.py --post      # also post to Discord
  python3 scripts/extract_properties.py --json      # machine-readable stdout
  python3 scripts/extract_properties.py --api-url=URL
                                                     # custom server
  python3 scripts/extract_properties.py --no-save   # don't write the JSON file
  python3 scripts/extract_properties.py --diff world_data/property_extract_20260608T063154Z.json
                                                     # show added/removed/changed vs old extract
  python3 scripts/extract_properties.py --diff old.json --json   # machine-readable diff

Exit codes:
  0 = clean (report generated)
  1 = no entities returned (server may be down or auth missing)
  2 = runtime error (parse failure, write error, etc.)
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import pathlib
import statistics
import sys
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
DEFAULT_API_URL = "http://127.0.0.1:8081"
DEFAULT_OUTPUT_DIR = "world_data"
LOG_CHANNEL_ID = "1511696310984773633"  # #openworld-log
INT_FIELDS = ("properties_int",)
FLOAT_FIELDS = ("properties_float",)
OPENCLAW_CFG = pathlib.Path.home() / ".openclaw" / "openclaw.json"


def _discord_token() -> Optional[str]:
    """Best-effort: pull the Discord bot token out of openclaw.json."""
    if not OPENCLAW_CFG.exists():
        return None
    try:
        cfg = json.loads(OPENCLAW_CFG.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    for k_chain in (
        ("discord", "token"),
        ("channels", "discord", "token"),
        ("plugins", "discord", "token"),
    ):
        node = cfg
        ok = True
        for k in k_chain:
            if not isinstance(node, dict) or k not in node:
                ok = False
                break
            node = node[k]
        if ok and isinstance(node, str) and node:
            return node
    return None


def _discord_post(channel_id: str, text: str) -> Tuple[int, str]:
    token = _discord_token()
    if not token:
        return (0, "no discord token in openclaw.json")
    url = f"https://discord.com/api/v10/channels/{channel_id}/messages"
    body = json.dumps({"content": text[:1900]}).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "Authorization": f"Bot {token}",
            "Content-Type": "application/json",
            "User-Agent": "openclaw-worker (extract_properties, v1)",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return (resp.status, "ok")
    except urllib.error.HTTPError as e:
        return (e.code, f"http error: {e}")
    except urllib.error.URLError as e:
        return (0, f"url error: {e}")


# ---------------------------------------------------------------------------
# Core: fetch + aggregate
# ---------------------------------------------------------------------------
def fetch_entities(api_url: str) -> List[Dict[str, Any]]:
    """GET /api/entities; cookie-based auth (openworld_auth=1)."""
    url = f"{api_url.rstrip('/')}/api/entities"
    req = urllib.request.Request(
        url,
        headers={
            "Accept": "application/json",
            "Cookie": "openworld_auth=1",
        },
        method="GET",
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        payload = json.loads(resp.read().decode("utf-8"))
    # API returns {count, data, success} (or possibly a bare list, or {entities: [...]})
    if isinstance(payload, list):
        return payload
    if isinstance(payload, dict):
        for key in ("data", "entities"):
            if key in payload and isinstance(payload[key], list):
                return payload[key]
    return []


def aggregate_properties(entities: List[Dict[str, Any]]) -> Dict[str, Dict[str, Any]]:
    """Walk every entity and collect per-property stats."""
    stats: Dict[str, Dict[str, Any]] = {}
    for ent in entities:
        ent_id = ent.get("id", "?")
        ent_name = ent.get("name", "?")
        for field, ptype in (
            ("properties_int", "int"),
            ("properties_float", "float"),
        ):
            for prop, raw in (ent.get(field) or {}).items():
                try:
                    val = float(raw)
                except (TypeError, ValueError):
                    continue
                key = f"{prop} ({ptype})"
                bucket = stats.setdefault(
                    key,
                    {
                        "property": prop,
                        "type": ptype,
                        "values": [],
                        "entity_names": [],
                    },
                )
                bucket["values"].append(val)
                if ent_name not in bucket["entity_names"]:
                    bucket["entity_names"].append(ent_name)
    # Summarize
    out: Dict[str, Dict[str, Any]] = {}
    for key, b in stats.items():
        vs = b["values"]
        out[key] = {
            "property": b["property"],
            "type": b["type"],
            "entity_count": len(vs),
            "distinct_entity_count": len(b["entity_names"]),
            "sum": round(sum(vs), 4),
            "mean": round(statistics.fmean(vs), 4) if vs else 0.0,
            "min": min(vs) if vs else None,
            "max": max(vs) if vs else None,
            "example_entities": b["entity_names"][:3],
        }
    return out


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------
def render_table(stats: Dict[str, Dict[str, Any]], total_entities: int) -> str:
    rows = sorted(
        stats.values(),
        key=lambda r: (-r["entity_count"], r["property"], r["type"]),
    )
    lines = []
    lines.append(
        f"# Property extract — {len(rows)} unique properties across "
        f"{total_entities} entities ({_dt.datetime.now(_dt.timezone.utc).isoformat(timespec='seconds')})"
    )
    lines.append("")
    lines.append(
        "| property | type | entities | distinct | sum | mean | min | max |"
    )
    lines.append(
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"
    )
    for r in rows:
        mn = "—" if r["min"] is None else f"{r['min']:g}"
        mx = "—" if r["max"] is None else f"{r['max']:g}"
        lines.append(
            f"| `{r['property']}` | {r['type']} | {r['entity_count']} | "
            f"{r['distinct_entity_count']} | {r['sum']:g} | {r['mean']:g} | "
            f"{mn} | {mx} |"
        )
    return "\n".join(lines) + "\n"


def diff_stats(
    new: Dict[str, Dict[str, Any]],
    old: Dict[str, Dict[str, Any]],
) -> Dict[str, List[Any]]:
    """Compare two property extracts and return added/removed/changed lists.

    Output shape:
      {
        "added":    [{property, type, entity_count}, ...],
        "removed":  [{property, type, entity_count_was}, ...],
        "changed":  [{property, type, old_entity_count, new_entity_count, delta, old_sum, new_sum}, ...],
        "old_entities": <int>,  # entity count at the time of the older extract
        "new_entities": <int>,  # entity count now
      }
    Properties are matched by their full key (e.g. "power (int)") so a
    rename shows up as removed+added (we don't try to detect renames;
    that's a separate concern).
    """
    added: List[Dict[str, Any]] = []
    removed: List[Dict[str, Any]] = []
    changed: List[Dict[str, Any]] = []

    for key, v in new.items():
        if key not in old:
            added.append({
                "property": v["property"],
                "type": v["type"],
                "entity_count": v["entity_count"],
            })
        else:
            o = old[key]
            if (
                v["entity_count"] != o["entity_count"]
                or v["sum"] != o["sum"]
            ):
                changed.append({
                    "property": v["property"],
                    "type": v["type"],
                    "old_entity_count": o["entity_count"],
                    "new_entity_count": v["entity_count"],
                    "delta": v["entity_count"] - o["entity_count"],
                    "old_sum": o["sum"],
                    "new_sum": v["sum"],
                })

    for key, o in old.items():
        if key not in new:
            removed.append({
                "property": o["property"],
                "type": o["type"],
                "entity_count_was": o["entity_count"],
            })

    # Stable ordering
    added.sort(key=lambda r: (-r["entity_count"], r["property"], r["type"]))
    removed.sort(key=lambda r: (-r["entity_count_was"], r["property"], r["type"]))
    changed.sort(
        key=lambda r: (
            -abs(r["new_entity_count"] - r["old_entity_count"]),
            r["property"],
            r["type"],
        )
    )
    return {
        "added": added,
        "removed": removed,
        "changed": changed,
    }


def render_diff_table(
    diff: Dict[str, Any],
    old_entities: int,
    new_entities: int,
) -> str:
    lines: List[str] = []
    lines.append(
        f"# Property extract diff — {len(diff['added'])} added, "
        f"{len(diff['removed'])} removed, {len(diff['changed'])} changed "
        f"({_dt.datetime.now(_dt.timezone.utc).isoformat(timespec='seconds')})"
    )
    lines.append(
        f"_Entities: {old_entities} → {new_entities} "
        f"({'+' if new_entities - old_entities >= 0 else ''}"
        f"{new_entities - old_entities})_"
    )
    lines.append("")
    if diff["added"]:
        lines.append("## Added (new properties not in old extract)")
        for r in diff["added"]:
            lines.append(
                f"- `+` `{r['property']}` ({r['type']}) — {r['entity_count']} entities"
            )
        lines.append("")
    if diff["removed"]:
        lines.append("## Removed (properties no longer present)")
        for r in diff["removed"]:
            lines.append(
                f"- `-` `{r['property']}` ({r['type']}) — was {r['entity_count_was']} entities"
            )
        lines.append("")
    if diff["changed"]:
        lines.append("## Changed (count or sum drifted)")
        for r in diff["changed"]:
            delta = r["new_entity_count"] - r["old_entity_count"]
            sign = "+" if delta > 0 else ""
            lines.append(
                f"- `~` `{r['property']}` ({r['type']}) — entities "
                f"{r['old_entity_count']} → {r['new_entity_count']} "
                f"({sign}{delta}); sum {r['old_sum']} → {r['new_sum']}"
            )
        lines.append("")
    if not (diff["added"] or diff["removed"] or diff["changed"]):
        lines.append("_No changes._")
    return "\n".join(lines) + "\n"


def _load_extract(path: pathlib.Path) -> Dict[str, Dict[str, Any]]:
    """Read a previous extract JSON file. Accepts both the script's
    own output format (`{"properties": {...}}`) and a bare
    `{key: stats}` dict."""
    payload = json.loads(path.read_text())
    if isinstance(payload, dict) and "properties" in payload:
        return payload["properties"]
    if isinstance(payload, dict):
        # accept bare {key: stats} where stats has a "type" key
        if all(isinstance(v, dict) and "type" in v for v in payload.values()):
            return payload
    raise ValueError(
        f"{path}: not a recognised extract format "
        f"(expected dict with 'properties' key or a bare {key: stats} dict)"
    )


def render_post_body(stats: Dict[str, Dict[str, Any]], total_entities: int) -> str:
    """Discord-friendly body (≤ 1900 chars)."""
    rows = sorted(
        stats.values(),
        key=lambda r: (-r["entity_count"], r["property"], r["type"]),
    )
    head = (
        f"**Property extract** — {len(rows)} unique properties across "
        f"{total_entities} entities\n"
    )
    out = [head]
    for r in rows[:25]:
        ex = ", ".join(r["example_entities"][:2])
        mn_str = "—" if r["min"] is None else f"{r['min']:g}"
        mx_str = "—" if r["max"] is None else f"{r['max']:g}"
        out.append(
            f"• `{r['property']}` ({r['type']}) — {r['entity_count']} ents, "
            f"sum={r['sum']:g}, mean={r['mean']:g}, "
            f"min={mn_str}, max={mx_str} "
            f"(e.g. {ex})"
        )
    if len(rows) > 25:
        out.append(f"... and {len(rows) - 25} more (full table in JSON dump)")
    return "\n".join(out)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--api-url", default=DEFAULT_API_URL, help="open-world server base url")
    ap.add_argument("--json", action="store_true", help="emit machine-readable JSON to stdout")
    ap.add_argument("--no-save", action="store_true", help="don't write the JSON dump to disk")
    ap.add_argument("--output-dir", default=DEFAULT_OUTPUT_DIR, help="where to write the JSON dump")
    ap.add_argument("--post", action="store_true", help="post a summary to #openworld-log")
    ap.add_argument(
        "--diff",
        metavar="OLD_EXTRACT_JSON",
        help="compare the live extract with a previous JSON file and print a diff "
        "(additions / removals / count+sum changes). If the previous file is a "
        "property_extract_*.json from this script, the entity-count delta is also "
        "reported. --json is respected (machine-readable diff).",
    )
    args = ap.parse_args()

    try:
        entities = fetch_entities(args.api_url)
    except urllib.error.HTTPError as e:
        print(f"HTTP {e.code} fetching {args.api_url}/api/entities: {e}", file=sys.stderr)
        return 2
    except urllib.error.URLError as e:
        print(f"Could not reach {args.api_url}/api/entities: {e}", file=sys.stderr)
        return 1

    if not entities:
        print("No entities returned (server may be down, auth missing, or world empty).", file=sys.stderr)
        return 1

    stats = aggregate_properties(entities)

    # ---- --diff mode -----------------------------------------------------
    # When --diff is set we short-circuit the normal table/JSON output: we
    # still want the live `stats` (so we can --save it for the next diff
    # pass), but the stdout payload is the diff itself. The --json flag
    # controls the diff's output format.
    if args.diff:
        old_path = pathlib.Path(args.diff)
        try:
            old_stats = _load_extract(old_path)
        except (FileNotFoundError, ValueError) as e:
            print(f"Could not load previous extract {old_path}: {e}", file=sys.stderr)
            return 2
        old_entities_raw = None
        # The script's own dumps include `total_entities`; bare
        # {key: stats} dicts (from `--json` redirection) don't, so we
        # fall back to 0 in that case.
        try:
            old_payload = json.loads(old_path.read_text())
            if isinstance(old_payload, dict):
                old_entities_raw = old_payload.get("total_entities")
        except (json.JSONDecodeError, ValueError, OSError):
            old_entities_raw = None
        old_entities = int(old_entities_raw) if isinstance(old_entities_raw, (int, float)) else 0
        diff = diff_stats(stats, old_stats)
        if args.json:
            print(
                json.dumps(
                    {
                        "old_entities": old_entities,
                        "new_entities": len(entities),
                        "old_extract": str(old_path),
                        "diff": diff,
                    },
                    indent=2,
                )
            )
        else:
            print(render_diff_table(diff, old_entities, len(entities)))
        # We still write the live JSON dump below so the next run can diff
        # against *this* one — but we don't re-print the main table.
        skip_main_print = True
    else:
        skip_main_print = False

    if not skip_main_print:
        if args.json:
            print(json.dumps({"total_entities": len(entities), "properties": stats}, indent=2))
        else:
            print(render_table(stats, len(entities)))

    if not args.no_save:
        out_dir = pathlib.Path(args.output_dir)
        out_dir.mkdir(parents=True, exist_ok=True)
        ts = _dt.datetime.now(_dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = out_dir / f"property_extract_{ts}.json"
        payload = {
            "extracted_at_utc": _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds"),
            "api_url": args.api_url,
            "total_entities": len(entities),
            "property_count": len(stats),
            "properties": stats,
        }
        out_path.write_text(json.dumps(payload, indent=2))
        if not args.json:
            print(f"\nSaved JSON dump to {out_path}")

    if args.post:
        body = render_post_body(stats, len(entities))
        status, msg = _discord_post(LOG_CHANNEL_ID, body)
        print(f"Discord post: status={status} {msg}", file=sys.stderr)
        if status not in (200, 201, 204):
            return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
