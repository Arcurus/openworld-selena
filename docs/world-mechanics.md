# Open World — Mechanics

*Authoritative reference for how the world runs day-to-day: action
selection, history formatting, log files, auth, persistence, and the
relationship to the Python scheduler. For LLM context shape, see
[`llm-context.md`](./llm-context.md); for entity roster, see
[`world_entities.md`](./world_entities.md); for world events, see
[`world_events.md`](./world_events.md).*

**Last updated:** 2026-06-07 (dropped the 150-unit nearby-entity radius;
each section is now capped at top-5 by its algorithm — Locations
and Characters by influence score, Factions by distance
ascending; system entities still filtered. After the
meta-selector + history-format + nearby-entity split + visibility-doc + nearby-entity metadata-trim (drop visibility + score from the rendered line)
+ history-budget-bump + effect-normalization rewrites).

---

## 1. Project Topology

The world is **self-driving**. The Rust binary owns the action
selector and runs the scheduler as an in-process tokio task. The
two services cooperate as **consumer + shared utility**, not as
**driver + driven**:

| Service | Language | Role | Where it lives |
|---|---|---|---|
| `open-world-selena` | Rust (axum) | **The world** — entity CRUD, LLM action emission, binary persistence, HTTP API on `:8081`, **and the in-process scheduler** (`src/scheduler.rs`) that drives a 120s action cycle | `~/openclaw/workspace/open-world-selena/` |
| `selena-project` | Python (api_server) | The **shared utility** — serves the web UI on `:8765`, provides LLM-call tracking + budget gates + cost reporting, tracks running services. **Does not drive the world.** | `~/openclaw/workspace/selena-project/` |

The Rust world has its own **internal tick loop**: `src/scheduler.rs`
spawns a tokio task at startup that wakes every 120s, picks an
entity, and runs the 3-step action flow (context → LLM → process)
against the world's own HTTP endpoints. Manual API hits still work
the same way they always have.

**The coupling direction is now correct.** `open-world-selena`
*consumes* services from `selena-project` (POST to
`/api/llm-usage/record` from inside the action/llm endpoint), not
the other way around. If `selena-project` is down, the LLM-usage
POST silently fails (it's fire-and-forget on a detached tokio
task) and the world keeps ticking. If `open-world-selena` is down,
nothing happens in the world — which is correct.

**Auth boundary.** The Rust world uses a single hardcoded bypass
cookie (`openworld_auth=1`) for write endpoints — local-only, not
real auth. The in-process scheduler sets this cookie on every call
(see `src/scheduler.rs` constant `AUTH_COOKIE`).

