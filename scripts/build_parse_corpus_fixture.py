#!/usr/bin/env python3
"""Build the parse-regression CORPUS fixture from the production error-logs.

`tests/fixtures/llm_parse_regression.json` pins ONE verbatim payload per
malformation class — the shape of the damage. This script builds the
companion fixture `tests/fixtures/llm_parse_regression_corpus.json`, which
pins several verbatim payloads per class, so that a parser change cannot
silently break a production shape that the single-payload fixture does not
cover (the malformation classes overlap in the wild: a truncated response
can also carry a trailing comma *and* a control character, and every
combination needs its own repair path).

Source of truth: the `Failed to parse LLM response:` entries in
`logs/error-log-*.log`, i.e. the responses the world server actually DROPPED.

    python3 scripts/build_parse_corpus_fixture.py            # write the fixture
    python3 scripts/build_parse_corpus_fixture.py --check    # verify it is current

Two kinds of dropped response are NOT put into the fixture, because no
repair can give them an action and pinning them would fail the suite:

  * a REFUSAL — the model declined to act ("I cannot generate an action
    for this entity…", or a `{"error": …}` envelope, or
    `{"type":"error","error":…}`). There is no action to recover.
  * a DEGENERATE response — a JSON value that carries no action at all
    (`{}`, or an array of pairs whose only pair is not an action).

Everything else is included: those are the payloads the parser must
recover, and the Rust test
`real_production_corpus_all_parse` asserts each one yields a non-empty
action name.

The output is deterministic (sorted by day, class, then payload), so the
fixture does not churn between runs.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from collections import Counter, defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from mine_parse_failures import classify, mine  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURE = os.path.join(ROOT, "tests", "fixtures", "llm_parse_regression_corpus.json")
REFUSAL_FIXTURE = os.path.join(ROOT, "tests", "fixtures", "llm_parse_refusals.json")

# Markers that mark a prose refusal. Kept conservative on purpose: a
# refusal is prose that starts with a self-reference ("I cannot…"), so a
# malformed JSON payload is never mistaken for one.
REFUSAL_MARKERS = (
    "cannot generate an action",
    "cannot generate a valid action",
    "can't generate an action",
    "cannot generate",
    "cannot suggest action",
    "cannot be written",
    "action blocked",
    "is tagged as",
    "i appreciate the detailed world-building",
)


def is_refusal_or_degenerate(payload: str) -> bool:
    """True when the payload carries no recoverable action."""
    text = payload.strip()
    if not text:
        return True

    # -- prose refusal (no JSON punctuation at all)
    if "{" not in text and "[" not in text:
        low = text.lower()
        return any(marker in low for marker in REFUSAL_MARKERS)

    # -- a JSON value we can read: does it carry an action?
    try:
        value = json.loads(text)
    except Exception:
        # Broken JSON: the parser has to repair it. Keep it — that is
        # exactly what this fixture is for.
        return False

    if isinstance(value, dict):
        if value.get("action"):
            return False
        # `{"error": …}` / `{"type":"error", …}` envelopes and `{}`.
        return True
    if isinstance(value, list):
        # An array of [key, value] pairs: usable only if one of the
        # pairs is the action.
        for pair in value:
            if isinstance(pair, list) and len(pair) == 2 and pair[0] == "action":
                return False
        return True
    return True


def build(max_per_class: int):
    payloads = mine()
    # Group by class, keep the first N of each class in a stable order.
    seen: dict[str, tuple[str, str, str]] = {}
    for day, cls, payload in payloads:
        seen.setdefault(payload, (day, cls, payload))

    ordered = sorted(seen.values(), key=lambda t: (t[0], t[1], t[2]))
    kept: list[str] = []
    refusals: list[str] = []
    counts: Counter = Counter()
    for day, cls, payload in ordered:
        if is_refusal_or_degenerate(payload):
            refusals.append(payload)
            continue
        if counts[cls] >= max_per_class:
            continue
        counts[cls] += 1
        kept.append(payload)
    return kept, refusals, counts


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--max-per-class", type=int, default=2,
                    help="how many verbatim payloads to keep per class")
    ap.add_argument("--check", action="store_true",
                    help="exit non-zero if the fixture on disk differs")
    args = ap.parse_args()

    kept, refusals, counts = build(args.max_per_class)
    text = json.dumps(kept, ensure_ascii=False, indent=2) + "\n"
    refusal_text = json.dumps(refusals, ensure_ascii=False, indent=2) + "\n"

    if args.check:
        current = ""
        if os.path.exists(FIXTURE):
            with open(FIXTURE, encoding="utf-8") as fh:
                current = fh.read()
        current_refusals = ""
        if os.path.exists(REFUSAL_FIXTURE):
            with open(REFUSAL_FIXTURE, encoding="utf-8") as fh:
                current_refusals = fh.read()
        if current != text or current_refusals != refusal_text:
            print(f"FIXTURE OUT OF DATE: {FIXTURE}", file=sys.stderr)
            return 1
        print(f"fixture is current ({len(kept)} payloads, {len(refusals)} refusals)")
        return 0

    os.makedirs(os.path.dirname(FIXTURE), exist_ok=True)
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        fh.write(text)
    with open(REFUSAL_FIXTURE, "w", encoding="utf-8") as fh:
        fh.write(refusal_text)

    print(f"wrote {len(kept)} payloads -> {FIXTURE} "
          f"({sum(len(p.encode()) for p in kept)} bytes)")
    for cls, n in counts.most_common():
        print(f"  {n:3}  {cls}")
    print(f"wrote {len(refusals)} refusal/degenerate payloads -> {REFUSAL_FIXTURE} "
          f"(no action to recover)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
