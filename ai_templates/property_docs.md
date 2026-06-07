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
