#!/usr/bin/env python3
"""
Cleanup pass: remove `last_processed_other_tick` key from
`properties_int` on every entity in the live world.

This is the operator-side cleanup for the v1→v2 migration.  After this
runs:

  * The new `WorldEntity::last_processed_other_tick` struct field is
    the ONLY place the marker lives.
  * The old `properties_int["last_processed_other_tick"]` key is
    gone from every entity.
  * The `LLM_INTERNAL_INT_PROPERTIES` const slice in
    `src/world_data/internal_properties.rs` is empty (the source code
    cleanup is a separate code-level change, not this script).

Idempotent: re-running on a world where the key is already gone is a
no-op (the DELETE endpoint returns 404 and we treat that as success).

Verification: a follow-up `GET /api/entities/:id` should show
`properties_int` with no `last_processed_other_tick` key, but the
top-level `last_processed_other_tick` field should still be present
with the migrated value.
"""
import json
import sys
import urllib.request
import urllib.error

API = "http://localhost:8081/api"
KEY = "last_processed_other_tick"


def fetch_entities():
    with urllib.request.urlopen(f"{API}/entities", timeout=5) as r:
        d = json.load(r)
    return d.get("data", [])


def delete_key(entity_id: str) -> tuple[str, str]:
    """DELETE the old key on one entity.  Returns (status, message)."""
    req = urllib.request.Request(
        f"{API}/entities/{entity_id}/properties/int/{KEY}",
        method="DELETE",
        headers={"Cookie": "openworld_auth=1"},
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return ("deleted", f"HTTP {r.status}")
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        # 404 is OK — the key wasn't there to begin with
        if e.code == 404:
            return ("absent", f"HTTP 404 (key already gone)")
        return ("error", f"HTTP {e.code}: {body[:200]}")
    except Exception as e:
        return ("error", f"exception: {e}")


def main():
    ents = fetch_entities()
    print(f"Loaded {len(ents)} entities. Cleaning up `{KEY}` from properties_int...\n")
    results = {"deleted": 0, "absent": 0, "error": 0, "had_no_field": 0, "had_value": 0}
    for e in ents:
        eid = e.get("id")
        name = e.get("name")
        old = e.get("properties_int", {}).get(KEY)
        new_field = e.get("last_processed_other_tick", 0)
        if old is None and new_field == 0:
            # Entity is brand new, never had the key.  Skip the DELETE.
            results["had_no_field"] += 1
            continue
        if old is not None:
            results["had_value"] += 1
        status, msg = delete_key(eid)
        results[status] = results.get(status, 0) + 1
        marker = "✗" if status == "error" else "✓"
        print(f"  [{marker}] {name[:32]:<32}  old_key={old}  new_field={new_field}  →  {status} ({msg})")

    print()
    print("=" * 70)
    print(
        f"Summary: {results['had_value']} entities had the old key, "
        f"{results['had_no_field']} were already clean, "
        f"{results.get('deleted', 0)} deleted, "
        f"{results.get('absent', 0)} already absent, "
        f"{results.get('error', 0)} errors"
    )
    print("=" * 70)

    if results.get("error"):
        sys.exit(1)


if __name__ == "__main__":
    main()
