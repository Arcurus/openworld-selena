# Entity Property Reference for the LLM

This file documents the **named** integer properties (the ones
that carry a special meaning the LLM should understand) so the
narrator can write meaningful effects. Generic per-entity stats
(whatever the entity happens to have — `mana_reserves` for a
mage, `army_size` for a faction, `phenomenon_count` for a
haunted location, etc.) are surfaced as raw values in
`{property_context}`; their meaning is entity-specific and the
LLM is expected to read it from the entity's name, description,
and tags.

This file is **loaded into every action prompt** at the
`{property_docs}` placeholder, right after the
`{property_context}` block (which shows the actual values).
If you rename a property or change its semantics, update this
file too — the LLM learns the schema here.

## `visibility` (story signal)

A high `visibility` means this entity is *exposed* — it stands
out, its presence is felt, and other entities will be very
aware of it.

A negative `visibility` means the entity is *hiding* —
withdrawn, concealed, easy to overlook. Other entities will be
less aware of it.

Use it as a narrative cue: how present or absent does this
entity feel in the world right now?

## `corruption` (story signal)

A high `corruption` means this entity is *corrupted* — its
essence has been twisted, its actions serve darker ends, and
its narrative arc is one of taint or fall. Use it to track
how far a character, location, or artifact has slipped from
its original nature.

A negative `corruption` means the entity is *purified* —
cleansed, sanctified, or otherwise resistant to corruption.
Use it to model a redeemed villain, a holy site, or a relic
that has been ritually restored.

Neutral is `0` — the entity is neither corrupted nor purified.

## `influence` (story signal)

A high `influence` means this entity has *political and
social leverage* — the ability to sway decisions, broker
deals, or rally others. Think back-room dealers, queens, guild
masters, and respected elders.

`influence` is **distinct from `power`** (which is closer to
military / combat strength) and **from `reputation`** (which
is how the world SEES the entity). A queen can have very
high `influence` while being personally weak; a back-room
dealer can have high `influence` even if nobody respects them.
A high-`influence` entity is someone whose words and gestures
move other actors, whether or not those actors like them.

Use it to model "soft power" — diplomacy, negotiation,
recruitment, vote outcomes, who gets a seat at the table.

## `suspicion` (story signal)

A high `suspicion` means this entity is *suspected of
wrongdoing* — the world is watching them closely. They might
be followed, investigated, or refused service. A negative
`suspicion` means the entity is *seen as above reproach* —
a trusted hero, a public official, a beloved community
member.

` suspicion` is **perception, not reality** — distinct from
`corruption`, which is actual evil. A pure-hearted reformer
can carry high `suspicion` if the public doesn't trust their
motives; a guilty party can carry low `suspicion` if they've
hidden their tracks well.

The server enforces a tag rule on this property: when
`max(1, power) - suspicion < -1` the entity gets the
`suspicious` tag (NPCs treat them with caution, gates close
to them, etc.). The tag is removed when the gap is positive
again. The dead zone of `[-1, 0]` prevents flicker right at
the boundary. A high-`power` entity needs *more* suspicion
to be tagged (a power-100 entity needs `suspicion > 101`;
a power-10 entity only needs `suspicion > 11`).

## Common generic properties

These appear in many entities and are worth recognising by
name.

- `power` — entity's overall strength tier; the cap for many
  other mechanics. Almost universal.
- `wealth` — money, treasure, material resources. Used by
  factions, merchants, kingdoms.
- `morale` — fighting spirit, hope, determination. Can go
  negative (despair) or very positive (eager, fanatical).
- `knowledge` — accumulated lore, secrets learned, lost
  truths recovered.
- `reputation` — how the world sees this entity. Distinct
  from `visibility`: `visibility` is a present-tense "are
  they here" signal, `reputation` is a longer-term
  "what do people think of them" signal.

## Writing effects

In your `effects` block, use the dot-prefix form for both
self-effects and cross-entity effects. Server-side per-target
safety nets still apply: magnitude cap, per-target
normalization, and the system-entity guard (the World Clock
and anything tagged `meta` cannot be written to). Negative
values are fine (corruption, visibility, morale) — they're
signed integers.

```json
"effects": {
  "self.visibility": -3,
  "self.corruption": 4,
  "Mira the Merchant.wealth": -5,
  "The Sunken Temple.phenomenon_count": 1
}
```
