#!/usr/bin/env python3
"""ow_sanity_check.py — Read-only sanity check for the open-world-selena.

Per Arcurus 2026-06-05 (#openworld): read-only sanity check that scans
entity history summaries for duplicate relations, scans the day's
LLM log for the counts we report (both-rate, multi-replace, parse
errors, truncation events), and any other worth-reporting signal it
finds along the way. Never mutates anything; --apply is intentionally
NOT supported. Posts a Markdown report to a Discord channel (default
#openworld-log, id 1511696310984773633 — renumbered 2026-06-06; was 1511711721868230767, deleted by Discord).

Usage:
    python3 code/ow_sanity_check.py                 # scan + print to stdout
    python3 code/ow_sanity_check.py --post          # scan + post to Discord
    python3 code/ow_sanity_check.py --post --dry-run
                                                  # build report, do not post
    python3 code/ow_sanity_check.py --json          # machine-readable output
    python3 code/ow_sanity_check.py --api-url=URL   # custom server (default http://127.0.0.1:8081)

Exit codes:
    0 = clean (no findings worth reporting)
    1 = findings (rows in the report) — non-fatal
    2 = runtime error (couldn't reach server, parse failure, etc.)
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import sys
import urllib.error
import urllib.request
from collections import Counter, defaultdict
from typing import Any, Dict, List, Optional, Tuple

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
DEFAULT_API_URL = "http://127.0.0.1:8081"
DEFAULT_LOG_DIR = "logs"
DEFAULT_LOG_FILE = "llm-log-2026-06-04.log"  # updated dynamically
OPENWORLD_LOG_CHANNEL_ID = "1511696310984773633"  # #openworld-log (correct active id; MEMORY.md had a stale 404 id)
SUMMARY_HARD_CAP = 10_000  # max_history_summary_chars

# ---------------------------------------------------------------------------
# Discord post (adapted from selena-project/code/cost_tracker.py)
# ---------------------------------------------------------------------------
def _discord_token() -> Optional[str]:
    """Look for a Discord bot token in ~/.openclaw/openclaw.json.

    Walk common config shapes, return the first hit. Returns None if
    not found; the caller should fall back to "no_post" mode.
    """
    import pathlib
    candidates = [
        pathlib.Path.home() / ".openclaw" / "openclaw.json",
        pathlib.Path.home() / ".config" / "openclaw" / "openclaw.json",
    ]
    for path in candidates:
        if not path.exists():
            continue
        try:
            cfg = json.loads(path.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            continue
        for k_chain in (("discord", "token"),
                        ("channels", "discord", "token"),
                        ("plugins", "discord", "token")):
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


def _discord_post(channel_id: str, text: str) -> Dict[str, Any]:
    """POST a message to a Discord channel via the bot token.

    Returns the parsed JSON response on success, or {"error": ...}.
    Truncates text to 2000 chars (Discord limit) and joins multi-
    line content with \\n so newlines render in chat.

    IMPORTANT: Discord's API rejects requests that don't carry a proper
    User-Agent header (Cloudflare blocks them with HTTP 403 error
    1010). The minimal accepted UA is the
    'DiscordBot (https://github.com/discord/discord-api-clients, 1.0)'
    string — a custom UA like 'ow-sanity-check/1.0' is not enough.
    """
    token = _discord_token()
    if not token:
        return {"error": "no_bot_token"}
    url = f"https://discord.com/api/v10/channels/{channel_id}/messages"
    body = json.dumps({"content": text[:2000]}).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bot {token}",
            "Content-Type": "application/json",
            "User-Agent": "DiscordBot (https://github.com/discord/discord-api-clients, 1.0)",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return json.loads(resp.read().decode("utf-8") or "{}")
    except urllib.error.HTTPError as e:
        return {"error": f"http_{e.code}", "body": e.read().decode("utf-8", errors="ignore")[:300]}
    except urllib.error.URLError as e:
        return {"error": f"url_error: {e.reason}"}


# ---------------------------------------------------------------------------
# 1) Duplicate-relations scan (entity summaries)
# ---------------------------------------------------------------------------
_REL_SPLIT = re.compile(r"\s*→\s*")


def _split_relations(summary: str) -> List[Tuple[str, str]]:
    """Split a history_summary into (narrative, [(name, text), ...]).

    Relations are detected by the `→` marker (the LLM's convention).
    Each `→ X: ...` becomes a (X, text) tuple. The narrative is
    everything before the first →. If there are no →, we return
    ([], []).
    """
    if not summary:
        return [], []
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


def _normalize_rel_name(name: str) -> str:
    """Loose-name comparison: strip trailing punct, collapse whitespace, lowercase.

    So 'Elder Moonthorn.' and 'Elder Moonthorn' compare equal.
    'the  Keepers  of  the  Eternal  Flame' and 'The Keepers of the
    Eternal Flame' compare equal. 'Whisperwood' and 'Whisperwood Forest'
    do NOT compare equal (no fuzzy string match — those are genuinely
    different entities; let the operator decide).
    """
    return re.sub(r"\s+", " ", name.strip().rstrip(".,;:!?'\"")).lower()


def scan_duplicate_relations(entities: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """For each entity, find duplicate relation lines (same name > 1 time).

    Returns a list of findings, one per affected entity. Empty list = clean.
    """
    findings: List[Dict[str, Any]] = []
    for e in entities:
        summary = e.get("history_summary") or ""
        if not summary:
            continue
        _, relations = _split_relations(summary)
        if not relations:
            continue
        # Group by normalized name
        groups: Dict[str, List[Tuple[str, str]]] = defaultdict(list)
        for name, text in relations:
            groups[_normalize_rel_name(name)].append((name, text))
        dups = {k: v for k, v in groups.items() if len(v) > 1}
        if dups:
            findings.append({
                "entity_id": e.get("id"),
                "entity_name": e.get("name"),
                "entity_type": e.get("entity_type"),
                "n_relations": len(relations),
                "n_unique": len(groups),
                "duplicates": [
                    {"canonical": dups_list[0][0], "lines": dups_list}
                    for dups_list in dups.values()
                ],
            })
    return findings


# ---------------------------------------------------------------------------
# 2) LLM-call counts (today's log)
# ---------------------------------------------------------------------------
def _extract_response(b: str) -> Optional[Dict[str, Any]]:
    m = re.search(r"---\s*Response\s*---\s*\n(.*?)(?:\n---|\Z)", b, re.DOTALL)
    if not m:
        return None
    raw = m.group(1).strip()
    # Strip ```json fences (LLM sometimes adds them)
    raw = re.sub(r"^```(?:json)?\s*\n?", "", raw)
    raw = re.sub(r"\n?```\s*$", "", raw)
    try:
        return json.loads(raw)
    except Exception:
        s = raw.find("{"); e = raw.rfind("}")
        if s >= 0 and e > s:
            try:
                return json.loads(raw[s:e+1])
            except Exception:
                return None
    return None


def _get_ts(b: str) -> Optional[str]:
    m = re.search(r"\[(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\]", b)
    return m.group(1) if m else None


def _is_test_action(action: str) -> bool:
    return bool(re.match(r"^test_", action or "", re.IGNORECASE))


def scan_llm_log(log_path: str) -> Dict[str, Any]:
    """Analyze the day's LLM log; return counts + findings.

    Buckets per call: replace_only / full_only / both / neither / parse_error.
    Also counts: multi-replace calls (>1 pair in history_summary_replace),
    truncation events (in the parsing outcome), and the post-restart
    subset (the warnings-vec fix landed at the binary restart).
    """
    if not os.path.exists(log_path):
        return {"error": f"log not found: {log_path}"}

    with open(log_path) as f:
        content = f.read()
    blocks = [b for b in content.split("=== LLM Call ===") if b.strip()]

    # The fix landed on commit 3373e0d, service restart at 2026-06-04 22:05:52 UTC
    # (CEST 00:05:52). Anything strictly after that timestamp uses the new binary
    # with complete warnings in the log.
    RESTART_TS = "2026-06-04 22:05:52"

    stats_total = Counter()
    stats_post = Counter()
    multi_total = 0
    multi_post = 0
    parse_error_samples: List[str] = []
    truncation_post = 0
    truncation_total = 0
    both_actions_post: List[str] = []

    # Warning-categorization (per Arcurus 2026-06-05: "does the check now
    # look at warnings in the log?"). The new binary (commit 3373e0d)
    # writes a Python list literal into the "--- Parsing ---" line, so
    # we can pull every warning out and bucket by the substring that
    # identifies it. Keys are short, stable labels so the Markdown
    # report stays compact.
    warning_counts_all: Counter = Counter()
    warning_counts_post: Counter = Counter()
    warning_samples: Dict[str, List[str]] = {}

    # Patterns keyed off a stable substring of the warning text. Add new
    # buckets here when apply_history_summary_replaces or the truncation
    # path starts emitting new warnings.
    WARN_BUCKETS = [
        ("both_dropped",        "Both history_summary and history_summary_replace"),
        ("neither",             "Neither history_summary"),
        ("truncated",           "exceeded"),  # generic "exceeded N chars" truncation
        ("old_part_not_found",  "old_part not found"),
        ("old_part_unique",     "occurs more than once"),
        # Tolerant JSON repair (226e685 + 78ea1ac): the World Clock
        # entity has been emitting a recurring empty-key malformation
        # in `history_summary_replace` (e.g. `{"old_part":"...","":"new_part":"..."}`
        # — a spurious empty-string key whose value is the *name* of
        # the next real key, then the actual pair appended without a
        # separator). Strict serde_json rejects the whole response.
        # The server now runs a conservative regex repair and surfaces
        # a descriptive warning. Substring matches the start of the
        # warning string `parse_llm_action_response: LLM response
        # matched a known malformed pattern and was repaired ...`.
        # Useful to track so we know whether the bug is still
        # recurring in production (and how often our repair actually
        # fires).
        ("regex_repair",        "parse_llm_action_response: LLM response matched a known"),
        # System-entity protection (c7f3bc27 / d7bf225): the LLM
        # sometimes tries to write effects to the world clock or other
        # system entities; the server correctly rejects those. Expected
        # and benign, but a useful signal to keep an eye on.
        ("system_entity_targeted",  "Entity is a system entity"),
        ("skipped_effect_system",   "Skipped effect on system entity"),
        # Magnitude / delta cap (LLM trying to write a too-large number).
        ("skipped_effect_magnitude",  "Skipped effect on"),
    ]

    def _bucket(warning: str) -> str:
        for label, needle in WARN_BUCKETS:
            if needle in warning:
                return label
        return "other"

    def _parse_warnings_from_parsing_line(parsing_line: str) -> List[str]:
        """Pull out the Python-list of warnings from the parsing line.

        The parsing line looks like:
          Applied 3 effects. Warnings: ["Both history_summary ...", "..."]
        Older builds (pre-3373e0d) write `Warnings: []`. We do a tolerant
        parse: find the first '[' and the matching ']' (handling nested
        quoted strings naively) and split top-level quoted entries.
        """
        idx = parsing_line.find("Warnings:")
        if idx < 0:
            return []
        tail = parsing_line[idx + len("Warnings:"):].strip()
        if not (tail.startswith("[") and tail.endswith("]")):
            return []
        body = tail[1:-1]
        if not body.strip():
            return []
        # Naive split on '", "' — works for the simple string-only lists
        # the current apply helper emits (no embedded commas in the
        # warning text). If the format ever gets fancier, switch to a
        # proper Python-literal parser here.
        parts = re.findall(r'"((?:[^"\\]|\\.)*)"', body)
        return [p for p in parts]

    for b in blocks:
        ts = _get_ts(b) or ""
        is_post = ts >= RESTART_TS
        obj = _extract_response(b)
        if obj is None:
            stats_total["parse_error"] += 1
            if is_post:
                stats_post["parse_error"] += 1
            m = re.search(r"---\s*Response\s*---\s*\n(.*?)(?:\n---|\Z)", b, re.DOTALL)
            if m and len(parse_error_samples) < 3:
                parse_error_samples.append(m.group(1).strip()[:120])
            continue
        has_r = "history_summary_replace" in obj
        has_f = "history_summary" in obj
        if has_r and has_f:
            stats_total["both"] += 1
            if is_post:
                stats_post["both"] += 1
                a = obj.get("action", "?")
                if not _is_test_action(a):
                    both_actions_post.append(a)
        elif has_r:
            stats_total["replace_only"] += 1
            if is_post:
                stats_post["replace_only"] += 1
        elif has_f:
            stats_total["full_only"] += 1
            if is_post:
                stats_post["full_only"] += 1
        else:
            stats_total["neither"] += 1
            if is_post:
                stats_post["neither"] += 1

        hsr = obj.get("history_summary_replace")
        if isinstance(hsr, list) and len(hsr) > 1:
            multi_total += 1
            if is_post:
                multi_post += 1

        # Walk the warnings array (if any) and bucket them.
        m_p = re.search(r"---\s*Parsing\s*---\s*\n(.*?)(?:\n---|\Z)", b, re.DOTALL)
        if m_p:
            for w in _parse_warnings_from_parsing_line(m_p.group(1)):
                bucket = _bucket(w)
                warning_counts_all[bucket] += 1
                if is_post:
                    warning_counts_post[bucket] += 1
                if len(warning_samples.setdefault(bucket, [])) < 3:
                    warning_samples[bucket].append(w)

    total_calls = sum(stats_total.values())
    post_calls = sum(stats_post.values())

    def pct(part: int, whole: int) -> str:
        return f"{(100 * part // whole) if whole else 0}%"

    return {
        "log_path": log_path,
        "total_calls_today": total_calls,
        "post_restart_calls": post_calls,
        "post_restart": dict(stats_post),
        "post_restart_pct": {
            "both": pct(stats_post["both"], post_calls),
            "replace_only": pct(stats_post["replace_only"], post_calls),
            "full_only": pct(stats_post["full_only"], post_calls),
        },
        "multi_replace_all_day": multi_total,
        "multi_replace_post_restart": multi_post,
        "truncation_events_all_day": truncation_total,
        "truncation_events_post_restart": truncation_post,
        "sample_parse_errors": parse_error_samples,
        "sample_both_actions_post": both_actions_post[:5],
        "warning_counts_all": dict(warning_counts_all),
        "warning_counts_post": dict(warning_counts_post),
        "warning_samples": warning_samples,
    }


# ---------------------------------------------------------------------------
# 3) Other worth-reporting signals
# ---------------------------------------------------------------------------
def scan_summary_lengths(entities: List[Dict[str, Any]]) -> Dict[str, Any]:
    """Distribution of summary lengths + any over the hard cap."""
    lengths = [(e.get("name"), e.get("entity_type"),
                len(e.get("history_summary") or "")) for e in entities]
    over_cap = [x for x in lengths if x[2] > SUMMARY_HARD_CAP]
    no_summary = [e.get("name") for e in entities if not e.get("history_summary")]
    if lengths:
        ls = sorted(l[2] for l in lengths)
        median = ls[len(ls) // 2]
        longest = max(lengths, key=lambda x: x[2])
    else:
        median, longest = 0, ("?", "?", 0)
    return {
        "n_entities": len(entities),
        "n_with_summary": len(lengths),
        "n_without_summary": len(no_summary),
        "n_over_cap": len(over_cap),
        "median_chars": median,
        "longest": {"name": longest[0], "type": longest[1], "chars": longest[2]},
        "over_cap_examples": [{"name": n, "type": t, "chars": c} for n, t, c in over_cap[:5]],
        "no_summary_examples": no_summary[:5],
    }


def scan_stale_entities(entities: List[Dict[str, Any]],
                        stale_hours: int = 24) -> List[Dict[str, Any]]:
    """Entities whose last action is older than stale_hours (best-effort).

    Uses the last entry in entity.history if present. Returns up to 10
    examples with the gap in hours.
    """
    now = _dt.datetime.now(_dt.timezone.utc)
    out: List[Dict[str, Any]] = []
    for e in entities:
        history = e.get("history") or []
        if not history:
            out.append({"name": e.get("name"), "id": e.get("id"),
                        "last_action": None, "age_hours": None,
                        "n_actions": 0})
            continue
        last = history[-1]
        ts_str = last.get("timestamp") or last.get("created_at")
        if not ts_str:
            continue
        try:
            ts = _dt.datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        except Exception:
            continue
        age = (now - ts).total_seconds() / 3600.0
        if age >= stale_hours:
            out.append({
                "name": e.get("name"), "id": e.get("id"),
                "last_action": last.get("action"),
                "age_hours": round(age, 1),
                "n_actions": len(history),
            })
    out.sort(key=lambda x: x.get("age_hours") or 0, reverse=True)
    return out[:10]


# ---------------------------------------------------------------------------
# Fetch entities from the running server
# ---------------------------------------------------------------------------
def fetch_entities(api_url: str) -> List[Dict[str, Any]]:
    url = f"{api_url.rstrip('/')}/api/entities"
    with urllib.request.urlopen(url, timeout=10) as r:
        data = json.load(r)
    if isinstance(data, list):
        return data
    return data.get("data", data.get("entities", []))


# ---------------------------------------------------------------------------
# Render as Discord-friendly Markdown
# ---------------------------------------------------------------------------
def render_report(rels: List[Dict[str, Any]],
                  log_stats: Dict[str, Any],
                  lengths: Dict[str, Any],
                  stale: List[Dict[str, Any]]) -> str:
    parts: List[str] = []
    parts.append(f"🛡️ **open-world sanity check** — {_dt.datetime.now().isoformat(timespec='seconds')}")
    parts.append("")

    # === Duplicate relations ===
    if rels:
        parts.append(f"⚠️  **Duplicate relations** ({len(rels)} entities):")
        for r in rels[:5]:
            parts.append(f"  • **{r['entity_name']}** ({r['entity_type']}) — "
                         f"{r['n_relations']} lines, {r['n_unique']} unique")
            for d in r["duplicates"]:
                lines = d["lines"]
                parts.append(f"    → `{d['canonical']}` appears {len(lines)}×:")
                for i, (_, text) in enumerate(lines, 1):
                    short = text[:80] + ("…" if len(text) > 80 else "")
                    parts.append(f"       {i}. {short}")
        if len(rels) > 5:
            parts.append(f"  …and {len(rels) - 5} more (truncated for Discord)")
    else:
        parts.append("✅ Duplicate relations: **clean** (every entity's summary has unique relation names)")
    parts.append("")

    # === LLM call counts ===
    if "error" not in log_stats:
        parts.append(f"📊 **LLM call counts** (log: `{os.path.basename(log_stats['log_path'])}`):")
        parts.append(f"  • total today: **{log_stats['total_calls_today']}**")
        parts.append(f"  • post-restart: **{log_stats['post_restart_calls']}** "
                     f"(binary restart = commit 3373e0d)")
        pr = log_stats["post_restart"]
        parts.append(f"  • post-restart breakdown: "
                     f"`both`={pr.get('both',0)} ({log_stats['post_restart_pct']['both']}), "
                     f"`replace_only`={pr.get('replace_only',0)} ({log_stats['post_restart_pct']['replace_only']}), "
                     f"`full_only`={pr.get('full_only',0)} ({log_stats['post_restart_pct']['full_only']}), "
                     f"`neither`={pr.get('neither',0)}, "
                     f"`parse_error`={pr.get('parse_error',0)}")
        parts.append(f"  • multi-replace (≥2 pairs in one LLM response): "
                     f"all-day = {log_stats['multi_replace_all_day']}, "
                     f"post-restart = {log_stats['multi_replace_post_restart']}")
        parts.append(f"  • truncation events: "
                     f"all-day = {log_stats['truncation_events_all_day']}, "
                     f"post-restart = {log_stats['truncation_events_post_restart']}")

        # === Warnings bucketed from the per-call warnings: [...] list ===
        w_all = log_stats.get("warning_counts_all") or {}
        w_post = log_stats.get("warning_counts_post") or {}
        w_samples = log_stats.get("warning_samples") or {}
        if w_all or w_post:
            parts.append("  • warnings bucketed (from `Warnings: [...]` per call):")
            # Stable order, all buckets even when zero, so the report shape
            # is stable run-to-run.
            bucket_order = ["both_dropped", "neither", "truncated",
                            "old_part_not_found", "old_part_unique",
                            "regex_repair",
                            "system_entity_targeted", "skipped_effect_system",
                            "skipped_effect_magnitude", "other"]
            bucket_labels = {
                "both_dropped":            "Both dropped (replace wins)",
                "neither":                 "Neither (no update)",
                "truncated":               "Truncated (over cap)",
                "old_part_not_found":      "old_part not found",
                "old_part_unique":         "old_part ambiguous (occurs N×)",
                "regex_repair":            "Regex repair (LLM empty-key bug)",
                "system_entity_targeted":  "System entity targeted",
                "skipped_effect_system":   "Skipped effect (system entity)",
                "skipped_effect_magnitude":"Skipped effect (magnitude cap)",
                "other":                   "Other (unrecognized)",
            }
            for b in bucket_order:
                a = w_all.get(b, 0)
                p = w_post.get(b, 0)
                if a == 0 and p == 0:
                    continue  # skip empty buckets so the report stays tight
                parts.append(f"    - {bucket_labels.get(b, b)}: "
                             f"all-day = {a}, post-restart = {p}")
            # Show up to 2 samples per bucket for the post-restart warnings.
            any_samples = False
            for b, samples in w_samples.items():
                if not samples:
                    continue
                if not any_samples:
                    parts.append("  • warning samples:")
                    any_samples = True
                for s in samples[:2]:
                    short = s if len(s) <= 140 else s[:137] + "…"
                    parts.append(f"    - `{bucket_labels.get(b, b)}`: {short}")
        else:
            parts.append("  • warnings bucketed: **none** "
                         "(the binary is writing the warnings: [...] list — "
                         "if you see this line, double-check the log format)")

        if log_stats["sample_parse_errors"]:
            parts.append("  • parse-error samples:")
            for s in log_stats["sample_parse_errors"]:
                parts.append(f"    - `{s}`")
        if log_stats["sample_both_actions_post"]:
            parts.append("  • sample `both` actions post-restart: "
                         + ", ".join(f"`{a}`" for a in log_stats["sample_both_actions_post"]))
    else:
        parts.append(f"⚠️ LLM log scan skipped: {log_stats['error']}")
    parts.append("")

    # === Summary length distribution ===
    parts.append(f"📏 **Summary length** (cap = {SUMMARY_HARD_CAP}):")
    parts.append(f"  • entities with summary: **{lengths['n_with_summary']}** / {lengths['n_entities']}")
    if lengths["n_without_summary"]:
        parts.append(f"  • no-summary entities: {lengths['n_without_summary']} "
                     f"({', '.join(lengths['no_summary_examples'][:3])}{'…' if lengths['n_without_summary']>3 else ''})")
    parts.append(f"  • median: **{lengths['median_chars']}** chars")
    if lengths["longest"].get("name"):
        parts.append(f"  • longest: **{lengths['longest']['name']}** = {lengths['longest']['chars']} chars")
    if lengths["n_over_cap"]:
        parts.append(f"  ⚠️ over cap: {lengths['n_over_cap']}")
        for ex in lengths["over_cap_examples"]:
            parts.append(f"    - {ex['name']} ({ex['type']}) = {ex['chars']} chars")
    parts.append("")

    # === Stale entities ===
    if stale:
        parts.append(f"🕰️  **Stale entities** (no action in 24+ h, top 10):")
        for s in stale:
            if s.get("age_hours") is None:
                parts.append(f"  • {s['name']} — never acted")
            else:
                parts.append(f"  • {s['name']} — last action: `{s['last_action']}` ({s['age_hours']} h ago, {s['n_actions']} total)")
    else:
        parts.append("✅ Stale entities: **none** (every entity acted in the last 24 h)")
    parts.append("")

    parts.append("_read-only check; no changes applied. per Arcurus 2026-06-05 #openworld._")
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main() -> int:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--api-url", default=DEFAULT_API_URL,
                   help=f"open-world-selena API base URL (default {DEFAULT_API_URL})")
    p.add_argument("--log-dir", default=DEFAULT_LOG_DIR,
                   help=f"directory containing llm-log-*.log (default {DEFAULT_LOG_DIR})")
    p.add_argument("--post", action="store_true",
                   help=f"post the report to #{OPENWORLD_LOG_CHANNEL_ID} "
                        "(#openworld-log) via the Discord bot token")
    p.add_argument("--dry-run", action="store_true",
                   help="build the report but do not POST even with --post")
    p.add_argument("--json", action="store_true",
                   help="emit machine-readable JSON to stdout instead of Markdown")
    args = p.parse_args()

    # Resolve today's log file dynamically
    today = _dt.date.today().isoformat()
    log_path = os.path.join(args.log_dir, f"llm-log-{today}.log")
    if not os.path.exists(log_path):
        # fall back to the most recent llm-log-*.log in the dir
        candidates = sorted(
            [f for f in os.listdir(args.log_dir) if f.startswith("llm-log-")],
            reverse=True,
        )
        if candidates:
            log_path = os.path.join(args.log_dir, candidates[0])

    # === Scan 1: entities (duplicate relations + summary lengths + staleness) ===
    try:
        entities = fetch_entities(args.api_url)
    except Exception as e:
        print(f"ERROR: could not fetch entities from {args.api_url}: {e}", file=sys.stderr)
        return 2

    rel_findings = scan_duplicate_relations(entities)
    lengths = scan_summary_lengths(entities)
    stale = scan_stale_entities(entities)

    # === Scan 2: LLM call counts ===
    log_stats = scan_llm_log(log_path)

    if args.json:
        out = {
            "timestamp": _dt.datetime.now().isoformat(timespec="seconds"),
            "duplicate_relations": rel_findings,
            "llm_log": log_stats,
            "summary_lengths": lengths,
            "stale_entities": stale,
        }
        print(json.dumps(out, indent=2))
    else:
        report = render_report(rel_findings, log_stats, lengths, stale)
        print(report)

    # === Post to Discord ===
    if args.post and not args.dry_run:
        if args.json:
            print("\n--post is not supported with --json; print the report first.", file=sys.stderr)
            return 1
        report = render_report(rel_findings, log_stats, lengths, stale)
        result = _discord_post(OPENWORLD_LOG_CHANNEL_ID, report)
        if "error" in result:
            print(f"\nDiscord post FAILED: {result['error']}", file=sys.stderr)
            return 1
        print(f"\nPosted to #openworld-log ({OPENWORLD_LOG_CHANNEL_ID}) OK.")
    elif args.post and args.dry_run:
        print("\n--dry-run: would have posted the report above to "
              f"#{OPENWORLD_LOG_CHANNEL_ID}.")

    # Exit code: 0 if no findings, 1 if findings (still non-fatal)
    has_findings = bool(rel_findings) or bool(stale)
    if "error" in log_stats:
        has_findings = True
    return 1 if has_findings else 0


if __name__ == "__main__":
    sys.exit(main())
