#!/usr/bin/env python3
"""restore_world_cast.py — restore the open-world cast from a verified snapshot.

WHY THIS TOOL EXISTS
---------------------
Todo ``f75999bc`` (open-world-selena): the live world lost its 49 entities on
2026-09-07 and has since run with a single entity (the World Clock). The
automated lane has therefore had NO eligible actor for 7 weeks. The recovery
path is documented in the todo's ``long_desc``, but until now it was a
*procedure written in prose* — six manual steps an operator had to run by
hand, with no verification and no rollback.

This tool is that procedure, made executable and safe:

  1. PREFLIGHT  — the candidate snapshot exists, is readable, and is
     fingerprinted (sha256 + size).
  2. VERIFY    — the candidate is loaded in an ISOLATED scratch instance
     (separate cwd, separate port, scheduler disabled, no LLM calls) and the
     entity count is read back from the API. A snapshot that does not load
     is rejected BEFORE the live world is touched.
  3. ARCHIVE   — the live ``world_data/save.owbl`` and the scheduler's
     ``ow_entity_last_action.json`` are copied to ``world_data/backups/``
     with a timestamp. Never overwritten.
  4. SWAP      — the service is stopped, the candidate replaces
     ``world_data/save.owbl``, the service is started again.
  5. CONFIRM   — the live API is polled until it answers; the live entity
     count must equal the verified count. If it does not, the tool ROLLS BACK
     (restores the archive, restarts the service) and exits non-zero.
  6. REPORT    — the measured before/after numbers are printed.

SAFETY MODEL
------------
* **Dry-run by default.** Nothing is written, stopped or copied unless
  ``--apply`` is passed.
* ``--apply`` additionally requires ``--yes=<TOKEN>`` with the exact token
  ``RESTORE-WORLD-CAST``. A bare ``--apply`` is refused.
* The live world is only ever replaced by a snapshot that has already been
  proven to load, and the previous save is always archived first — so the
  swap is reversible by hand even if the tool dies mid-flight.
* The tool NEVER deletes a save file. Archives are written with fresh
  timestamps.
* No LLM calls: the isolated verification instance runs with the scheduler
  disabled, and the live instance is only stopped/restarted (never asked to
  act).

Usage
-----
    python3 code/restore_world_cast.py                       # dry-run: verify + plan
    python3 code/restore_world_cast.py --verify-only         # just the isolated load test
    python3 code/restore_world_cast.py --candidate=PATH      # use a different snapshot
    python3 code/restore_world_cast.py --apply --yes=RESTORE-WORLD-CAST

Exit codes
----------
    0 = success (or a clean dry-run)
    1 = preflight failed (candidate missing / unreadable)
    2 = runtime error (service control, API unreachable, …)
    3 = verification failed — live count != verified count (ROLLED BACK)
    4 = refused (missing/invalid --yes token)

Requires: python3, the built binary at ``target/release/open_world``,
``systemctl --user`` for service control, and the ``open-world-selena``
user service.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.dirname(HERE)

DEFAULT_CANDIDATE = os.path.join(
    PROJECT_ROOT, "world_data", "backups", "save-manual-20260607-212246.owbl"
)
LIVE_SAVE = os.path.join(PROJECT_ROOT, "world_data", "save.owbl")
LAST_ACTION = os.path.join(PROJECT_ROOT, "world_data", "ow_entity_last_action.json")
BACKUP_DIR = os.path.join(PROJECT_ROOT, "world_data", "backups")
BINARY = os.path.join(PROJECT_ROOT, "target", "release", "open_world")
SETTINGS = os.path.join(PROJECT_ROOT, "settings.json")
ENV_FILE = os.path.join(PROJECT_ROOT, ".env")

SERVICE = "open-world-selena.service"
LIVE_API = "http://127.0.0.1:8081"

CONFIRM_TOKEN = "RESTORE-WORLD-CAST"

# A restored world must have at least this many entities, else something is
# badly wrong (the 2026-09-07 failure left 1). Refuse to swap for a snapshot
# that cannot clear the bar.
MIN_PLAUSIBLE_ENTITIES = 2


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------
def _now_iso() -> str:
    return _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")


def _stamp() -> str:
    return _dt.datetime.now().strftime("%Y%m%dT%H%M%S")


def sha256_of(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def free_port() -> int:
    """Ask the OS for an unused localhost port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def systemctl(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["systemctl", "--user", *args],
        capture_output=True, text=True, timeout=120,
    )


