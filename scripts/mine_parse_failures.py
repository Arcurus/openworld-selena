#!/usr/bin/env python3
"""Mine malformed LLM payloads out of the open-world-selena error logs.

The world server drops an LLM action response whenever
``parse_llm_action_response`` returns an Err.  Every such drop is
written to ``logs/error-log-YYYY-MM-DD.log`` as::

    [2026-10-24 07:55:47] ERROR: Failed to parse LLM response: \
        JSON parse error: <serde message> - Input: <raw payload>

The raw payload may span many lines (the LLM pretty-prints JSON
sometimes), so an entry ends at the next ``[timestamp] ERROR:`` line.

This script extracts those payloads, groups them by the *shape* of the
malformation (not the raw serde message, which embeds line/column
offsets that differ per payload), and writes them out as a JSON array
of strings — exactly the shape ``tests/fixtures/llm_parse_regression.json``
expects.

Usage:
    python3 scripts/mine_parse_failures.py --out /tmp/probe.json
    python3 scripts/mine_parse_failures.py --since 2026-10-01 --list-classes

The output is deliberately deterministic (sorted) so the fixture file
does not churn between runs.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import sys
from collections import Counter, defaultdict

LOG_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "logs")

# An entry header: "[2026-10-24 07:55:47] ERROR: ..."
ENTRY_RE = re.compile(r"^\[\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\]\s+ERROR:\s*")
TS_RE = re.compile(r"^\[(\d{4}-\d{2}-\d{2}) ")

MARKER = "Failed to parse LLM response:"
SEP = " - Input: "


def iter_entries(text: str):
    """Yield (date, body) for every ERROR entry in a log file."""
    cur_date = None
    buf: list[str] = []
    for line in text.splitlines():
        m = ENTRY_RE.match(line)
        if m:
            if buf:
                yield cur_date, "\n".join(buf)
            buf = [line[m.end():]]
            d = TS_RE.match(line)
            cur_date = d.group(1) if d else None
        elif buf:
            buf.append(line)
    if buf:
        yield cur_date, "\n".join(buf)


def extract_payload(body: str):
    """Return (err_head, raw_payload) or (None, None)."""
    if MARKER not in body:
        return None, None
    rest = body.split(MARKER, 1)[1]
    if SEP not in rest:
        return None, None
    err_head, payload = rest.split(SEP, 1)
    return err_head.strip(), payload


def classify(err_head: str) -> str:
    """Bucket a serde error message into a stable malformation class."""
    e = err_head.replace("JSON parse error: ", "").strip()
    # Drop the "at line N column M" tail — offsets are payload-specific.
    e = re.sub(r"\s*at line \d+ column \d+\s*$", "", e)
    e = re.sub(r"\s*\(line \d+\)$", "", e)
    # Collapse the serde tail after "got: " / " - " into a short shape.
    for sep in ("got: ", ": "):
        if sep in e and len(e.split(sep)[0]) < 120:
            head = e.split(sep)[0].strip()
            if head:
                return head
    return e[:120].strip()


def mine(since: str | None = None, until: str | None = None):
    files = sorted(glob.glob(os.path.join(LOG_DIR, "error-log-*.log")))
    payloads: list[tuple[str, str, str]] = []  # (date, class, raw)
    for path in files:
        base = os.path.basename(path)
        day = base.replace("error-log-", "").replace(".log", "")
        if since and day < since:
            continue
        if until and day > until:
            continue
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                text = fh.read()
        except OSError:
            continue
        for day_seen, body in iter_entries(text):
            err_head, payload = extract_payload(body)
            if payload is None:
                continue
            cls = classify(err_head or "")
            payloads.append((day_seen or day, cls, payload))
    return payloads


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--since", help="only logs from YYYY-MM-DD onward")
    ap.add_argument("--until", help="only logs up to YYYY-MM-DD")
    ap.add_argument("--out", help="write the JSON array of payloads here")
    ap.add_argument("--list-classes", action="store_true")
    ap.add_argument("--unique-by-class", action="store_true",
                    help="keep only the first payload of each malformation class")
    ap.add_argument("--max-per-class", type=int, default=0,
                    help="keep at most N payloads per class (0 = unlimited)")
    args = ap.parse_args()

    payloads = mine(args.since, args.until)
    if not payloads:
        print("no parse failures found", file=sys.stderr)
        return 1

    counts = Counter(c for _d, c, _p in payloads)
    print(f"total parse failures: {len(payloads)}")
    for cls, n in counts.most_common():
        print(f"  {n:5d}  {cls}")

    if args.list_classes:
        return 0

    chosen = payloads
    if args.unique_by_class or args.max_per_class:
        seen: Counter = Counter()
        kept = []
        for day, cls, payload in payloads:
            if args.max_per_class and seen[cls] >= args.max_per_class:
                continue
            seen[cls] += 1
            kept.append((day, cls, payload))
        chosen = kept

    # Deduplicate byte-identical payloads, keep first occurrence order.
    uniq: dict[str, tuple[str, str, str]] = {}
    for day, cls, payload in chosen:
        if payload not in uniq:
            uniq[payload] = (day, cls, payload)
    ordered = sorted(uniq.values(), key=lambda t: (t[0], t[1]))

    out_path = args.out or "/tmp/ow_parse_failures.json"
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump([p for _d, _c, p in ordered], fh, ensure_ascii=False, indent=2)
    print(f"\nwrote {len(ordered)} distinct payloads -> {out_path}")
    for day, cls, payload in ordered:
        print(f"  {day}  {cls[:70]:70s} len={len(payload)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
