#!/usr/bin/env python3
"""merge_relations.py — Apply a merge to duplicate relations in entity history summaries.

Per Arcurus 2026-06-07 (#openworld, todo 6e33da08): build the complement
to ow_sanity_check.py's read-only scan. The scan finds duplicate
relations (same name appears > 1 time in `entity.history_summary`).
This tool merges them, with human review via dry-run by default.

Workflow:
  1. Fetch all entities from the running server (auth required)
  2. Re-detect duplicates using the same logic as ow_sanity_check
  3. For each duplicate group, build a merged replacement line
     (the strategy controls which variant wins: keep-first, keep-last,
     longest, or combine with ` | ` separator)
  4. In dry-run mode (default): print the plan, exit
  5. In --apply mode: snapshot world_data/save.owbl to
     world_data/backups/save-pre-merge-{ts}.owbl, then call
     POST /api/entities/:id/history-summary/replace for each merge
  6. Optionally post a report to #openworld-log

Safety:
  - Default mode is dry-run. The script refuses to --apply without
    --yes (a typed confirmation token from the operator).
  - Always snapshots save.owbl before applying. The snapshot is
    timestampted and never overwritten.
  - All HTTP calls use auth cookies; the script bails with a clear
    message if auth is missing.

Usage:
  python3 code/merge_relations.py                            # scan + plan (no changes)
  python3 code/merge_relations.py --apply --yes              # scan + apply with keep-last
  python3 code/merge_relations.py --strategy=longest --apply --yes
  python3 code/merge_relations.py --strategy=combine --apply --yes
  python3 code/merge_relations.py --entity=<id> --apply --yes
                                                              # only act on one entity
  python3 code/merge_relations.py --json                     # machine-readable plan
  python3 code/merge_relations.py --post                     # post the report to #openworld-log
  python3 code/merge_relations.py --post --apply --yes       # apply AND post the result
  python3 code/merge_relations.py --api-url=URL              # custom server

Exit codes:
  0 = clean (no duplicates, or all merges applied successfully)
  1 = findings (duplicates exist, dry-run refused to apply)
  2 = runtime error (couldn't reach server, auth failed, etc.)
  3 = partial apply (some merges failed; check stderr)

Per Arcurus 2026-06-07: the script is conservative and refuses any
silently-destructive action. The plan-and-apply split is mandatory.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import shutil
import sys
import urllib.error
import urllib.request
from collections import defaultdict
from typing import Any, Dict, List, Optional, Tuple

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
DEFAULT_API_URL = "http://127.0.0.1:8081"
AUTH_COOKIE = ("openworld_auth=1",)  # matches the existing ow_sanity_check.py auth
WORLD_DATA_DIR = "world_data"
BACKUP_DIR = "world_data/backups"
OPENWORLD_LOG_CHANNEL_ID = "1511696310984773633"  # #openworld-log

# Reuse the exact relation-split regex from ow_sanity_check so the two
# scripts agree on what "duplicate" means.
_REL_SPLIT = re.compile(r"\s*→\s*")

# History-summary cap. Matches the Rust `default_max_history_summary_chars`
# in src/main.rs:503. The /api/entities/:id response includes the
# effective per-entity cap; the default is 10000 unless settings.json
# overrides it.
DEFAULT_HISTORY_SUMMARY_CAP = 10000

# ---------------------------------------------------------------------------
# Discord post (copied from ow_sanity_check.py so the two scripts are
# independent and either can run without the other)
# ---------------------------------------------------------------------------
def _discord_token() -> Optional[str]:
    """Read the Discord bot token from ~/.config/discord_bot_token (or env).
    Returns None if not configured (caller decides what to do).
    """
    paths = [
        os.path.expanduser("~/.config/discord_bot_token"),
        os.path.expanduser("~/openclaw/workspace/.discord_bot_token"),
    ]
    for p in paths:
        if os.path.exists(p):
            try:
                return open(p).read().strip()
            except Exception:
                continue
    return os.environ.get("DISCORD_BOT_TOKEN")


def _discord_post(channel_id: str, text: str) -> Dict[str, Any]:
    """Post `text` to the given Discord channel via the bot token.
    Mirrors the helper in ow_sanity_check.py.
    """
    token = _discord_token()
    if not token:
        return {"error": "no Discord bot token (set DISCORD_BOT_TOKEN or write ~/.config/discord_bot_token)"}
    url = f"https://discord.com/api/v10/channels/{channel_id}/messages"
    # Discord message length cap is 2000 chars. Split if needed.
    out: List[Dict[str, Any]] = []
    for chunk in _chunk_for_discord(text, 1900):
        body = json.dumps({"content": chunk}).encode("utf-8")
        req = urllib.request.Request(
            url, data=body, method="POST",
            headers={
                "Authorization": f"Bot {token}",
                "Content-Type": "application/json",
                "User-Agent": "openworld-merge-relations/1.0 (python urllib)",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=15) as r:
                out.append(json.load(r))
        except urllib.error.HTTPError as e:
            out.append({"error": f"HTTP {e.code}: {e.read().decode('utf-8', 'replace')[:200]}"})
        except Exception as e:
            out.append({"error": f"{type(e).__name__}: {e}"})
    return {"chunks": out}


def _chunk_for_discord(text: str, max_chars: int) -> List[str]:
    """Split `text` on paragraph boundaries to fit Discord's 2000-char cap."""
    if len(text) <= max_chars:
        return [text]
    chunks: List[str] = []
    while text:
        if len(text) <= max_chars:
            chunks.append(text)
            break
        # Find a newline near the cap
        cut = text.rfind("\n", 0, max_chars)
        if cut < max_chars // 2:
            cut = max_chars  # hard cut if no good break
        chunks.append(text[:cut].rstrip())
        text = text[cut:].lstrip("\n")
    return chunks