def fetch_entity_count(api_url: str, timeout: int = 10) -> Tuple[int, List[Dict[str, Any]]]:
    """Return (count, entities) from GET /api/entities?limit=1000."""
    url = f"{api_url.rstrip('/')}/api/entities?limit=1000"
    with urllib.request.urlopen(url, timeout=timeout) as r:
        data = json.load(r)
    if isinstance(data, list):
        ents = data
        count = len(ents)
    else:
        ents = data.get("data", data.get("entities", []))
        count = data.get("count", len(ents))
    return int(count), list(ents)


def is_system_entity(e: Dict[str, Any]) -> bool:
    """Mirror of the server's WorldEntity::is_system_entity()."""
    if (e.get("entity_type") or "").lower() in ("world_clock", "abstract"):
        return True
    tags = e.get("tags") or []
    if isinstance(tags, str):
        tags = [tags]
    return "meta" in [str(t).lower() for t in tags]


# ---------------------------------------------------------------------------
# Step 2 — isolated verification (load the candidate in a scratch instance)
# ---------------------------------------------------------------------------
def verify_candidate_offline(candidate: str,
                             timeout_s: int = 90) -> Dict[str, Any]:
    """Load the candidate in an isolated scratch instance and read back its
    entity count. Does NOT touch the live world.

    The scratch instance gets:
      * its own cwd (so ``world_data/save.owbl`` is the candidate),
      * its own port,
      * ``world_data/ow_scheduler_config.json`` with ``enabled: false``
        (no LLM calls, no actions),
      * a symlink to the project's ``.env`` (the server refuses to start
        without one — it creates ``.env`` on first run and fails if the
        path is not writable).
    """
    if not os.path.isfile(BINARY):
        return {"ok": False, "error": f"binary not found: {BINARY}"}
    if not os.path.isfile(ENV_FILE):
        return {"ok": False, "error": f".env not found: {ENV_FILE}"}

    scratch = tempfile.mkdtemp(prefix="ow-restore-verify-")
    port = free_port()
    proc: Optional[subprocess.Popen] = None
    try:
        os.makedirs(os.path.join(scratch, "world_data"), exist_ok=True)
        shutil.copyfile(candidate, os.path.join(scratch, "world_data", "save.owbl"))
        with open(os.path.join(scratch, "world_data", "ow_scheduler_config.json"), "w") as fh:
            json.dump({"enabled": False, "interval_seconds": 600,
                       "actions_per_cycle": 1}, fh)
        # settings.json with a different port
        with open(SETTINGS) as fh:
            settings = json.load(fh)
        settings.setdefault("server", {})["port"] = port
        with open(os.path.join(scratch, "settings.json"), "w") as fh:
            json.dump(settings, fh, indent=2)
        # .env as a symlink (no secret copy leaves the project dir)
        os.symlink(ENV_FILE, os.path.join(scratch, ".env"))

        log_path = os.path.join(scratch, "server.log")
        with open(log_path, "wb") as logf:
            proc = subprocess.Popen(
                [BINARY], cwd=scratch, stdout=logf, stderr=subprocess.STDOUT,
                env={**os.environ, "OW_LLM_PROJECT": "restore-verify"},
            )

        api = f"http://127.0.0.1:{port}"
        deadline = time.time() + timeout_s
        last_err = ""
        while time.time() < deadline:
            if proc.poll() is not None:
                tail = _tail(log_path, 15)
                return {"ok": False, "error": f"instance exited early:\n{tail}"}
            try:
                count, ents = fetch_entity_count(api, timeout=5)
                return {
                    "ok": True,
                    "entity_count": count,
                    "non_system_count": sum(1 for e in ents if not is_system_entity(e)),
                    "port": port,
                    "sample_names": [e.get("name") for e in ents[:8]],
                }
            except Exception as exc:  # noqa: BLE001 - any failure = not up yet
                last_err = str(exc)
                time.sleep(1.0)
        return {"ok": False, "error": f"isolated instance did not answer: {last_err}"}
    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=15)
            except Exception:  # noqa: BLE001
                proc.kill()
        shutil.rmtree(scratch, ignore_errors=True)


