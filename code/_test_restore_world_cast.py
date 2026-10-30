#!/usr/bin/env python3
"""Smoke test for the offline-testable parts of restore_world_cast.py.

Mirrors the pattern of _test_merge_relations.py: runs the pure logic
against synthesized data — no live server, no live world touched.

Verifies:
  1. is_system_entity() mirrors the server's predicate (world_clock type,
     abstract type, 'meta' tag) — the same rule the scheduler uses.
  2. The confirmation token check refuses a bare --apply and any wrong
     token, and accepts exactly RESTORE-WORLD-CAST.
  3. A candidate that does not exist fails preflight (exit 1) and never
     reaches the apply stage.
  4. A candidate that exists but fails the isolated verification is
     refused before the live world is touched (exit 1).
  5. sha256_of() is stable and matches hashlib for the same file.
  6. archive_live_world() writes timestamped copies into world_data/backups
     and never deletes the source files.
"""
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, "/home/openclaw/openclaw/workspace/open-world-selena/code")

import restore_world_cast as rwc  # noqa: E402

FAILS = []


def check(name, cond, detail=""):
    if cond:
        print(f"  ✅ {name}")
    else:
        print(f"  ❌ {name} — {detail}")
        FAILS.append(name)


print("== 1. is_system_entity() ==")
check("world_clock type is system", rwc.is_system_entity({"entity_type": "world_clock"}))
check("abstract type is system", rwc.is_system_entity({"entity_type": "abstract"}))
check("meta tag is system",
      rwc.is_system_entity({"entity_type": "character", "tags": ["meta"]}))
check("meta tag as bare string is system",
      rwc.is_system_entity({"entity_type": "character", "tags": "meta"}))
check("normal character is NOT system",
      not rwc.is_system_entity({"entity_type": "character", "tags": ["noble"]}))
check("entity with no type/tag is NOT system",
      not rwc.is_system_entity({"name": "Kira Dawnblade"}))

print("== 2. confirmation token ==")
check("CONFIRM_TOKEN is the documented literal", rwc.CONFIRM_TOKEN == "RESTORE-WORLD-CAST")
# main() must refuse --apply without the exact token. Point the candidate at
# a real file so preflight passes and we reach the token gate.
tmp = tempfile.mkdtemp(prefix="ow-restore-test-")
try:
    cand = os.path.join(tmp, "cand.owbl")
    with open(cand, "wb") as fh:
        fh.write(b"not-a-real-save")
    orig_verify = rwc.verify_candidate_offline
    rwc.verify_candidate_offline = lambda c, timeout_s=90: {
        "ok": True, "entity_count": 50, "non_system_count": 49,
        "port": 1, "sample_names": [],
    }
    try:
        rc = rwc.main(["--candidate", cand, "--apply"])
        check("bare --apply is refused (exit 4)", rc == 4, f"got {rc}")
        rc = rwc.main(["--candidate", cand, "--apply", "--yes=WRONG"])
        check("wrong --yes is refused (exit 4)", rc == 4, f"got {rc}")
    finally:
        rwc.verify_candidate_offline = orig_verify

    print("== 3. missing candidate fails preflight ==")
    orig_verify = rwc.verify_candidate_offline
    called = {"v": False}
    def _sentinel(c, timeout_s=90):
        called["v"] = True
        return {"ok": True, "entity_count": 50, "non_system_count": 49}
    rwc.verify_candidate_offline = _sentinel
    try:
        rc = rwc.main(["--candidate", os.path.join(tmp, "nope.owbl")])
        check("missing candidate exits 1", rc == 1, f"got {rc}")
        check("missing candidate never reaches verification", not called["v"])
    finally:
        rwc.verify_candidate_offline = orig_verify

    print("== 4. failing verification is refused before any swap ==")
    orig_verify = rwc.verify_candidate_offline
    orig_swap = rwc.swap_in_candidate
    swapped = {"v": False}
    rwc.verify_candidate_offline = lambda c, timeout_s=90: {
        "ok": False, "error": "instance exited early",
    }
    def _swap_sentinel(*a, **kw):
        swapped["v"] = True
        return {"ok": True}
    rwc.swap_in_candidate = _swap_sentinel
    try:
        rc = rwc.main(["--candidate", cand, "--apply", "--yes=RESTORE-WORLD-CAST"])
        check("failed verification exits 1", rc == 1, f"got {rc}")
        check("failed verification never calls swap_in_candidate", not swapped["v"])
    finally:
        rwc.verify_candidate_offline = orig_verify
        rwc.swap_in_candidate = orig_swap

    print("== 5. sha256_of() ==")
    import hashlib
    with open(cand, "rb") as fh:
        want = hashlib.sha256(fh.read()).hexdigest()
    check("sha256_of matches hashlib", rwc.sha256_of(cand) == want)
    check("sha256_of is stable", rwc.sha256_of(cand) == rwc.sha256_of(cand))

    print("== 6. archive_live_world() ==")
    # Redirect the module's paths at the temp dir for this check.
    orig_live, orig_last = rwc.LIVE_SAVE, rwc.LAST_ACTION
    orig_bdir = rwc.BACKUP_DIR
    live = os.path.join(tmp, "world_data", "save.owbl")
    lastact = os.path.join(tmp, "world_data", "ow_entity_last_action.json")
    bdir = os.path.join(tmp, "world_data", "backups")
    os.makedirs(os.path.dirname(live), exist_ok=True)
    with open(live, "wb") as fh:
        fh.write(b"live-save-bytes")
    with open(lastact, "w") as fh:
        json.dump({"abc": 1.0}, fh)
    rwc.LIVE_SAVE, rwc.LAST_ACTION, rwc.BACKUP_DIR = live, lastact, bdir
    try:
        out = rwc.archive_live_world()
        check("save archive recorded", "save_archive" in out, str(out))
        check("last_action archive recorded", "last_action_archive" in out)
        check("save archive exists on disk", os.path.isfile(out.get("save_archive", "")))
        check("archived save has the same bytes",
              open(out["save_archive"], "rb").read() == b"live-save-bytes")
        check("live save still present (nothing deleted)", os.path.isfile(live))
        check("last_action still present (nothing deleted)", os.path.isfile(lastact))
        check("archive filename is timestamped",
              "save-pre-restore-" in os.path.basename(out.get("save_archive", "")),
              out.get("save_archive"))
    finally:
        rwc.LIVE_SAVE, rwc.LAST_ACTION, rwc.BACKUP_DIR = orig_live, orig_last, orig_bdir
finally:
    shutil.rmtree(tmp, ignore_errors=True)

print("")
if FAILS:
    print(f"❌ {len(FAILS)} check(s) failed: {FAILS}")
    sys.exit(1)
print("✅ all checks passed")
