#!/usr/bin/env python3
"""One-time cleanup: delete 'messed up' dot-key properties (Arcurus 2026-06-06 22:44).

Per Arcurus in #openworld (msg 1512950316738416751):
  "yea apply the suggested power changed. and one time delete the properties
   which are messed up with a . in them"

The dot-key effect parser fix (commit 6deb73c) prevents new garbage, but old
garbage from before the fix is still in the save file. The literal interpretation
is "keys with a dot" — but in practice the LLM emitted them as space-separated
or underscore-concatenated multi-word entity names plus a single-word property
(e.g. "Shadow Ridge Camp.power" → "Shadow Ridge Camp_power" or
"Shadow_Ridge_Camp_power"). So we also catch the common variants.

Detection rule (conservative — only keys that look like LLM-emit garbage):
  1. key contains a space (very unusual in property names), OR
  2. key matches the pattern [A-Z][a-z]+_[A-Z] — multi-word entity name
     flattened to underscores, e.g. "Shadow_Ridge_Camp_power"

Idempotent: re-runnable; checks each candidate key before deleting.

Outputs a summary at the end and exits 0 on success.
"""
import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request

API = os.environ.get("OW_API", "http://127.0.0.1:8081")
PASSWORD = os.environ.get("WEB_PASSWORD", "")

# Detection rules — see module docstring
KEY_HAS_SPACE = re.compile(r"\s")
KEY_CAMEL_UNDERSCORE = re.compile(r"[A-Z][a-z]+_[A-Z]")


def is_messed(key: str) -> bool:
    return bool(KEY_HAS_SPACE.search(key) or KEY_CAMEL_UNDERSCORE.search(key))


def http_request(method: str, path: str, body: dict | None = None,
                 cookies: list[str] | None = None) -> tuple[int, dict]:
    url = f"{API}{path}"
    headers = {"Accept": "application/json"}
    if cookies:
        headers["Cookie"] = "; ".join(cookies)
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, json.loads(resp.read().decode("utf-8") or "{}")
    except urllib.error.HTTPError as e:
        try:
            payload = json.loads(e.read().decode("utf-8") or "{}")
        except Exception:
            payload = {"raw": "<unparseable>"}
        return e.code, payload


def login() -> str:
    if not PASSWORD:
        print("ERROR: WEB_PASSWORD env var is required", file=sys.stderr)
        sys.exit(2)
    code, body = http_request("POST", "/api/env/verify-password", {"password": PASSWORD})
    if code != 200 or not body.get("verified"):
        print(f"ERROR: login failed (HTTP {code}): {body}", file=sys.stderr)
        sys.exit(2)
    return body["cookie_name"] + "=1"


def list_entities(cookie: str) -> list[dict]:
    code, body = http_request("GET", "/api/entities", cookies=[cookie])
    if code != 200:
        print(f"ERROR: list entities failed (HTTP {code}): {body}", file=sys.stderr)
        sys.exit(2)
    return body.get("data", [])


def find_messed(entities: list[dict]) -> list[tuple[str, str, str]]:
    """Return [(entity_id, prop_kind, key), ...] for every messed-up key."""
    out = []
    for e in entities:
        for k in e.get("properties_int", {}).keys():
            if is_messed(k):
                out.append((e["id"], "int", k))
        for k in e.get("properties_float", {}).keys():
            if is_messed(k):
                out.append((e["id"], "float", k))
        for k in e.get("properties_string", {}).keys():
            if is_messed(k):
                out.append((e["id"], "string", k))
    return out


def delete_property(cookie: str, entity_id: str, kind: str, key: str) -> tuple[int, dict]:
    encoded_key = urllib.parse.quote(key, safe="")
    code, body = http_request(
        "DELETE",
        f"/api/entities/{entity_id}/properties/{kind}/{encoded_key}",
        cookies=[cookie],
    )
    return code, body


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dry-run", action="store_true",
                    help="list candidates but do not delete")
    ap.add_argument("--show", action="store_true",
                    help="show all property keys (for debugging)")
    args = ap.parse_args()

    cookie = login()
    print(f"✓ Logged in (cookie={cookie!r})")

    entities = list_entities(cookie)
    print(f"✓ Listed {len(entities)} entities")

    if args.show:
        for e in entities:
            for kind in ("int", "float", "string"):
                for k in e.get(f"properties_{kind}", {}).keys():
                    marker = "  MESSED" if is_messed(k) else ""
                    print(f"  {e['name']:30s} {kind:6s} {k!r}{marker}")
        return 0

    candidates = find_messed(entities)
    if not candidates:
        print("✓ No messed-up properties found — world is clean.")
        return 0

    print(f"Found {len(candidates)} messed-up property key(s):")
    for ent_id, kind, key in candidates:
        print(f"  - {ent_id[:8]}  {kind:6s}  {key!r}")

    if args.dry_run:
        print("\n--dry-run set; no deletions performed.")
        return 0

    # Group by entity for cleaner output
    by_entity: dict[str, list[tuple[str, str]]] = {}
    for ent_id, kind, key in candidates:
        by_entity.setdefault(ent_id, []).append((kind, key))

    ok = 0
    fail = 0
    for ent_id, items in by_entity.items():
        for kind, key in items:
            code, body = delete_property(cookie, ent_id, kind, key)
            if code == 200 and body.get("success"):
                ok += 1
                print(f"  ✓ deleted {kind} {key!r}")
            else:
                fail += 1
                print(f"  ✗ FAILED {kind} {key!r} (HTTP {code}): {body}")

    # Save
    print("\nTriggering world save to persist the cleanup...")
    code, body = http_request("POST", "/api/world/save", cookies=[cookie])
    if code == 200 and body.get("success"):
        print(f"  ✓ saved (path={body.get('path','?')}, "
              f"size={body.get('size_bytes','?')} bytes)")
    else:
        print(f"  ! save call returned HTTP {code}: {body}")

    print(f"\n=== Summary: {ok} deleted, {fail} failed ===")
    return 0 if fail == 0 else 1


if __name__ == "__main__":
    import urllib.parse  # used in delete_property
    sys.exit(main())