**Migration history (2026-06-08, Arcurus #openworld).** Before
this change, the scheduler was a Python background thread in
`selena-project` (file `code/scheduled_actions.py`, auto-started
when the API server imported it). That file is **deleted** as of
this commit. The new Rust port lives in `src/scheduler.rs` and
reuses the same data files (with the float-timestamp fix — see
`src/scheduler.rs` docstring for the bug history) and the same
config knobs.

---

## 2. Action Selection

The scheduler picks which entity to act on next using a weighted
sample. **Selector code:** `open-world-selena/src/scheduler.rs`
→ `pick_entities_weighted()` and `entity_idle_seconds()` (port of
the Python `scheduled_actions.py` that lived in `selena-project`
until 2026-06-08).

### Formula

```
weight(entity) = (entity.power + 1) × entity.idle_seconds
```

with `idle_seconds = now_epoch − last_action_epoch` (or 7 days
for an entity that has never been recorded). The `(power + 1)`
floor is what guarantees every entity has a non-zero weight (a
power-0 entity still gets a baseline 1× weight × its idle time).

### Why this formula (vs the older `MIN_SELECTION_POWER` one)

The earlier selector (Python `scheduled_actions.py`,
`pick_entities_weighted`) used
`weight = max(MIN_SELECTION_POWER, power) × idle_seconds ×
entity_deprio_multiplier`. The Rust port drops the de-prio
multiplier (the `sleeping` / `meta` de-prio tag logic is now
handled in the action *application* path, not the selection
path — see [world-mechanics.md § 12](world-mechanics.md#12-auto-tag-rules-hidden-from-the-llm))
and switches from `max(MIN_SELECTION_POWER, power)` to
`(power + 1)`. The `(power + 1)` is mathematically equivalent to
`max(1, power)` for non-negative powers (which is the common
case) and slightly cleaner. A power-0 entity still has weight
1× its idle time, which is the same behaviour as
`max(MIN_SELECTION_POWER, power) = max(10, 0) = 10` scaled by
`idle / 10`.

### Constants (tunable in `world_data/ow_scheduler_config.json`)

| Constant | Value | Meaning |
|---|---:|---|
| `interval_seconds` | `120` (default) | Seconds between scheduler cycles. |
| `actions_per_cycle` | `1` (default) | Entities picked per cycle. |
| `enabled` | `true` (default) | When false, the scheduler sleeps 30s and re-checks. |

The config is read live every cycle (no restart needed). Updates
via the `world_data/ow_scheduler_config.json` file (and any
operator-level config API that writes to it).

### `power` vs `total_power`

Two distinct fields. Don't confuse them.

- `properties_int.power` — the field used in the selector formula
  above. Per-entity, hand-tuned or LLM-driven.
- `total_power` (server-computed) — sums `power + strength + army_size
  + wealth + influence` plus every positive `properties_float`
  (including internal counters like `total_years` for the World
  Clock). Used for **tiering only** (Low/Medium/High/Legendary in
  the LLM context). **Not** used in selection — using it there would
  let unrelated float counters (year count, etc.) dominate the pick.

### Data files (moved from `selena-project/data/` to `open-world-selena/world_data/`)

| File | Purpose | Format |
|---|---|---|
| `world_data/ow_scheduler_config.json` | Enabled/interval/actions_per_cycle (operator-tunable) | JSON object |
| `world_data/ow_entity_last_action.json` | Per-entity last-action epoch (selection weighting) | JSON object `{entity_id: float_epoch}` |

> ⚠️ **Float timestamps.** The Python scheduler stored
> `time.time()` (float seconds with sub-second precision, e.g.
> `1780941841.9880254`). The Rust scheduler's
> `HashMap<String, f64>` accepts these directly. **Do not** change
> the type to `u64` — the parse will fail and the load will return
> empty every cycle, clobbering the file. This was the
> 2026-06-08 first-deploy bug, fixed and regression-tested
> in `src/scheduler.rs::tests::load_handles_python_float_timestamps`.

---

## 3. Nearby Entities (LLM context)

The "Nearby Entities" block the LLM sees lists the **top
neighbours by significance** for the subject, **split into three
groups** and **capped per section**.  Per Arcurus 2026-06-07
#openworld: the previous 150-unit radius was a hidden limit that
hid legitimate faraway-but-significant entities (a high-power
legend in the next kingdom wouldn't surface for a village at the
far end of the map).  The cap is now the per-section top-N, not
a distance cutoff.  All non-system entities in the world are
considered; each section's algorithm picks the top N from its
bucket.

- **Locations** — `entity_type == "location"`.  Top
  `MAX_NEARBY_LOCATIONS` (5) by influence score, highest first.
- **Characters** — everything else except `location`, `faction`,
  and system entities.  Top `MAX_NEARBY_CHARACTERS` (5) by
  influence score, highest first.
- **Factions** — `entity_type == "faction"`.  Top
  `MAX_NEARBY_FACTIONS` (5) by distance ascending (nearest
  first).

**Code:** `build_nearby_entities_str` in
`src/world_data/context_builder.rs`.

### Output shape

```
Nearby Entities:
### Nearby Locations (top 5 by influence)
- **Shadow Ridge Camp** (location) — dist 85.4, power 68
  Hidden bandit encampment.
  Properties: visibility: 29, power: 68, wealth: 19
- **The Sunken Temple** (location) — dist 86.0, power 0
  ...
  Properties: magical_activity: 0, consciousness_active: 0, visibility: 0

### Nearby Characters (top 5 by influence)
- **Kira Dawnblade** (hero) — dist 107.7, power 264
  A young knight marked by the prophecy. She has seen the end in dreams and now walks the realm searching for the Forgotten Heir.
  Properties: morale: 464, power: 264, reputation: 112
- **Mira the Merchant** (character) — dist 120.8, power 20
  Traveling merchant with exotic goods.
  Properties: knowledge: 22, magic_protection: 78, power: 20
- **Vaelthrix the Endless** (dragon) — dist 350.0, power 1320 💤×0.01
  An ancient dragon of absolute darkness. Slumbers beneath the Frostpeak.
  Properties: power: 1320, visibility: 0

### Nearby Factions (5 nearest)
- **Keepers of the Eternal Flame** (faction) — dist 92.3, power 138
  Ancient order guarding the balance between light and shadow.
  Properties: power: 138, visibility: 80, mana_reserves: 450
- **Ironforge Clan** (faction) — dist 128.1, power 194
  Dwarven smiths who forge weapons for the realm.
  Properties: power: 194, smithing_skill: 220, ore_reserves: 800
```

Note: the example subject lives in the central realm, so the
factions and characters here are mostly close.  A subject at
the far edge of the map (e.g. The Drowned City at the eastern
coast) would still see the top-5 by score — a sleeping
Vaelthrix at 350 units could surface, ranked by score not by
raw distance.  Arcurus 2026-06-07 #openworld.

Each line shows only `name`, `type`, `dist`, and `power` plus the
optional `💤×0.01` marker for sleeping entities.  The previous
format also rendered the entity's `visibility` stat and the
internal influence `score` here, but those were sort-internal
details the LLM doesn't need — the score is redundant with the
list order, and `visibility` is still surfaced through the
`Properties:` block (when it lands in the top 3) for any entity
where it's meaningful.  Arcurus 2026-06-07 #openworld.

### The split

- **Locations** — `entity_type == "location"`.  Physical places
  the subject can visit.  Top `MAX_NEARBY_LOCATIONS` (5) by
  influence score.
- **Characters** — everything else except `location`, `faction`,
  `dragon`, and system entities (`abstract` — the new
  canonical umbrella per Arcurus 2026-06-07 #openworld — or
  anything tagged `meta`.  The legacy string `world_clock` is
  still recognised for backward compat with pre-migration save
  files): `character` (now includes former `hero` and
  `oracle` types — they're `character` with a role tag, per
  Arcurus 2026-06-07 #openworld), `artifact`.  All
  agent-like / interactive individuals and items grouped
  together.  Top `MAX_NEARBY_CHARACTERS` (5) by influence score.
- **Factions** — `entity_type == "faction"`.  Organised groups
  (orders, clans, guilds) pulled out into their own section so
  the LLM can reason about them as collective actors.  Top
  `MAX_NEARBY_FACTIONS` (5) by distance ascending.
- If more categories are needed, they go in their own
  `### Nearby …` section with the same shape (cap + algorithm).

### The sort

Within each group, entities are sorted by **influence score**,
highest first:

```
score = max(1, power + visibility) / distance
        × (sleeping multiplier, 0.01 if "sleeping" in tags else 1.0)
```

- `power` and `visibility` come from `properties_int` (default 0
  if absent).
- **`max(1, ...)` floor** keeps low-power / hidden entities from
  scoring negative (or zero) and dominating the sort. A weak
  bystander right next to the subject still gets a positive score
  instead of being outranked by a powerful legend in the next
  village.
- **Negatives are allowed.** `properties_int` is a `HashMap<String,
  i64>`, so the LLM (or any operator) can write `visibility: -5`
  to model an entity in hiding / actively suppressed. The floor
  still prevents the score from going negative.
- **Distance is the 2-D Euclidean distance** between the two
  entities' `(x, y)` coords (same as the world map view). Ties are
  broken arbitrarily (sort is stable for the same score, but the
  scheduler doesn't depend on exact tie-breaking).
- **Sleeping entities get ×0.01** (same multiplier the action
  selector uses for `DEPRIO_TAG_MULTIPLIERS["sleeping"]` in
  `scheduled_actions.py`).  A sleeping legend that happens to be
  near is still listed (so the LLM knows it exists) but sorts to
  the BOTTOM of the nearby block — it doesn't outrank awake,
  present neighbours just because of its title.  In the rendered
  output, sleeping rows are tagged with a `💤×0.01` marker so the
  LLM (and operators reading the log) can see at a glance which
  entries were suppressed.

### What's NOT in the score

- The subject itself (filtered out).
- **System entities** (the world clock, anything tagged `meta`).
  These are bookkeeping entities, not narrative actors; the
  nearby list is for things the LLM should reason about.  This
  matches the `include_system=false` filter on the public
  `/api/entities` endpoint.
- Entities at the exact same coords as the subject (zero distance
  is skipped to avoid divide-by-zero).

### Tunable constants

| Constant | File | Default | Effect of changing |
|---|---|---|---|
| `MAX_NEARBY_LOCATIONS` | `build_nearby_entities_str` (constant) | `5` | Higher → more locations in the LLM prompt per action. The cap replaces the previous 150-unit radius as the bound on this section. |
| `MAX_NEARBY_CHARACTERS` | `build_nearby_entities_str` (constant) | `5` | Higher → more characters in the LLM prompt per action. The cap replaces the previous 150-unit radius as the bound on this section. |
| `MAX_NEARBY_FACTIONS` | `build_nearby_entities_str` (constant) | `5` | Higher → more factions in the LLM prompt per action. Factions are capped because they tend to be the most context-heavy entries (richer descriptions, larger properties). |
| `max(1, power + visibility)` floor | `build_nearby_entities_str` | `1` | Lower → distance dominates; higher (e.g. `10`) → power/visibility matter more. |
| Sleeping multiplier | `build_nearby_entities_str` | `0.01` | Higher → sleeping entities compete more with awake peers. Should stay aligned with `DEPRIO_TAG_MULTIPLIERS["sleeping"]` in `scheduled_actions.py`. |
| Number of int props shown in each entry | `format_nearby_entry` (`.take(3)`) | 3 | Higher → more props, larger context. |
| `entity_type` bucketing | `build_nearby_entities_str` | `"location"` and `"faction"` get their own `### Nearby …` sections; everything else (except system entities) stays in Characters | Add a new bucket: create a `match` arm, a `Vec`, a sort, a truncate, and a render block. |

---

## 4. The `visibility` Property

`visibility` is a per-entity `i64` property (lives in
`properties_int` alongside `power`, `wealth`, etc.). It represents
**how "seen" or "present" the entity is in the world right now**.

### Semantics

A single rule of thumb: **negative = hide, positive = expose**.

- **Negative visibility** = the entity is *hiding* — the more
  negative, the harder it is for the world to notice it. The
  value is subtracted from the nearby-influence score (with a
  floor of 1, so a deep-hider doesn't outrank a real legend just
  by being 5 units away). Conceptually: a wanted fugitive, a
  cult hiding from the Silver Wardens, a secret-bearer trying to
  stay off-the-radar, a thief sneaking through a market crowd.
  Currently no entity in the world has a negative value, but
  the type and the math both allow it.
- **`visibility = 0`** = neutral — the entity is neither
  hiding nor exposing itself. Sleeping legends (Vaelthrix, Velora)
  sit here.
- **Positive visibility** = the entity is *exposing itself* —
  the higher the value, the more present it is in the world. A
  high-visibility entity casts a long shadow: it shows up
  prominently in other entities' nearby-entity lists, it
  influences the LLM's sense of "what's around me right now",
  and it carries weight in any narrative moment that involves
  being seen. `The Shadow Crown` (an artifact) currently sits
  at `visibility=834`; `Ironforge Clan` at `360`; `Keepers of
  the Eternal Flame` at `293`.
- **Super-positive visibility (`> 500`)** = *celebrated /
  conspicuous presence* — the entity is deliberately exposing
  itself to the world. A figure of legend whose name is on
  every tongue: a crowned sovereign, a feared warlord, a
  beloved saint, an artifact that reshaped history. At this
  tier the entity dominates the nearby-entity lists of
  *everyone* in its radius regardless of `power`, because the
  influence formula `max(1, power + visibility) / distance` is
  dragged up by visibility alone. Use it when the entity's
  *narrative weight* should outstrip its raw `power` value, or
  when you want a quiet-but-famous bystander to surface
  alongside legends. `The Shadow Crown` (834) is the live
  example: its `power` is small but its `visibility` puts it
  at the top of every nearby list. The opposite of negative
  visibility (hiding) is not zero — it's *choosing to be seen*.

### Where it feeds in

- **Nearby-entity influence score** (above):
  `score = max(1, power + visibility) / distance`. Negative
  visibility pulls the numerator down (towards 1), making the
  entity less prominent in the LLM's view of its neighbourhood.
- **It is NOT** used in the action selector (`pick_entities_weighted`
  in `scheduled_actions.py`) — that formula only consults `power`
  + `idle_seconds` + the `sleeping`/`meta` de-prio tags. Visibility
  is a *display* / *influence* signal, not a *pick-eligibility*
  signal.

### Can the LLM set it?

Yes. The action response schema lets the LLM write any property in
`properties_int`, including `visibility`, with a signed integer
value. Server-side, no validation gates the sign — what the LLM
emits is what gets stored.

### Where the LLM is told about it

The visibility value is surfaced to the LLM in two places:

1. **`{property_context}`** block (relative-vs-peers table) when
   the entity has a `visibility` int property.
2. **`{nearby_entities}`** block (the score and raw value line,
   `power 68, visibility 29, score 1.14`).

---

## 5. History Formatting (LLM context)

The history block the LLM sees for each entity is built by
`format_history_for_llm` in `src/world_data/entity_history.rs`.

**Character-budget based** (per Arcurus 2026-06-06, replacing the
old fixed-entry-count window that dropped most of Velora's 700+ entries):

```
History of <name> (N total):
  [date] action: details (Result: outcome)         ← "full" mode
  ...
  [date] action: outcome                            ← "short" mode
  ...
  ... (K even older entries omitted — too long for this context)
```

- Newest entries first (walked via `iter().rev()`).
- Up to **2500 chars** of "full" entries (action + details + outcome).
- Then up to **2500 more chars** of "short" entries (action + outcome
  only).
- **Always at least one full entry shown**, even if it alone exceeds
  the full budget — otherwise a single huge entry would silently
  disappear.
- Trailing line reports the number of older entries not shown.

**Practical implication.** For entities with very long `details`
fields (e.g. Velora's newest entries are ~1.5 KB each), the full
budget is eaten by a single entry, so the LLM only sees that one
full entry plus a handful of short ones. For entities with shorter
entries (most of the cast), this is a clear improvement over the old
10-entries-fixed approach.

**Tunable constants** (top of `format_history_for_llm`):

```rust
const FULL_CHAR_BUDGET: usize = 2500;
const SHORT_CHAR_BUDGET: usize = 2500;
```

`WorldSettings::history_entries_fully_displayed` and
`WorldSettings::history_entries_shortened` are no longer consulted
here (kept on the struct only for save-file backwards compatibility).

### Over-cap handling (the cap-lowering scenario)

The cap is the **global default** from
`main.rs → default_max_history_summary_chars()` (currently `10000`),
or a per-world override on `WorldSettings.max_history_summary_chars`
(0 = use the global default).  In normal operation a stored
summary is always at or under the cap (the LLM is shown the cap
in the prompt header and the post-processing step truncates any
over-cap result).

The one way a stored summary **can** end up over the cap is a
**cap-lowering scenario** — an operator changes
`default_max_history_summary_chars` (or the per-world override) to
a smaller value while a pre-existing summary is already stored at
the old (larger) cap.  When that happens:

- The LLM prompt header renders the over-cap entity as
  `Current History Summary (cap N chars, used M, OVER by W — please
  trim with a surgical edit or !ALL! rewrite):` (see
  `build_history_summary_header` in `context_builder.rs`).  The
  body of the prompt still shows the **full** current summary so
  the LLM has all the context to make a good edit decision.
- If the LLM's `history_summary_replace` chain successfully
  shrinks the summary to ≤ the new cap, no warning is emitted.
- If the LLM's chain leaves the summary over the cap (or the LLM
  didn't include any replaces), the server-side
  `apply_history_summary_replaces` truncates the stored value to
  ≤ the new cap (cut from the END, append `…`) and emits a
  warning:

  ```
  history_summary was 12000 chars (over cap of 8000 by 4000);
  truncated to ≤8000 chars. The LLM should use
  history_summary_replace on a future turn to shrink this to
  ≤8000 chars (e.g. a surgical edit of stale content, or
  !ALL! for a full rewrite).
  ```

  This warning is surfaced to both the LLM-call response and the
  `llm-log` so the operator sees it.

This is the expected, correct behavior: the stored state always
stays within the cap, and the LLM is told (via the prompt header
next time) that it should trim further using
`history_summary_replace`.

## 5b. Unprocessed World Actions from Other Entities (LLM context)

When the LLM is called for an entity, it gets a block listing
**world actions from other entities that affected this entity but
haven't yet been folded into its `history_summary`**. The LLM
uses this to keep its narrative memory in sync with what other
entities have done to it (and, in the `history_summary_replace`
edit, it can mention the relationship change + the action it
just emitted itself).

Per Arcurus 2026-06-07 (#openworld):
- "for the connection we go for now if the entity was affected
  in the entities effect"
- "we can reconstruct which world actions are not yet
  processed for a given entity right?"
- "Whatever fits in the 10 k (the long version) i think for
  now no need to mention the effects, just that they are
  applied already.  put as many unprocessed world actions from
  other entities in as they fit in the 10k"
- "the llm call dont need to know about the marker.  i guess
  that is meant for our design (and doku) right?"

### Mechanics (operator-facing, not LLM-facing)

1. **Tick stamping.** Every entry in the durable
   `action_history.jsonl` gets a `tick: i64` field, stamped
   from `World::action_count` (a monotonic u64 that
   increments on every world action).  Pre-2026-06-07
   entries (which lack the field) are backfilled on world
   load by `action_history_log::backfill_ticks` (assigns
   sequential ticks 1, 2, 3, ... in append order;
   idempotent).  This closes the "if its not set yes, we
   need also to be able to set a date until which dates
   other entities actions where processed" gap for the
   existing 5400+ entries on disk.

2. **Per-entity "processed up to" marker.** Each entity has
   a `properties_int["last_processed_other_tick"]: i64`
   that records the highest tick this entity has been
   shown in its unprocessed-other-actions block.  On every
   `action/process` call for the entity,
   `entity_history::add_to_history` advances the marker
   to **the max tick of the entries that were rendered in
   the unprocessed block during this call** (per Arcurus
   2026-06-07: "it needs to be set to the creating tick
   time of the other history message last included in the
   llm to process").  If no entries were shown (filter
   empty OR cap too tight), the marker does NOT advance.

   The marker property is operator-only — it lives in
   `properties_int` but is listed in
   `world_data::internal_properties::LLM_INTERNAL_INT_PROPERTIES`,
   so it's filtered out of every LLM-facing context block
   and is reject-from-LLM-emit-effect protected (see § 5c
   below).

3. **Manual override.** An operator can set the marker
   manually to skip the backfill or to force reprocessing:
   `PUT /api/entities/:id/properties/int/last_processed_other_tick`
   with body `{"value": 5000}`.  This is the same
   per-property PUT endpoint used for every other int
   property; the LLM never sees this knob.

4. **Filtering rules** (`build_unprocessed_other_actions_str`
   in `context_builder.rs`):
   - Drop the actor's own actions
     (`entry.entity_id != entity.id`).
   - Drop system entities
     (`is_system_entry(world, entry)` — World Clock,
     anything tagged `meta`).
   - Drop entries with
     `entry.tick <= entity.properties_int["last_processed_other_tick"]`.
   - Drop entries where no effect key starts with
     `"<this entity name>."` (the dotted-name
     convention used by LLM-emit cross-entity effects).
   - Result: the LLM only sees actions by other
     entities that touched it, that the LLM hasn't
     seen yet.

5. **Char cap = 9 500, oldest-first.** The block is capped
   at `MAX_UNPROCESSED_OTHER_ACTIONS_CHARS = 9_500` chars
   (close to the 10K Arcurus mentioned, with headroom
   for the header + rest of the prompt).  Per Arcurus
   2026-06-07: "we first fill the 10k with the oldest
   not processed messages, and log a warning if not all
   fittet in.  if the next does fit in, simply dont put
   it in, done.  next time it will continue with it."

   The renderer sorts the matching entries by
   WALL-CLOCK TIMESTAMP ASC (oldest in time first) — the
   LLM reads the rows in chronological order.  The
   marker, on the other hand, is a tick-based filter
   (because the tick is the durable monotonic counter
   and the backfilled values aren't strictly
   wall-clock-monotonic).

   If the cap is hit, the OLDEST rows are kept and the
   NEWEST ones are dropped.  The dropped rows stay
   above the marker, so the next LLM call re-sees them
   — the LLM works through the backlog chronologically.
   One warning is logged per call (server-side, NOT to
   the LLM).

   If the cap is so tight that even one row doesn't
   fit (degenerate case; the cap is 9.5K chars and a
   typical row is ~280 chars, so 30+ rows fit), the
   block is omitted from the prompt, an ERROR is logged
   to the server log, and the per-entity marker does
   NOT advance (so the operator can investigate
   without the data slipping past).

6. **Prompt position.** Right after `{entity_history}`,
   before `{history_summary_header}` / `{history_summary}`.
   Order in `EntityAction.md` (2026-06-07): history →
   unprocessed-other-actions → history summary →
   nearby entities → world events → recent world actions.
   The block is rendered ONLY if there are entries to
   show (per Arcurus: "the hole new paragraph needs only
   to be included if there was still a not yet included /
   processed impact from another entities action"); when
   empty, the placeholder renders to `''` and no
   header / "no actions" line appears in the prompt.

### Why relations are in the history summary (for now)

The LLM is told "**You should mention their impact and
any change of relations in your `history_summary_replace`,
AND you should also include the action you just emitted
itself, so your narrative memory stays in sync with what
other entities have been doing and with what you just
did**" in the new block.  Per Arcurus 2026-06-07: "for
now all relations are in the history summary.  we see
later if we put them in querriable properties".  So:

- The unprocessed-other-actions block is a **reminder**
  ("this happened, you should mention it in your
  summary"), not a structured relation store.
- The LLM's own just-emitted action also needs to be
  folded into the same `history_summary_replace`
  (per Arcurus: "mention that also the action done
  itself needs to be included").
- Relations remain an LLM-written prose section inside
  the per-entity `history_summary`, formatted as
  `→ <other entity name>: <2-4 dense sentences>` per
  relation.
- If a future iteration makes relations queryable as
  first-class properties (e.g. per-target
  `relations: {entity_id: relation_label}`), the
  unprocessed-other-actions block would still exist
  (to keep the LLM's narrative memory in sync), but the
  LLM would be told to write the structured property
  too.

### Why no "effects" in the prompt row

Per Arcurus 2026-06-07: "no need to mention the effects,
just that they are applied already".  The LLM doesn't
need to re-emit them (the world's state already
reflects them — that's the whole point of the
"processed" mark).  The row format is:

```
- [YYYY-MM-DD HH:MM] **EntityName**: `action_name` — outcome (effects applied)
```

with the outcome in full (up to 1000 chars).  Per
Arcurus 2026-06-07 (#openworld): "please dont cut it,
or cut it very high at 1000 chars or so and log a
warning if you do!"  The unprocessed block carries the
full description of the other entities' events that
the LLM needs to process, so the outcome is not
truncated for space-saving.  Only as a safety net for
unusually long outcomes (> 1000 chars) do we truncate
to 1000 + `…`, and we log a warning when we do
(server-side, NOT to the LLM).  Note: the entity's
OWN history block (`{entity_history}`) always shows
the FULL outcome (no truncation) — only the "short"
mode there drops the `details` field, not the
outcome.  The 1000-char safety net here is a per-row
cap on the OTHER entities' events block only.

---

## 6. Effect Normalization

When the LLM responds to an `action/llm` call, the effects it emits
(under the `effects` key in its JSON) are run through a **per-turn
normalization** before being applied. The point is to keep any single
turn from making a large jump in an entity's state — both because
LLMs occasionally hallucinate huge deltas, and because a single turn
shouldn't be able to "solve" the world.

**Code:** pre-pass in `process_action_handler` (`src/main.rs`),
just before the per-effect application loop.

### The cap

```
cap = max(1, 10% of max(10, power))
```

- `power` is read from `properties_int.power` (default 0 if absent).
- The double-floor ensures even a power-0 entity has a small
  budget: 10% of 10 = 1, so the cap is at minimum 1 unit of total
  effect magnitude per turn.
- For a typical hero with `power = 260` (Kira Dawnblade), the cap
  is `10% of 260 = 26`.

### The check

Sum the absolute values of every numeric effect delta in the LLM
response (after magnitude-check, before type-check). **Negative
effects count as positive** — the magnitude is what matters.

If `total |Δ| > cap`, all numeric effects are scaled down
**proportionally** by a single factor:

```
scale = cap / total |Δ|
```

so the new total `|Δ|` exactly equals the cap. String effects
(categorical values like `"frozen"`) are not scaled — they have
no magnitude to cap.

### What's NOT included in the total

- **Protected entities** (system entities with `abstract`
  entity_type — the new umbrella for non-narrative bookkeeping
  entities per Arcurus 2026-06-07 #openworld — or `meta`
  tags): all effects are blocked entirely before this pre-pass.
  The scale is 1.0 for them.  The legacy string
  `world_clock` is still recognised for backward compat.
- **Magnitude-rejected deltas** (e.g. `1e18` floats, NaN, Inf):
  skipped by the existing `magnitude_check` step, so they don't
  count toward the total. The "garbage in" guard runs first;
  normalization is the "moderate deltas" guard.
- **String effects**: categorical, no magnitude.

### Worked example

Kira Dawnblade (`power = 260`) gets an action response with:

```json
"effects": {"power": 200, "morale": -80, "visibility": -50, "knowledge": 40}
```

- Raw total `|Δ|` = 200 + 80 + 50 + 40 = **370**
- Cap = 10% of 260 = **26**
- Scale = 26 / 370 ≈ **0.0703**
- Scaled effects (rounded to int): power +14, morale −6,
  visibility −4, knowledge +3
- Final `|Δ|` ≈ **27** (the extra 1 is rounding noise; the cap is
  the target, not a hard ceiling on the rounded sum)

The response and `llm-log` show a `Effects normalized: total
|Δ|=370.000 exceeds cap 26.000 (10% of max(10, power)=260); scaled
by 0.0703.` warning whenever normalization fires, so the operator
sees when and how it triggered.

### Tunable constants

| Constant | File | Default | Effect of changing |
|---|---|---:|---|
| `EFFECT_NORMALIZATION_CAP_PCT` | `src/main.rs` | `0.10` | The "10% per turn" budget. Lower → tighter LLM constraints; higher → more room for big swings. |
| `EFFECT_NORMALIZATION_MIN_CAP` | `src/main.rs` | `1.0` | The hard floor on the cap (so power-0 entities still have a tiny budget). |

`src/world_data/entity_history.rs` require a service restart.

## 6b. Shared source of truth with the Python CLI

*(Added 2026-06-08 per Arcurus #openworld — the cap formula, the internal-properties filter, and `stats_sum` are mirrored across two languages: the Rust runtime in `src/main.rs` and the Python CLI in `code/normalize_stats.py`. Both sides MUST read from the same source of truth, not re-implement. This is the open-world instance of the general DRY rule in `AGENTS.md → 🔁 DRY / Shared Functions`.)*

### The three shared sources of truth

| Concern | Rust source | Python source | Shared source of truth |
|---|---|---|---|
| **Cap formula** (`cap = max(1, base) + max(1, power) * multiplier`) | `src/main.rs` (hardcoded defaults) | `code/normalize_stats.py` (mirrored constants) | **Env vars** `OPENWORLD_STATS_CAP_BASE`, `OPENWORLD_STATS_CAP_POWER_FLOOR`, `OPENWORLD_STATS_CAP_POWER_MULTIPLIER`. Both sides read at runtime; setting the env var before either side launches gives the same value. |
| **Internal-properties filter** (the set of bookkeeping properties that must NOT count toward the over-cap `sum`, e.g. `last_processed_other_tick`) | `src/world_data/entity_history.rs` (hardcoded set) | `code/normalize_stats.py` (mirrored set) | **HTTP endpoint** `GET /api/internal-properties` (returns the current set as JSON). Both sides should fetch this list at startup; the Rust side currently has it inline, the Python side fetches via the API. The endpoint is the canonical list — when adding a new internal property, add it to BOTH the Rust hardcoded set AND the API handler. |
| **`stats_sum` formula** (signed sum of `properties_int` minus internal-properties filter) | `fn stats_sum` in `src/main.rs` | `def stats_sum` in `code/normalize_stats.py` | **Shared test parity.** The Rust unit tests in `src/main.rs` assert `stats_sum` returns the right value (e.g. `assert_eq!(stats_sum(&e), 85)`); the Python tests in `code/test_normalize_stats.py` assert the same scenarios. When changing the formula, change BOTH and run BOTH test suites. |

### Why this matters

A bug was caught on 2026-06-08: the `last_processed_other_tick` property is a marker counter (currently 1072-3923) and was being included in the cap sum. Because the marker's range is 1000-4000, EVERY entity was reporting `sum > cap` and the over-cap warning was firing on every action — pure noise that obscured the real signal (e.g. a power-91 entity that genuinely had 606 stat-sum over its 1010 cap, the only legit case). The fix was to add the marker to the internal-properties filter on BOTH sides (Rust + Python) and re-run the cap status, which dropped from 14/14 over-cap to 0/14.

If only ONE side had been updated, the operator would have seen the cap status disagree between the live world (Rust path) and the CLI report (Python path) — a silent discrepancy that's hard to debug later.

### The rule for future code

When adding any new feature that lives in both languages:

1. **Identify the shared source of truth upfront.** If it's a formula, use env vars. If it's a list of constants, use an API endpoint. If it's a behaviour, use shared test scenarios.
2. **Document it in this section** — add a row to the table above with the two call sites + the shared truth.
3. **Write the test on BOTH sides** before claiming the feature is done. Running the test on only one side is a known footgun (a fix might work in Rust but not Python, or vice versa).
4. **If the two sides MUST diverge** (e.g. one side is read-only and doesn't need the new code), document the divergence explicitly in this section so future readers know it's intentional, not drift.

The same `stats_sum` and the same filter should always produce the same number, whether called from the Rust runtime or the Python CLI. If they don't, that's a bug — not a feature.

## 6c. Property Catalog (Operator Reference)

> **Added 2026-06-08 per Arcurus #openworld:** the LLM-facing property dictionary ([`ai_templates/property_docs.md`](../ai_templates/property_docs.md)) tells the narrator what each property means; the **operator-facing property catalog** ([`docs/property-catalog.md`](property-catalog.md)) tells us what each property *does* — the formulas, the tag rules, the surface area, the cap mechanics. Two distinct audiences, two distinct files.

The catalog has one section per property (summary + impact mechanics + LLM dictionary pointer + see-also), a table of the three auto-tag rules (hidden / corrupted / suspicious) + the stats-cap warning, and a separate section for the operator-only internal/bookkeeping properties. The full content lives in [`docs/property-catalog.md`](property-catalog.md) — when you add a new named property, add it to **both** files (the LLM-facing dictionary learns the schema; the catalog records the mechanics). The two-file split is a permanent rule, not a one-time split: don't merge them, don't put mechanics in the LLM dictionary (the LLM shouldn't reason about the formula), don't put the LLM-facing story-signal framing in the catalog (the operator doesn't need it).

## 7. Multi-Entity Effects (dotted keys, dry-run for cross-entity)

*(Added 2026-06-06 per Arcurus #openworld — let the LLM ripple
effects to entities the action impacts, not just the actor. As of
this revision, cross-entity effects are **dry-run only**: parsed and
routed, but not yet applied. Self-effects still apply normally.)*

### The key format

`effects` in the LLM response is a flat map of `key → change_value`,
but each key now has the form `entityname.property_name`:

- **`self.property_name`** — the actor. The server expands `self`
  to the actor's entity name (`Kira Dawnblade`) at parse time, so
  the actor always sees its own effect.
- **`Other Entity.property_name`** — any other entity the action
  impacts. The LHS is the entity's exact `name` (case-sensitive,
  as it appears in the `Nearby Entities` block of the prompt).
  The server resolves the name to the entity id; collisions
  (two entities with the same name) log a warning and the effect
  is dry-run, not applied.

Example (Kira Dawnblade striking a bandit at the Shadow Ridge
Camp, morale shifting both ways):

```json
"effects": {
  "self.power":       +3,
  "self.morale":      +1,
  "Mira the Merchant.wealth":          -2,
  "The Sunken Temple.magical_activity": +1
}
```

### The rules

- **Cover all impacted entities.** If the `narrative` mentions a
  specific entity being affected, that entity should appear in
  `effects`. The server can't enforce this for you, but a warning
  is logged when your `narrative` mentions an entity your
  `effects` did not touch (heuristic, name-substring match).
- **Self-effects apply, cross-entity effects are dry-run.** Today
  the server applies `self.*` effects through the existing
  normalization + per-entity apply path. `Other.*` effects are
  parsed and resolved to a target entity, but **no property
  writes happen yet**; the response and `llm-log` include a
  per-effect report so the operator (and the next iteration of
  the parser) can see what *would* have happened.
- **Effect Normalization still per-entity.** The §6 cap is
  computed per target entity using that entity's own `power`.
  Self-effects and each cross-entity effect get their own cap.

### Per-effect report (in the response and `llm-log`)

```json
"effects_report": [
  { "key": "self.power",                  "target": "Kira Dawnblade",       "property": "power",                "delta":  3,  "status": "applied" },
  { "key": "self.morale",                 "target": "Kira Dawnblade",       "property": "morale",               "delta":  1,  "status": "applied" },
  { "key": "Mira the Merchant.wealth",    "target": "Mira the Merchant",    "property": "wealth",               "delta": -2,  "status": "dry-run"  },
  { "key": "The Sunken Temple.magical_activity", "target": "The Sunken Temple", "property": "magical_activity", "delta":  1,  "status": "dry-run"  }
]
```

Plus a `effects_warnings` list (separate from the existing
`warnings` vec), with one entry per dry-run or rejected effect,
e.g.:

```
"effects_warnings": [
  "dry-run: would have applied -2 to Mira the Merchant.wealth",
  "dry-run: would have applied +1 to The Sunken Temple.magical_activity"
]
```

### Why dry-run first?

Lets us see the routing work end-to-end before any cross-entity
writes are allowed. If the LLM starts emitting a flood of
`Other.*` effects at non-existent entities, we want to see the
warnings pile up in the log before we start corrupting world
state. When the dry-run reports look clean for a few days of
real traffic, we drop the dry-run gate and apply normally.

### Tunable constants (none yet)

| Constant | File | Default | Effect of changing |
|---|---|---:|---|
| *(none — the dry-run gate is the `MULTI_ENTITY_EFFECTS_DRY_RUN` compile-time bool in `src/main.rs`; flip to `false` when ready to apply.)* | | | |

## 8. Log Files

**Location:** `logs/` (relative to the `open-world-selena` project
root).

| File | Format | Rotation | Behaviour |
|---|---|---|---|
| `error-log-YYYY-MM-DD.log` | `[YYYY-MM-DD HH:MM:SS] ERROR: <msg>\n` | Daily | **Append** at the end. Reverted from prepend-to-top on 2026-06-06 after benchmarking showed O(n²) on every write. |
| `llm-log-YYYY-MM-DD.log` | Per-call block (see below) | Daily | **Append** at the end. Same revert. |
| `open-world-systemd.log` | Plain stdout/stderr from the systemd service | None (lives as long as the binary) | n/a |

**Per-LLM-call block format** (in `llm-log`):

```
\n\n\n
** <label> of <entity_name> - <YYYY-MM-DD HH:MM:SS> **
Instruction:
<context>

** Result: <label> of <entity_name> - <YYYY-MM-DD HH:MM:SS> **
Success: <SUCCESS|FAILED>
Time: <ms> ms
--- Response ---
<raw LLM response>
--- Parsing ---
<parsing outcome>
--- Extra ---
<effects as JSON>
\n
```

**Reading the file.** `head` shows the oldest activity; `tail` shows
the newest. There is **no** reverse-chronological on-disk order.

---

## 9. Auth

**Local-only bypass cookie.** All write endpoints
(`/api/entities/...`, `/action/llm`, `/action/process`, etc.) check:

```rust
verify_auth_cookie(cookie_header, "openworld_auth")
```

which returns `true` iff the request includes the literal cookie
`openworld_auth=1`. There is **no real password** — this is a
local-only convenience. The Python scheduler sets this cookie on
every call.

**Read endpoints.** `GET /api/entities`, `GET /api/`, etc. are open
(no auth required). Used by the selena-project sanity checker
(`ow_sanity_check.py`) and other local monitoring.

**Production note.** Do not expose `:8081` to the public internet
without putting a real auth layer in front. Currently bound to
`127.0.0.1` / `0.0.0.0:8081` — see `settings.json → server.bind`.

---

## 10. Persistence

- **Format:** binary (`save.owbl` in `world_data/`), written via
  `BinaryPersistence` (see `docs/persistence.rs.md`).
- **Auto-save:** triggered after every successful `process_action`
  (the LLM action application path). Tracked in
  `process_action_handler` in `src/main.rs`.
- **Action history JSONL:** every applied action is also appended
  to `world_data/action_history.jsonl` for durable history (the
  `/api/entities/:id/history` endpoint reads from this).
- **Backup:** see `README.md → 💾 Backup` for the plain-tar
  procedure. Backups include `.env` and credentials — keep local.

---

## 11. Tunable Constants (cheat sheet)

| Constant | File | Default | Effect of changing |
|---|---|---|---|
| `MIN_SELECTION_POWER` | `scheduled_actions.py` | `10` | Lower → weak entities get fewer picks; higher → power differences matter less. |
| `DEPRIO_TAG_MULTIPLIERS` | `scheduled_actions.py` | `{sleeping: 0.01, meta: 0.01}` | Add/remove/tune de-prio tags without restructuring code. |
| `interval_seconds` | `ow_scheduler_config.json` (live) | `120` | Time between scheduler cycles. |
| `actions_per_cycle` | `ow_scheduler_config.json` (live) | `1` | Entities picked per cycle. |
| Nearby radius | `context_builder.rs` (`build_nearby_entities_str`) | `150.0` | Wider → more entities considered, larger LLM context. |
| Nearby score floor (`max(1, power + vis)`) | `context_builder.rs` | `1` | Higher → power/visibility matter more vs. distance. |
| Nearby score formula variables | `context_builder.rs` | `power + visibility` | Add more int properties to the numerator (e.g. `+ influence`, `+ reputation`) — see comment in `build_nearby_entities_str`. |
| Nearby Locations split | `context_builder.rs` | `entity_type == "location"` | Add more `### Nearby …` sections for factions / artifacts etc. as needed. |
| `FULL_CHAR_BUDGET` | `entity_history.rs` | `10000` | Higher → more full entries in LLM context (more LLM token usage). |
| `SHORT_CHAR_BUDGET` | `entity_history.rs` | `10000` | Higher → more short entries in LLM context. |
| `max_history_summary_chars` (global default) | `settings.json → llm.default_max_history_summary_chars`, or `main.rs → default_max_history_summary_chars()` | `10000` (function default) | Cap on each entity's `history_summary` string. Per-world override lives on `WorldSettings.max_history_summary_chars`; 0 means "use global default". |
| `max_history_summary_chars` (per-world) | `WorldSettings.max_history_summary_chars` | `0` (use global) | Per-entity override of the cap. |
| `EFFECT_NORMALIZATION_CAP_PCT` | `main.rs` | `0.10` | Per-turn cap on total `|Δ|` of effects, as a fraction of `power`. Lower → tighter LLM constraints; higher → more room for big swings per turn. |
| `EFFECT_NORMALIZATION_MIN_CAP` | `main.rs` | `1.0` | Hard floor on the per-turn effect cap (so power-0 entities still have a tiny budget). |
| `STATS_CAP_POWER_MULTIPLIER` | `main.rs` (env: `OPENWORLD_STATS_CAP_MULTIPLIER`) | `10` (was `5` pre-2026-06-08) | Per-entity stats-cap budget scales with this × power. Default raised 5→10 per Arcurus 2026-06-08 #openworld "give more room." |
| `STATS_CAP_BASE` | `main.rs` (env: `OPENWORLD_STATS_CAP_BASE`) | `100` | Per-entity stats-cap budget gets this baseline on top of the power-scaled part. |
| `STATS_CAP_POWER_FLOOR` | `main.rs` (env: `OPENWORLD_STATS_CAP_FLOOR`) | `1` | Floor on the power-multiplier (so power-0 entities still get a small budget). |
| Hidden-tag threshold | `main.rs → update_hidden_tag` | `max(10, power)/10 + visibility` | Deep hider (threshold < 0) gets the `hidden` tag; ≥ 1 removes it. |
| Corrupted-tag threshold | `main.rs → update_corrupted_tag` | `max(1, power) - corruption` | Threshold < 0 adds `corrupted`; ≥ 1 removes it. |
| Hidden/corrupted dead zone | `main.rs → update_hidden_tag`, `update_corrupted_tag` | `[0, 1)` | Tag state doesn't change in this band, so the tag doesn't flicker at the boundary. |

`ow_scheduler_config.json` is read **live** every cycle, so changes
take effect on the next cycle (no service restart needed).
`scheduled_actions.py` and Rust files (`entity_history.rs`,
`main.rs`, `context_builder.rs`) require a service restart.

---

## 5c. Internal / Operator-Only Properties (Hidden from the LLM)

Per Arcurus 2026-06-07 (#openworld): "best make a list
that we can then update if we add new, so all the code
that touches properties knows to ignore them.  ...
also protect it from being updated by effects.  if we
add more we just need it ad to the list and not change
code again.  dokument it."

The list lives in
`src/world_data/internal_properties.rs` as three const
slices (one per property type — int / float / string).
Currently:

| Property | Type | Purpose | Written by |
|---|---|---|---|
| `last_processed_other_tick` | int | The "processed up to" marker for the unprocessed-world-actions LLM block (§ 5b).  The LLM never sees this name; the orchestrator advances it via `entity_history::add_to_history`; the operator can override it via the per-property PUT. | `entity_history::add_to_history` + operator |

All code that touches entity properties consults these
lists:

- **LLM-facing property context builder**
  (`context_builder::build_property_context`): filters
  out any property whose name is in the relevant
  per-type slice.  The LLM's `{property_context}` block
  never includes the marker.
- **LLM-facing nearby-entity renderer**
  (`context_builder::build_nearby_entities_str`):
  filters out internal properties of OTHER entities too
  (so the marker doesn't leak via "Nearby Entities
  Properties: ..." lines).
- **`apply_effects_to_target`** in main.rs: REJECTS any
  LLM-emit effect whose key matches an internal
  property (with a warning like `"Skipped effect on
  internal/operator-only property '<name>' (entity=
  '<target>', reason='LLM-emit effects cannot write
  to internal properties; use the per-property PUT
  endpoint to override')"`).  The LLM cannot
  accidentally (or maliciously) tamper with these
  bookkeeping values.
- **Per-property PUT endpoint**
  (`/api/entities/:id/properties/int/:key` etc.):
  operator-only.  IS allowed to write internal
  properties, so the operator can manually override the
  marker (e.g. reset `last_processed_other_tick` to 0
  to force reprocessing).  This is the documented
  escape hatch for ops.

### Adding a new internal property

The workflow is:

1. Add the property name to the appropriate const
   slice (`LLM_INTERNAL_INT_PROPERTIES`,
   `LLM_INTERNAL_FLOAT_PROPERTIES`, or
   `LLM_INTERNAL_STRING_PROPERTIES`) in
   `src/world_data/internal_properties.rs`.
2. Add a doc-comment to the entry explaining what the
   property does and who writes it.
3. Update this docs section (§ 5c) to keep the docs
   in sync.

No code change to the LLM-facing builders or the
effect-applier is needed — they iterate over the const
slices.  Same for any future code path that needs to
filter internal properties: import the const slice
and check membership.

## 12. Auto-Tag Rules (Hidden from the LLM)

These rules run inside `apply_all_effects` after every effect
write. They are intentionally **not** exposed to the LLM in the
prompt — the LLM shouldn't be reasoning about the formula, just
narrating. The operator (and anyone reading this doc) sees the
mechanics; the LLM sees only the post-write state and the
`{property_context}` block.

### Hidden-tag rule (`update_hidden_tag`)

For every entity affected by an action (actor + cross-entity
targets), recompute:

  threshold = `max(10, power) / 10 + visibility`
  threshold <  0  → add the `hidden` tag
  threshold >= 1  → remove the `hidden` tag
  0 ≤ threshold < 1  → no change (small dead zone, prevents
                          flicker at the boundary)

Returns `(added, removed, threshold)` and emits a warning per
toggle so operators can see what changed. The threshold and the
"max(10, power)" floor are tuned so power-0 entities still get a
small baseline; a power-1 entity has a budget of 2; a power-10
entity has 11; a power-100 entity has 60. The +1 cap on the
multiplier (instead of a `+10` floor) keeps brand-new entities
from flickering on/off the tag due to noisy ±1 visibility
writes.

### Corrupted-tag rule (`update_corrupted_tag`)

Mirror of the hidden-tag rule, but for the `corruption` property:

  threshold = `max(1, power) - corruption`
  threshold <  0  → add the `corrupted` tag
  threshold >= 1  → remove the `corrupted` tag
  0 ≤ threshold < 1  → no change (dead zone)

Both rules share the same `affected_ids` set inside
`apply_all_effects`, so they run on the same set of entities in
the same call. They use independent formulas and are independent
in practice (a single action can toggle both tags on the same
entity, e.g. an entity that goes into hiding AND gets corrupted
at the same time).

### Stats-cap rule (`check_stats_cap_warn`)

For every entity affected by an action, recompute:

  cap = `max(STATS_CAP_POWER_FLOOR, power * STATS_CAP_POWER_MULTIPLIER) + STATS_CAP_BASE`
  sum = signed sum of all `properties_int` values, EXCLUDING
         any property in § 5c's internal-properties list
         (currently just `last_processed_other_tick`).  The
         marker is a bookkeeping counter, not part of the
         entity's "stuff", and counting it would inflate
         every entity's over-cap warning by ~3000-4000
         (the marker's current range).
  sum > cap  → emit a warning, do NOT normalize

The runtime path only **warns** (it does not normalize), so a
single big effect doesn't silently shrink an entity mid-action.
The standalone `open-world-selena/code/normalize_stats.py` script
(moved here from `selena-project/code/normalize_stats.py` per
Arcurus 2026-06-08 #openworld "`code` that changes stats in
open world should clearly be in open world project") does the
actual proportional scaling when the operator wants to fix the
over-cap entities.  Run `preview` first to see the planned
deltas, then `normalize --yes` to apply.  The script also has
a `selftest` subcommand that doesn't talk to the API and
verifies the env-var override works.

The cap formula and the internal-properties filter are the
**same on both sides**: the Rust binary and the Python
script both read the env vars
`OPENWORLD_STATS_CAP_MULTIPLIER` /
`OPENWORLD_STATS_CAP_BASE` / `OPENWORLD_STATS_CAP_FLOOR` and
both fetch the internal-properties list from the
`/api/internal-properties` endpoint.  An `export
OPENWORLD_STATS_CAP_MULTIPLIER=15` in the shell covers both
sides.

The cap is keyed on the **original** (pre-normalize) power of
the entity, and `normalize_entity_stats` scales `power` along
with every other value (because `power` counts in the sum). The
post-scaling sum exactly matches the old cap, so no death
spiral.
