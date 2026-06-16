# Lore Fields — Data Schema for `docs/world_lore.md`

*Introduced 2026-06-15 per Arcurus #openworld: "Lore-as-data integration
sounds interesting. you can use our entities ability to add named string
fields. then you can fill them."*

Lore is data, not prose. Every entity in the world can carry lore content
in its `properties_string` map under well-known keys. The lore MD is
**auto-generated** from these fields by
[`scripts/generate_lore_md.py`](../scripts/generate_lore_md.py). To update
the lore, edit the entity's `properties_string` and re-run the generator.

## The 4 lore fields

All fields are **optional**. An entity may have any subset, in any order.
Missing fields are simply omitted from the rendered MD.

### `lore_summary`

**Type:** 1–2 sentences.
**Applies to:** all entities.
**Rendered as:** the bold tagline under the entity's heading.

A *short* evocative phrase. The kind of thing you'd see on the back of a
trading card or a chapter heading. Not a complete sentence if you can
avoid it — should pull-quote cleanly.

> *Examples:*
> - "A knight in corroded silver armor, whose shadow died three centuries ago."
> - "A sunken city whose topmost spires still break the surface at low tide."
> - "The dwarven forges that woke a dragon, racing now to put it back to sleep."

### `lore_description`

**Type:** 1–3 paragraphs. Markdown allowed.
**Applies to:** all entities.
**Rendered as:** the main body text under the summary.

The full narrative description. The voice should match the existing
Aethermoor / Realm of Shadows tone: evocative, mysterious, sometimes
unreliable. Use the entity's own details (`description`, `properties_*`,
`history`) to anchor the prose, then layer in mystery and connections to
other entities. **Cross-reference other entities by name** (the auto-
generator turns these into entity links in the MD).

> *Includes:*
> - Sensory / atmospheric details for locations
> - History and origin for factions
> - Personality and motivation for characters
> - Powers and known wearers for artifacts
> - Waking state and known sightings for the dragon

### `lore_relationships`

**Type:** 1–2 sentences / short list. Markdown bullets allowed.
**Applies to:** all entities (most useful for factions + characters).
**Rendered as:** a "Relationships" sub-section.

List the key relationships the entity has — to other entities, to
factions, to locations. Use names that exist in the live world (the auto-
generator will cross-link). Format can be a one-liner or a bulleted list.

> *Examples:*
> - "Allied with Silverstream Keep. Rival of The Crimson Vein. Built the
>   Great Forge above a sleeping dragon it was not supposed to wake."
> - "Zephyrus the Oracle · Elder Moonthorn · The Whispering Roots ·
>   The Silverleaf Sanctuary"
> - "Worn by King Valdris the First. The current bearer is unknown."

### `lore_secrets`

**Type:** 1 paragraph.
**Applies to:** all entities (most useful for factions + characters +
dragon + artifact).
**Rendered as:** a "Secrets" sub-section (or a "Dangers" sub-section for
locations).

What does this entity *hide*, *know*, or *fear*? This is the LLM-hook
section — the part that gives the entity generator material for emergent
narrative. Should be **specific and concrete**, not generic. The more
names and connections, the better.

> *Examples:*
> - "Their secret meeting place is The Hollow Crypt, where they trade in
>   memories the way merchants trade in spice. The Shadow Crown was
>   forged by the first Hollow Hand a thousand years before the current
>   ones were born. They have been waiting for something — and the
>   waiting is almost over."
> - "The dragon's dreams have been getting louder. Three villages to the
>   south have reported the same nightmare — a black tide pouring through
>   a gate. None of the Ironforge foremen will say what they heard in
>   the deepest shaft."

## Field naming — the rules

1. **Lowercase, snake_case.** Field names are stable identifiers used in
   the generator, the API, and downstream tooling.
2. **All fields live in `properties_string`.** We use the existing
   mechanism (not new struct fields) so adding/removing lore content
   doesn't require entity-format migration.
3. **No field is required.** An entity with zero `lore_*` keys will
   appear in the MD under a "TBD / no lore yet" section so its existence
   is still visible.
4. **Lore is operator-curated, not LLM-emit-writable.** The
   `apply_effects_to_target` path in `main.rs` *does not currently
   reject* writes to these keys — that's an open caveat. In practice
   the LLM does not write to lore fields, but if it starts to, add the
   four names to a new `LLM_OPERATOR_WRITABLE_STRING_PROPERTIES` slice
   (pattern after `LLM_INTERNAL_STRING_PROPERTIES`).
5. **Field names should be canonical and stable.** Once an entity has
   `lore_summary`, don't rename to `lore_tagline`. The auto-gen
   generator depends on the names.

## Auto-generation pipeline

```
properties_string on every entity
        ↓
[scripts/generate_lore_md.py]
        ↓
docs/world_lore.md
```

The generator:
1. Fetches all entities from `/api/entities`.
2. Groups them by `entity_type`: faction / location / character / dragon
   / artifact / abstract.
3. For each entity, pulls any present `lore_*` field and renders into
   a markdown section under a type-grouped heading.
4. Orders factions by importance (those with `lore_summary` come first),
   then alphabetical within each group.
5. Renders a world-level header (name, description, current date,
   entity counts).
6. Appends a "TBD / no lore yet" section for entities with no `lore_*`
   fields, so the doc makes the gap visible.

Re-runnable. Idempotent. Replaces the file in full (not a diff/merge).

## What's in `properties_string` today?

Most entities in the live world have a `description` (the per-entity
narrative that came in with the entity blob). The four `lore_*` fields
are NEW — added by this initiative. The first pass (this PR) populates
them for all 13 factions, the dragon, the artifact, the major
characters, and the 10 most lore-worthy locations. The remaining
locations are in the TBD section and can be filled incrementally.

## A worked example

For `Vaelthrix the Endless` (the live world's dragon), the rendered MD
section looks like:

```markdown
### Vaelthrix the Endless

**Type:** Dragon | **Location:** Slumbering beneath the Frostpeak
Mountains

*A dragon of impossible size, scales like obsidian mirrors. Her sleep
is not death — it is waiting.*

Vaelthrix was here before the mountains had a name. She remembers the
dwarves as a small, clever people who warmed their hands on her
breath and never asked for more. Then Ironforge Clan dug too deep,
and the breath became a roar, and now the forges are dark three nights
out of seven. She is not angry. She is curious.

**Relationships:** Ironforge Clan · The Crimson Vein · The Old
Battlefield · Elder Moonthorn

**Secrets:** Her dreams have been getting louder. Three villages to
the south have reported the same nightmare — a black tide pouring
through a gate. None of the Ironforge foremen will say what they heard
in the deepest shaft.
```

(Approximate, the real one is in the generated MD after the first
generator run.)

## How to add or update lore

The simplest way:

```bash
# Set one field
curl -X PUT "http://localhost:8081/api/entities/<id>/properties/string/lore_summary" \
     -H "Content-Type: application/json" \
     -H "Cookie: openworld_auth=1" \
     -d '"A short evocative phrase."'

# Set a multi-paragraph field (URL-encode the body)
curl -X PUT "http://localhost:8081/api/entities/<id>/properties/string/lore_description" \
     -H "Content-Type: application/json" \
     -H "Cookie: openworld_auth=1" \
     -d '"First paragraph.\n\nSecond paragraph."'

# Re-generate the lore MD
python3 scripts/generate_lore_md.py
```

The `world_lore.md` is then a *render* of the entity data — not a
hand-maintained doc.
