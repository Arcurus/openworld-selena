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
its narrative arc is one of taint. Use it to track how far
a character, location, or artifact has slipped from its
original nature.

Corrupted entities are typically **very selfish** — their
actions prioritise their own gain or pleasure at the
expense of others, and they often **spread corruption
further in the world**, whether consciously (a villain
who deliberately poisons the minds of followers) or
unconsciously (a curse-bearer whose mere presence
withers what they touch). A corrupted being corrupts
by being what it is.

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

`influence` is **distinct from `power`** (which is the
entity's general power, considering all of its strengths —
military, magical, economic, political, anything that
gives the entity weight in the world) and **from `reputation`**
(which is how the world SEES the entity). A queen can
have very high `influence` (lineage, network, veto power)
while her `power` is moderate; a back-room dealer can
have high `influence` even if `reputation` is low. A
high-`influence` entity is someone whose words and
gestures move other actors, whether or not those actors
like them.

Use it to model "soft power" — diplomacy, negotiation,
recruitment, vote outcomes, who gets a seat at the table.

## `suspicion` (story signal)

A high `suspicion` means this entity is *suspected of
wrongdoing* — the world is watching them closely. They might
be followed, investigated, or refused service. A negative
`suspicion` means the entity is *seen as above reproach* —
a trusted hero, a public official, a beloved community
member.

`suspicion` is **perception, not reality** — distinct from
`corruption`, which is actual evil. A pure-hearted reformer
can carry high `suspicion` if the public doesn't trust their
motives; a guilty party can carry low `suspicion` if they've
hidden their tracks well.

`suspicion` is also a property the LLM can deliberately
**shape** based on the entity's behaviour, not just a
passive reflection of what they are. Two patterns worth
keeping in mind:

- **Corrupted political beings or heroes** typically
  *hide* their nature — bribes, alibis, public-image
  work. Their `suspicion` should usually move **down**
  (or stay low) even as their `corruption` rises.
  They're working to prevent the world from seeing
  what they are.
- **Powerful beings who don't care what others think**
  (a tyrant, a self-righteous god, a demon in open
  defiance) act openly suspicious and their
  `suspicion` can legitimately **soar** without it
  being a discovery — it's a declaration, not a slip.

## Common generic properties

These appear in many entities and are worth recognising by
name.

- `power` — how powerful this entity is, not only physical
  strength — also magical, political, economic, anything
  that gives the entity weight in the world. Required for
  every entity.
- `wealth` — money, treasure, material resources. Applies
  broadly: factions, merchants, kingdoms, villages,
  individual characters — anyone with a treasury or a
  purse. Merchants and certain guilds care a great deal
  about it; their power and survival often ride on it.
- `morale` — fighting spirit, hope, determination. Can go
  negative (despair) or very positive (eager, fanatical).
- `knowledge` — accumulated lore, secrets learned, lost
  truths recovered.
- `reputation` — how the world sees this entity. Distinct
  from `visibility`: `visibility` is a present-tense "are
  they here" signal, `reputation` is a longer-term
  "what do people think of them" signal.