def _tail(path: str, n: int = 20) -> str:
    try:
        with open(path, "r", errors="ignore") as fh:
            return "".join(fh.readlines()[-n:])
    except Exception:  # noqa: BLE001
        return "(no log)"


# ---------------------------------------------------------------------------
# Steps 3-5 — archive, swap, confirm
# ---------------------------------------------------------------------------
def archive_live_world() -> Dict[str, str]:
    """Copy the live save + scheduler idle-map into world_data/backups/."""
    os.makedirs(BACKUP_DIR, exist_ok=True)
    out: Dict[str, str] = {}
    stamp = _stamp()
    if os.path.isfile(LIVE_SAVE):
        dest = os.path.join(BACKUP_DIR, f"save-pre-restore-{stamp}.owbl")
        shutil.copyfile(LIVE_SAVE, dest)
        out["save_archive"] = dest
    if os.path.isfile(LAST_ACTION):
        dest = os.path.join(BACKUP_DIR, f"ow_entity_last_action-pre-restore-{stamp}.json")
        shutil.copyfile(LAST_ACTION, dest)
        out["last_action_archive"] = dest
    return out


def swap_in_candidate(candidate: str, expected_count: int) -> Dict[str, Any]:
    """Stop the service, install the candidate, restart, and verify.

    On any failure the archived save is put back and the service restarted.
    """
    archives = archive_live_world()
    saved_archive = archives.get("save_archive")

    stop = systemctl("stop", SERVICE)
    if stop.returncode != 0:
        systemctl("start", SERVICE)
        return {"ok": False, "stage": "stop",
                "error": f"systemctl stop failed: {stop.stderr.strip()}"}

    try:
        shutil.copyfile(candidate, LIVE_SAVE)
    except Exception as exc:  # noqa: BLE001
        _rollback(saved_archive)
        return {"ok": False, "stage": "copy", "error": str(exc)}

    start = systemctl("start", SERVICE)
    if start.returncode != 0:
        _rollback(saved_archive)
        return {"ok": False, "stage": "start",
                "error": f"systemctl start failed: {start.stderr.strip()}"}

    # Poll the live API until it answers, then compare the count.
    deadline = time.time() + 120
    live_count = None
    last_err = ""
    while time.time() < deadline:
        try:
            live_count, _ = fetch_entity_count(LIVE_API, timeout=5)
            break
        except Exception as exc:  # noqa: BLE001
            last_err = str(exc)
            time.sleep(2.0)
    if live_count is None:
        _rollback(saved_archive)
        return {"ok": False, "stage": "confirm",
                "error": f"live API never answered after restart: {last_err}"}

    if live_count != expected_count:
        _rollback(saved_archive)
        return {"ok": False, "stage": "confirm",
                "error": f"live entity count {live_count} != verified count {expected_count}",
                "live_count": live_count, "expected_count": expected_count,
                "rolled_back": True}

    return {"ok": True, "live_count": live_count, "archives": archives}


