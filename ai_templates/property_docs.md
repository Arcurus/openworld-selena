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

## `visibility` (story signal only)

A high `visibility` means this entity is *exposed* — it stands
out, its presence is felt, and other entities will be very
aware of it.

A negative `visibility` means the entity is *hiding* —
withdrawn, concealed, easy to overlook. Other entities will be
less aware of it.

Use it as a narrative cue: how present or absent does this
entity feel in the world right now?

The post-effect **hidden-tag rule** auto-toggles a `hidden`
tag on entities whose `(max(10, power) / 10 + visibility)`
threshold falls below 0 (deep hider) and removes it when the
threshold rises to 1 or above. A small dead zone `[0, 1)`
prevents flicker near the boundary. See
[`docs/world-mechanics.md` §4](../../docs/world-mechanics.md#4-the-visibility-property)
for the full mechanics.

## `corruption` (story signal only)

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

The post-effect **corrupted-tag rule** auto-toggles a
`corrupted` tag on entities whose `(max(1, power) - corruption)`
threshold falls below 0 (corruption has overtaken the entity's
own strength) and removes it when the threshold rises to 1 or
above. A small dead zone `[0, 1)` prevents flicker near the
boundary.

Why this formula: low-power entities are easy to corrupt (a
power-10 entity with `corruption: 15` → threshold = `10 - 15 =
-5` → tagged `corrupted`), while high-power entities are hard
to corrupt (a power-100 entity needs `corruption > 100` before
the tag appears). Mirrors the hidden-tag rule's "famous dragon
is still scary" intuition — a legendary evil entity resists
corruption's pull on its identity, while a freshly-minted
cultist can fall to it in a single bad turn.

## Common generic properties (no special tag rule)

These appear in many entities and are worth recognising by
name; they have no auto-tag rule.

- `power` — entity's overall strength tier; the cap for many
  other mechanics (effect normalization, stats cap, etc.).
  Almost universal.
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

## Writing effects for these properties

In your `effects` block, use the dot-prefix form for both
self-effects and cross-entity effects. Server-side per-target
safety nets still apply: magnitude cap (no single delta > 1e6),
per-target normalization (each target's effect budget is keyed
on its own `power`, with a +10 max-amount baseline), and the
system-entity guard (the World Clock and anything tagged `meta`
cannot be written to). Negative values are fine (corruption,
visibility, morale) — they're signed i64.

```json
"effects": {
  "self.visibility": -3,
  "self.corruption": 4,
  "Mira the Merchant.wealth": -5,
  "The Sunken Temple.phenomenon_count": 1
}
```
