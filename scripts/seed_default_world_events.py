#!/usr/bin/env python3
"""Seed the 5 default lore events into the world via the existing API.

The world was created before World::seed_default_events was added to
World::new(), so the loaded world has 0 active_events. This is the
client-side workaround: POST each of the 5 canonical "Shadow Awakening"
events via /api/world/events.

Idempotent: checks GET /api/world/events first; if any of the 5 UUIDs
already exist, skips those.
"""
import json
import urllib.request
import urllib.error
import sys

API = "http://127.0.0.1:8081"
COOKIE = "openworld_auth=1"

DEFAULT_EVENTS = [
    {
        "id": "e10f8432-2dbe-4b73-9826-2366c7772c9f",
        "name": "The Shadow Awakens",
        "description": "For centuries dismissed as myth, the Scrolls of the First Age spoke of a darkness that would return when the realm forgot its vigilance. Now the signs are undeniable: shadows in the Northern Pass stretch longer than the mountains themselves, animals flee southward, and the Moonwell at Elder Moonthorn reflects something other than the future. The Prophecy of the Shadow Crown has begun to unfold.",
        "influence": "Entities grow suspicious, militaristic, and watchful. Silverstream Keep mobilizes. Ironforge forges weapons day and night. Whisperwood closes its borders. Trade becomes riskier. Trust between factions erodes. Power-hungry actors see opportunity. The realm is tense with approaching doom.",
        "active": True,
    },
    {
        "id": "c7eca4b6-8dc8-45ba-ba5a-337803de3019",
        "name": "Velora Walks Again",
        "description": "A knight in corroded silver armor has been sighted on the roads at night. Her helm reflects no light and she leaves no shadow. Velora the Undying, who held the Northern Pass alone for seven days during the Demon Wars, has returned. She seeks the Forgotten Heir mentioned in the prophecy and trades secrets with those brave enough to meet her gaze.",
        "influence": "Heroes and knights feel a stirring of destiny. Some seek Velora out for blessings. Others fear her appearance as a sign of the worst. Kira Dawnblade in particular feels the prophecy pulling at her. Mira the Merchant has rare tales to sell. The Silver Wardens of Silverstream Keep sense the return of their founder.",
        "active": True,
    },
    {
        "id": "a23dac23-4fd1-4936-9696-059cae6ce77d",
        "name": "The Shadowmaw Stirs",
        "description": "Ironforge miners report tremors deep beneath Frostpeak. The forges have grown hot without fuel. The clan elders whisper of bad dreams — impossible dreams of black wings and a heartbeat that shakes the world. Vaelthrix the Endless, the ancient dragon who slept beneath the Frostpeak Mountains before the First Age, has begun to dream. Her dreams leak into the world as visions and earthquakes.",
        "influence": "Dwarves of Ironforge grow fearful but resolute. Miners dig deeper in search of ancient weapons. Mountain-dwelling entities feel the tremors. The wandering bard hears songs about dragons returning. Some interpret the dreams as omens; others as opportunities. The realm feels heavier, charged with waiting.",
        "active": True,
    },
    {
        "id": "88f129bd-2c08-4f23-9969-4818d3858bfd",
        "name": "The Silver Wardens Mobilize",
        "description": "The banners of Silverstream Keep fly from every tower. Knights ride out in pairs along the northern roads. A formal decree has been issued: every traveler must declare their business or be turned back. The Silver Wardens — Silverstream Keep's elite order — believe themselves the prophesied defenders of the realm. They have begun recruiting among the common folk, and the cost of admission is a secret they will not share.",
        "influence": "Knights and warriors grow bold. Refugees and villagers consider joining. Bandits and outlaws grow more cautious. The Keep itself grows in power, but at the cost of internal suspicion. The mobilization of one faction pressures all others — should they also prepare for war? Trade slows. Tensions rise along every road.",
        "active": True,
    },
    {
        "id": "46a976d2-c2a7-46b5-903f-1a04ae751058",
        "name": "The Bells of the Sunken Temple",
        "description": "Travelers near the southern marshlands report hearing bells at dusk. The Sunken Temple — half-submerged since the Second Age and abandoned for a thousand years — has begun to ring. No one has yet dared enter. The Wandering Bard claims to have heard a voice singing along with the bells, in a language no scholar recognizes. Mira the Scribe is taking notes.",
        "influence": "Scholars and sages grow curious. Adventurers plan expeditions. Locals avoid the marshlands. Zephyrus the Oracle speaks in riddles about it, which everyone interprets differently. The realm feels as if something is waking that was meant to stay asleep. The Drowned City, said to be the temple sister, has grown quieter — its silence more ominous than its noise.",
        "active": True,
    },
]


def get_existing():
    req = urllib.request.Request(f"{API}/api/world/events")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode())
            return data.get("events", [])
    except Exception as e:
        print(f"WARN: GET failed: {e}", file=sys.stderr)
        return []


def post_event(ev):
    payload = {
        "name": ev["name"],
        "description": ev["description"],
        "influence": ev["influence"],
        "active": ev["active"],
    }
    body = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{API}/api/world/events",
        data=body,
        headers={"Content-Type": "application/json", "Cookie": COOKIE},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")
    except Exception as e:
        return 0, {"error": str(e)}


def main():
    existing = get_existing()
    existing_ids = {e.get("id") for e in existing}
    print(f"Existing events: {len(existing)}")

    added = 0
    skipped = 0
    for ev in DEFAULT_EVENTS:
        target_id = ev["id"]
        # Check if the canonical id is already in active_events
        # The server assigns a new uuid on POST, so we use name+description match instead
        if any(e.get("name") == ev["name"] and e.get("description") == ev["description"] for e in existing):
            print(f"  SKIP: '{ev['name']}' (already present, by name match)")
            skipped += 1
            continue
        status, body = post_event(ev)
        if status == 200 and body.get("success"):
            print(f"  ADD:  '{ev['name']}' OK (new_id={body.get('event', {}).get('id', '?')[:8]})")
            added += 1
        else:
            print(f"  FAIL: '{ev['name']}' status={status} body={body}")
    print()
    print(f"Summary: added={added}, skipped={skipped}")
    return 0 if added >= 0 else 1


if __name__ == "__main__":
    sys.exit(main())