def _rollback(saved_archive: Optional[str]) -> None:
    """Put the archived save back and bring the service up."""
    if saved_archive and os.path.isfile(saved_archive):
        try:
            shutil.copyfile(saved_archive, LIVE_SAVE)
        except Exception:  # noqa: BLE001
            pass
    systemctl("start", SERVICE)


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------
def build_report(verify: Dict[str, Any], candidate: str,
                 live_before: Optional[int], plan: str) -> str:
    lines = [f"🛠️  **open-world cast restore** — {_now_iso()}", ""]
    lines.append(f"• candidate: `{os.path.relpath(candidate, PROJECT_ROOT)}`")
    if verify.get("ok"):
        lines.append(f"• verified in an isolated instance: **{verify['entity_count']} entities** "
                     f"({verify.get('non_system_count')} non-system)")
        lines.append(f"• live entity count now: **{live_before if live_before is not None else '?'}**")
    else:
        lines.append(f"• verification FAILED: `{verify.get('error')}`")
    lines.append("")
    lines.append(plan)
    return "\n".join(lines)


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--candidate", default=DEFAULT_CANDIDATE,
                    help="snapshot to restore (default: the verified 2026-06-07 save)")
    ap.add_argument("--verify-only", action="store_true",
                    help="run only the isolated load test; never touch the live world")
    ap.add_argument("--apply", action="store_true",
                    help="actually swap the world (requires --yes)")
    ap.add_argument("--yes", default="",
                    help=f"must be exactly {CONFIRM_TOKEN} to apply")
    ap.add_argument("--json", action="store_true", help="machine-readable output")
    args = ap.parse_args(argv)

    # ---- Preflight -------------------------------------------------------
    candidate = os.path.abspath(args.candidate)
    preflight: Dict[str, Any] = {"candidate": candidate}
    if not os.path.isfile(candidate):
        preflight["ok"] = False
        preflight["error"] = f"candidate snapshot not found: {candidate}"
        print(json.dumps(preflight, indent=2) if args.json else f"❌ {preflight['error']}")
        return 1
    preflight["ok"] = True
    preflight["size_bytes"] = os.path.getsize(candidate)
    preflight["sha256"] = sha256_of(candidate)
    preflight["mtime"] = _dt.datetime.fromtimestamp(
        os.path.getmtime(candidate)).isoformat(timespec="seconds")

    live_before = None
    try:
        live_before, _ = fetch_entity_count(LIVE_API, timeout=5)
    except Exception:  # noqa: BLE001
        live_before = None

    # ---- Verify ----------------------------------------------------------
    verify = verify_candidate_offline(candidate)
    if not verify.get("ok") or verify.get("entity_count", 0) < MIN_PLAUSIBLE_ENTITIES:
        if args.json:
            print(json.dumps({"preflight": preflight, "verify": verify,
                              "live_before": live_before}, indent=2))
        else:
            print(f"❌ candidate verification FAILED: {verify.get('error')}")
            print("   → the live world was NOT touched.")
        return 1

    # ---- Plan ------------------------------------------------------------
    plan_lines = [
        "**plan** (dry-run — nothing has been changed):",
        f"1. archive live save → `world_data/backups/save-pre-restore-<TS>.owbl`",
        f"2. stop `{SERVICE}`, copy the candidate over `world_data/save.owbl`",
        f"3. start `{SERVICE}`, wait for `GET /api/entities`",
        f"4. confirm live count == {verify['entity_count']} "
        f"(else restore the archive and roll back)",
    ]
    if args.apply:
        plan_lines.append("")
        plan_lines.append("**applying now** …")

    if not args.apply:
        if args.json:
            print(json.dumps({"preflight": preflight, "verify": verify,
                              "live_before": live_before, "applied": False}, indent=2))
        else:
            print(build_report(verify, candidate, live_before, "\n".join(plan_lines)))
            print("")
            print("dry run only — pass `--apply --yes=%s` to execute." % CONFIRM_TOKEN)
        return 0

    # ---- Apply -----------------------------------------------------------
    if args.yes != CONFIRM_TOKEN:
        print(f"❌ refused: --apply requires --yes={CONFIRM_TOKEN} "
              f"(got {args.yes!r}). Nothing was changed.")
        return 4

    result = swap_in_candidate(candidate, verify["entity_count"])
    if args.json:
        print(json.dumps({"preflight": preflight, "verify": verify,
                          "live_before": live_before, "result": result,
                          "applied": bool(result.get("ok"))}, indent=2))
    else:
        print(build_report(verify, candidate, live_before, "\n".join(plan_lines)))
        print("")
        if result.get("ok"):
            print(f"✅ restored: live world now has **{result['live_count']} entities**")
            print(f"   archive: `{result['archives'].get('save_archive')}`")
        else:
            print(f"❌ restore failed at stage `{result.get('stage')}`: {result.get('error')}")
            if result.get("rolled_back"):
                print("   → rolled back to the archived save; the service is running again.")
    return 0 if result.get("ok") else 3


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
