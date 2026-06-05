#!/usr/bin/env python3
"""
Replace part of an entity's history_summary via the OW API.

Per Arcurus 2026-06-04 (#openworld): thin CLI wrapper around
POST /api/entities/:id/history-summary/replace. The HTTP
endpoint is single-replace (one old_part + new_part per call);
the LLM-emit `history_summary_replace` command (separate
code path in process_action_handler) is the only way to do
multi-replace in one go.

Conventions (mirror the API exactly):
  - old_part is REQUIRED to be non-empty UNLESS new_part is
    also empty (then the whole call is a no-op).
  - old_part="!ALL!" + non-empty new_part = FULL REPLACE: discard
    the current summary, set it to new_part. Use this when a full
    restructure is needed. (Added 2026-06-05 per Arcurus #openworld.)
  - old_part="" + non-empty new_part = APPEND new_part to end
    of current summary (no warning logged).
  - non-empty old_part not found in current summary = WARNING
    response, no change made. Pass --strict to make it 404
    instead (not_found_is_error=true on the API).
  - Result is truncated to the entity's max cap (10000 chars
    by default) with a warning if it goes over.

Auth: the server accepts Cookie: openworld_auth=1 for any
request that hits a protected endpoint (the cookie is the
session marker; the WEB_PASSWORD is only checked at /api/login
to mint the cookie). This script assumes the server is
running locally; for remote use, you'd need a real session
cookie or a token-based flow.

Usage:
  # Find-replace (most common case)
  python3 scripts/replace_history_summary.py <entity_id> --old "old text" --new "new text"

  # Full replace (discard current, set to new)
  python3 scripts/replace_history_summary.py <entity_id> --old "!ALL!" --new "<complete new summary>"

  # Append to end of current summary
  python3 scripts/replace_history_summary.py <entity_id> --old "" --new " (appended note)"

  # Strict mode (404 instead of warning on not-found)
  python3 scripts/replace_history_summary.py <entity_id> --old "X" --new "Y" --strict

  # Custom server URL
  python3 scripts/replace_history_summary.py <entity_id> --old "X" --new "Y" --url http://localhost:9000
"""

import argparse
import json
import sys
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(
        description="Replace part of an entity's history_summary via the OW API.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("Usage:")[1] if "Usage:" in __doc__ else "",
    )
    parser.add_argument("entity_id", help="UUID of the entity to update")
    parser.add_argument(
        "--old",
        required=True,
        help="Text to find in the current summary. Use empty string to APPEND new_part to the end.",
    )
    parser.add_argument(
        "--new",
        default="",
        help="Text to replace old_part with. Default empty (i.e. delete old_part).",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="If old_part is not found, return 404 instead of 200+warning. Maps to not_found_is_error=true on the API.",
    )
    parser.add_argument(
        "--url",
        default="http://localhost:8081",
        help="Base URL of the OW server. Default: http://localhost:8081",
    )
    args = parser.parse_args()

    endpoint = f"{args.url.rstrip('/')}/api/entities/{args.entity_id}/history-summary/replace"
    payload = {
        "old_part": args.old,
        "new_part": args.new,
        "not_found_is_error": args.strict,
    }

    req = urllib.request.Request(
        endpoint,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "Cookie": "openworld_auth=1",
        },
        method="POST",
    )

    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            status = resp.status
            body = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        # 4xx/5xx — body is JSON with {error, success:false}
        try:
            body = json.loads(e.read().decode("utf-8"))
        except Exception:
            body = {"error": str(e), "success": False}
        status = e.code
    except urllib.error.URLError as e:
        print(f"❌ Connection failed: {e.reason}", file=sys.stderr)
        print(f"   Is the OW server running at {args.url}?", file=sys.stderr)
        sys.exit(2)
    except Exception as e:
        print(f"❌ Unexpected error: {e}", file=sys.stderr)
        sys.exit(2)

    # Pretty-print the result.
    if status == 200 and body.get("success"):
        summary = body.get("history_summary", "")
        chars = body.get("history_summary_chars", 0)
        truncated = body.get("truncated", False)
        warning = body.get("warning")
        max_chars = body.get("max_chars", "?")
        max_source = body.get("max_chars_source", "?")

        print(f"✓ Replaced.")
        print(f"  length:  {chars} / {max_chars} chars ({max_source} cap)")
        if truncated:
            print(f"  ⚠️  truncated: {warning or 'exceeded cap'}")
        elif warning:
            print(f"  ⚠️  {warning}")
        if summary:
            # Show first/last 80 chars to give a quick visual.
            head = summary[:80].replace("\n", " ")
            tail = summary[-80:].replace("\n", " ") if len(summary) > 160 else ""
            if tail and head != tail:
                print(f"  summary: {head} ... {tail}")
            else:
                print(f"  summary: {head}")
        sys.exit(0)
    else:
        # Non-200 or success=false. Show the error.
        err = body.get("error", f"HTTP {status}")
        print(f"❌ Failed (HTTP {status}): {err}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