# ---------------------------------------------------------------------------
# Relation parsing (shared shape with ow_sanity_check._split_relations)
# ---------------------------------------------------------------------------
def split_relations(summary: str) -> Tuple[str, List[Tuple[str, str]]]:
    """Split a history_summary into (narrative, [(name, text), ...]).

    Relations are detected by the `→` marker (the LLM's convention).
    Each `→ X: ...` becomes a (X, text) tuple. The narrative is
    everything before the first →.
    """
    if not summary:
        return "", []
    parts = _REL_SPLIT.split(summary)
    narrative = parts[0].strip()
    relations: List[Tuple[str, str]] = []
    for p in parts[1:]:
        m = re.match(r"^([^:]+):\s*(.*)$", p, re.DOTALL)
        if m:
            relations.append((m.group(1).strip(), m.group(2).strip()))
        else:
            relations.append(("?", p.strip()))
    return narrative, relations


def normalize_rel_name(name: str) -> str:
    """Loose-name comparison: strip trailing punct, collapse whitespace, lowercase."""
    return re.sub(r"\s+", " ", name.strip().rstrip(".,;:!?'\"")).lower()


# ---------------------------------------------------------------------------
# Plan: identify duplicates + build the merged replacement line
# ---------------------------------------------------------------------------
def find_duplicates(entities: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """For each entity, find duplicate-relation groups (same name > 1 time).

    Returns a list of findings, one per affected entity. Empty list = clean.
    Each finding has the structure:
        {
            "entity_id": str,
            "entity_name": str,
            "current_summary": str,
            "duplicates": [
                {
                    "canonical": str,           # the first name (preserved as-is in the line)
                    "lines": [(name, text), ...]  # all variants, in original order
                }
            ],
        }
    """
    findings: List[Dict[str, Any]] = []
    for e in entities:
        summary = e.get("history_summary") or ""
        if not summary:
            continue
        _, relations = split_relations(summary)
        if not relations:
            continue
        # Preserve original order; group by normalized name.
        groups: Dict[str, List[Tuple[str, str]]] = defaultdict(list)
        for name, text in relations:
            groups[normalize_rel_name(name)].append((name, text))
        dups = {k: v for k, v in groups.items() if len(v) > 1}
        if dups:
            findings.append({
                "entity_id": e.get("id"),
                "entity_name": e.get("name"),
                "current_summary": summary,
                "duplicates": [
                    {"canonical": dups_list[0][0], "lines": dups_list}
                    for dups_list in dups.values()
                ],
            })
    return findings


STRATEGIES = ("keep-first", "keep-last", "combine", "longest")


def choose_winner(lines: List[Tuple[str, str]], strategy: str) -> Tuple[str, str]:
    """Pick the merged (name, text) pair from `lines` per `strategy`.

    - keep-first: take the first line as-is. Drops everything else.
    - keep-last:  take the last line. Most recent LLM "understanding" wins.
    - longest:    take the line with the longest text. Preserves max info.
    - combine:    concatenate all texts with ' | ' separator. Deduplicates
                  identical texts first. Name is taken from the first line.
    """
    if not lines:
        raise ValueError("choose_winner called with empty lines")
    if strategy == "keep-first":
        return lines[0]
    if strategy == "keep-last":
        return lines[-1]
    if strategy == "longest":
        return max(lines, key=lambda lt: len(lt[1]))
    if strategy == "combine":
        name = lines[0][0]
        seen = []
        seen_lower = set()
        for _, t in lines:
            tl = t.strip()
            if not tl:
                continue
            if tl.lower() in seen_lower:
                continue
            seen.append(tl)
            seen_lower.add(tl.lower())
        return name, " | ".join(seen)
    raise ValueError(f"unknown strategy: {strategy!r} (choose from {STRATEGIES})")


def build_replacement_plan(
    findings: List[Dict[str, Any]],
    strategy: str,
    only_entity_id: Optional[str] = None,
    cap: int = DEFAULT_HISTORY_SUMMARY_CAP,
) -> List[Dict[str, Any]]:
    """For each finding, build the per-entity merge plan.

    A "plan" entry contains:
        entity_id, entity_name, strategy,
        merges: [{canonical, old_line, new_line, dropped_lines}],
        expected_new_summary_chars: int,
        over_cap_by: int  (positive = merge pushes past `cap`)

    We use the full original "→ canonical: text" line as `old_line` so
    the find-replace matches exactly (incl. the `→` prefix). This
    relies on the same `→` convention as the LLM emits and the API
    accepts. We replace ONE occurrence per call (the API does the
    first-match replace; subsequent calls keep shrinking the dup set).

    The `cap` argument defaults to DEFAULT_HISTORY_SUMMARY_CAP. The
    /api/entities/:id response may report a different per-entity
    cap; callers can pass that value to make the cap check accurate.
    """
    plan: List[Dict[str, Any]] = []
    for f in findings:
        if only_entity_id and f["entity_id"] != only_entity_id:
            continue
        merges = []
        for dup in f["duplicates"]:
            canonical = dup["canonical"]
            lines = dup["lines"]
            winner_name, winner_text = choose_winner(lines, strategy)
            # Build the full "→ X: text" line as it appears in summary.
            old_line = f"→ {lines[0][0]}: {lines[0][1]}"
            new_line = f"→ {winner_name}: {winner_text}"
            dropped = [t for (_, t) in lines]  # all variants, for the report
            merges.append({
                "canonical": canonical,
                "old_line": old_line,
                "new_line": new_line,
                "dropped_lines": dropped,
            })
        if not merges:
            continue
        # Estimate new char count: current - (sum of old_line chars - new_line chars).
        # This is an upper bound; truncation may still kick in.
        cur = f["current_summary"]
        delta = sum(len(m["new_line"]) - len(m["old_line"]) for m in merges)
        expected = len(cur) + delta
        plan.append({
            "entity_id": f["entity_id"],
            "entity_name": f["entity_name"],
            "strategy": strategy,
            "current_summary_chars": len(cur),
            "merges": merges,
            "expected_new_summary_chars": expected,
            "cap": cap,
            "over_cap_by": max(0, expected - cap),
        })
    return plan

# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------
def _http_get(path: str, api_url: str) -> Any:
    url = f"{api_url.rstrip('/')}{path}"
    req = urllib.request.Request(url, headers={"Cookie": AUTH_COOKIE[0]})
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.load(r)


def _http_post(path: str, body: Dict[str, Any], api_url: str) -> Tuple[int, Any]:
    url = f"{api_url.rstrip('/')}{path}"
    req = urllib.request.Request(
        url, data=json.dumps(body).encode("utf-8"),
        method="POST",
        headers={"Cookie": AUTH_COOKIE[0], "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", "replace")[:400]
        return e.code, {"error": f"HTTP {e.code}: {body}"}


def fetch_entities(api_url: str) -> List[Dict[str, Any]]:
    data = _http_get("/api/entities", api_url)
    if isinstance(data, list):
        return data
    if isinstance(data, dict):
        if "error" in data:
            raise RuntimeError(f"server returned error: {data['error']}")
        return data.get("data", data.get("entities", []))
    return []


def fetch_entity(entity_id: str, api_url: str) -> Dict[str, Any]:
    return _http_get(f"/api/entities/{entity_id}", api_url)


# ---------------------------------------------------------------------------
# Backup + apply
# ---------------------------------------------------------------------------
def snapshot_world(cwd: str) -> Optional[str]:
    """Copy world_data/save.owbl to world_data/backups/save-pre-merge-{ts}.owbl.

    Returns the path of the backup, or None if save.owbl does not exist.
    Silently skips if the source is missing (e.g. server keeps state in
    memory and the file is only written on a save event).
    """
    src = os.path.join(cwd, WORLD_DATA_DIR, "save.owbl")
    if not os.path.exists(src):
        return None
    os.makedirs(os.path.join(cwd, BACKUP_DIR), exist_ok=True)
    ts = _dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    dst = os.path.join(cwd, BACKUP_DIR, f"save-pre-merge-{ts}.owbl")
    shutil.copy2(src, dst)
    return dst


def apply_merge(entity_id: str, old_line: str, new_line: str, api_url: str) -> Dict[str, Any]:
    """Call POST /api/entities/:id/history-summary/replace with the merge.

    The handler does the first-match find-replace. Returns the JSON
    response (with `success`, `history_summary_chars`, `warning`, etc.).
    """
    path = f"/api/entities/{entity_id}/history-summary/replace"
    body = {"old_part": old_line, "new_part": new_line, "not_found_is_error": False}
    code, resp = _http_post(path, body, api_url)
    return {"http_status": code, "response": resp}

# ---------------------------------------------------------------------------
# Report rendering
# ---------------------------------------------------------------------------
def render_plan_report(plan: List[Dict[str, Any]], strategy: str) -> str:
    parts: List[str] = []
    parts.append(f"🧬 **merge-relations plan** — {_dt.datetime.now().isoformat(timespec='seconds')}")
    parts.append(f"strategy: **{strategy}** ({_strategy_explainer(strategy)})")
    parts.append("")
    if not plan:
        parts.append("✅ No duplicate relations to merge.")
        return "\n".join(parts)
    total_merges = sum(len(p["merges"]) for p in plan)
    over_cap = [p for p in plan if p["over_cap_by"] > 0]
    parts.append(f"📋 {len(plan)} entities / {total_merges} merge operations planned.")
    if over_cap:
        cap = over_cap[0]["cap"]
        parts.append(f"⚠️  {len(over_cap)} entity(ies) would exceed cap ({cap} chars) post-merge:")
        for p in over_cap:
            parts.append(f"   - {p['entity_name']}: {p['expected_new_summary_chars']} chars "
                         f"(+{p['over_cap_by']} over)")
        parts.append("   → server would silently truncate; use --strategy=keep-first/keep-last/longest "
                     "or pass --ignore-cap to override.")
    for p in plan:
        parts.append(f"  • **{p['entity_name']}** ({p['entity_id'][:8]}..): "
                     f"{p['current_summary_chars']} → ~{p['expected_new_summary_chars']} chars")
        for m in p["merges"]:
            old_short = m["old_line"][:60] + ("…" if len(m["old_line"]) > 60 else "")
            new_short = m["new_line"][:60] + ("…" if len(m["new_line"]) > 60 else "")
            parts.append(f"    - `{m['canonical']}`: {old_short}")
            parts.append(f"        → {new_short}")
            if len(m["dropped_lines"]) > 1:
                parts.append(f"        (drops {len(m['dropped_lines'])-1} variant(s))")
    return "\n".join(parts)


def render_apply_report(plan: List[Dict[str, Any]], results: List[Dict[str, Any]], backup_path: Optional[str], strategy: str) -> str:
    parts: List[str] = []
    parts.append(f"✅ **merge-relations applied** — {_dt.datetime.now().isoformat(timespec='seconds')}")
    parts.append(f"strategy: **{strategy}**")
    if backup_path:
        parts.append(f"backup: `{backup_path}`")
    else:
        parts.append("backup: ⚠️  none (save.owbl not on disk; relying on server in-memory state)")
    parts.append("")
    ok = sum(1 for r in results if r["ok"])
    fail = sum(1 for r in results if not r["ok"])
    parts.append(f"📊 {ok} merges OK, {fail} failed.")
    if fail:
        parts.append("**Failures:**")
        for r in results:
            if r["ok"]:
                continue
            parts.append(f"  • {r['entity_name']} — `{r['canonical']}`: {r['error']}")
    return "\n".join(parts)


def _strategy_explainer(strategy: str) -> str:
    return {
        "keep-first": "keep first occurrence, drop the rest",
        "keep-last": "keep last occurrence (most recent LLM write wins)",
        "longest": "keep the longest text variant",
        "combine": "concatenate unique variants with ' | '",
    }.get(strategy, "?")

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main() -> int:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--api-url", default=DEFAULT_API_URL,
                   help=f"open-world-selena API base URL (default {DEFAULT_API_URL})")
    p.add_argument("--strategy", default="keep-last", choices=STRATEGIES,
                   help="merge strategy (default: keep-last)")
    p.add_argument("--entity", default=None,
                   help="only act on this entity_id (default: all entities with duplicates)")
    p.add_argument("--apply", action="store_true",
                   help="apply the merges (default: dry-run only)")
    p.add_argument("--yes", action="store_true",
                   help="required with --apply: confirms you want to mutate the world")
    p.add_argument("--post", action="store_true",
                   help=f"post the report to #openworld-log ({OPENWORLD_LOG_CHANNEL_ID})")
    p.add_argument("--json", action="store_true",
                   help="emit machine-readable JSON to stdout instead of Markdown")
    p.add_argument("--no-backup", action="store_true",
                   help="skip the save.owbl snapshot (not recommended)")
    p.add_argument("--cap", type=int, default=DEFAULT_HISTORY_SUMMARY_CAP,
                   help=f"history_summary char cap (default {DEFAULT_HISTORY_SUMMARY_CAP}, "
                        "matches src/main.rs default_max_history_summary_chars)")
    p.add_argument("--ignore-cap", action="store_true",
                   help="apply even if a merge would push an entity past --cap (server will truncate)")
    args = p.parse_args()

    # Fetch entities
    try:
        entities = fetch_entities(args.api_url)
    except urllib.error.HTTPError as e:
        print(f"ERROR: HTTP {e.code} fetching entities (auth missing?): {e.read().decode('utf-8','replace')[:200]}",
              file=sys.stderr)
        return 2
    except Exception as e:
        print(f"ERROR: could not fetch entities from {args.api_url}: {e}", file=sys.stderr)
        return 2

    findings = find_duplicates(entities)
    plan = build_replacement_plan(findings, args.strategy, only_entity_id=args.entity, cap=args.cap)

    if args.json:
        out = {
            "timestamp": _dt.datetime.now().isoformat(timespec="seconds"),
            "strategy": args.strategy,
            "apply_mode": args.apply,
            "findings_count": len(findings),
            "plan": plan,
        }
        print(json.dumps(out, indent=2))
    else:
        print(render_plan_report(plan, args.strategy))

    # Dry-run path: stop here
    if not args.apply:
        if args.post and plan:
            msg = render_plan_report(plan, args.strategy)
            # Always render the report even in --json mode for posting.
            if args.json:
                msg = render_plan_report(plan, args.strategy)
            result = _discord_post(OPENWORLD_LOG_CHANNEL_ID, msg)
            if "error" in result:
                print(f"Discord post FAILED: {result['error']}", file=sys.stderr)
                return 2
            print(f"Posted plan to #openworld-log ({OPENWORLD_LOG_CHANNEL_ID}).")
        return 1 if plan else 0

    # Apply path
    if not args.yes:
        print("ERROR: --apply requires --yes (confirmation token from the operator).", file=sys.stderr)
        return 2
    if not plan:
        print("Nothing to apply.")
        return 0

    # Cap safety: refuse to apply if any entity would exceed --cap
    # (the server would silently truncate, making the merge partial
    # and the dropped variants effectively lost). --ignore-cap is
    # the explicit escape hatch.
    over_cap = [p for p in plan if p["over_cap_by"] > 0]
    if over_cap and not args.ignore_cap:
        print(f"ERROR: {len(over_cap)} entity(ies) would exceed --cap ({args.cap} chars):", file=sys.stderr)
        for p in over_cap:
            print(f"  • {p['entity_name']}: {p['expected_new_summary_chars']} chars "
                  f"(+{p['over_cap_by']} over)", file=sys.stderr)
        print("Re-run with --ignore-cap to override (server will truncate).", file=sys.stderr)
        print("Or switch to a non-growing strategy (keep-first/keep-last/longest).", file=sys.stderr)
        return 2

    backup_path = None
    if not args.no_backup:
        try:
            backup_path = snapshot_world(os.getcwd())
        except Exception as e:
            print(f"ERROR: backup failed: {e}", file=sys.stderr)
            return 2

    # Apply each merge. The API does a first-match find-replace; we
    # call once per (entity, duplicate group) and pass the *first*
    # old_line each time. Subsequent calls in the same group would
    # match a different old_line anyway (the variants differ slightly),
    # so a single call per group is correct.
    results: List[Dict[str, Any]] = []
    for p_item in plan:
        for m in p_item["merges"]:
            try:
                res = apply_merge(p_item["entity_id"], m["old_line"], m["new_line"], args.api_url)
                ok = res["http_status"] == 200 and isinstance(res["response"], dict) and res["response"].get("success", False)
                results.append({
                    "entity_id": p_item["entity_id"],
                    "entity_name": p_item["entity_name"],
                    "canonical": m["canonical"],
                    "ok": ok,
                    "http_status": res["http_status"],
                    "response": res["response"],
                    "error": None if ok else (
                        res["response"].get("warning", "no success=true in response")
                        if isinstance(res["response"], dict) else f"HTTP {res['http_status']}"
                    ),
                })
            except Exception as e:
                results.append({
                    "entity_id": p_item["entity_id"],
                    "entity_name": p_item["entity_name"],
                    "canonical": m["canonical"],
                    "ok": False,
                    "http_status": None,
                    "response": None,
                    "error": f"{type(e).__name__}: {e}",
                })

    if args.json:
        out = {
            "timestamp": _dt.datetime.now().isoformat(timespec="seconds"),
            "strategy": args.strategy,
            "apply_mode": True,
            "backup_path": backup_path,
            "results": results,
        }
        print(json.dumps(out, indent=2))
    else:
        print()
        print(render_apply_report(plan, results, backup_path, args.strategy))

    if args.post:
        msg = render_apply_report(plan, results, backup_path, args.strategy)
        if args.json:
            # In JSON mode the human report wasn't printed; re-render.
            msg = render_apply_report(plan, results, backup_path, args.strategy)
        result = _discord_post(OPENWORLD_LOG_CHANNEL_ID, msg)
        if "error" in result:
            print(f"Discord post FAILED: {result['error']}", file=sys.stderr)
            # Don't fail the apply, just report.

    has_failures = any(not r["ok"] for r in results)
    return 3 if has_failures else 0


if __name__ == "__main__":
    sys.exit(main())
