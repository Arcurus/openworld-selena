# Open World — Mechanics

*Authoritative reference for how the world runs day-to-day: action
selection, history formatting, log files, auth, persistence, and the
relationship to the Python scheduler. For LLM context shape, see
[`llm-context.md`](./llm-context.md); for entity roster, see
[`world_entities.md`](./world_entities.md); for world events, see
[`world_events.md`](./world_events.md).*

**Last updated:** 2026-06-06 (added §7 Multi-Entity Effects — dotted-key
`entityname.property_name` schema, dry-run for cross-entity writes;
self-effect expansion; per-effect routing report. After the
meta-selector + history-format + nearby-entity split + visibility-doc
+ history-budget-bump + effect-normalization rewrites).

---

## 1. Project Topology

The world is split across **two cooperating services**:

| Service | Language | Role | Where it lives |
|---|---|---|---|
| `open-world-selena` | Rust (axum) | The world itself — entity CRUD, LLM action emission, binary persistence, HTTP API on `:8081` | `~/openclaw/workspace/open-world-selena/` |
| `selena-project` | Python (api_server) | The **scheduler** — picks which entity to act on every 120s, calls the world's API to trigger an action cycle. Also serves the web UI on `:8765`. | `~/openclaw/workspace/selena-project/` |

The Rust world has **no internal tick loop**. Every LLM-driven action
is triggered by an external HTTP call (the Python scheduler, or a
manual API hit from the user).

**Auth boundary.** The Rust world uses a single hardcoded bypass
cookie (`openworld_auth=1`) for write endpoints — local-only, not
real auth. The Python scheduler sets this cookie on every call.

---

## 2. Action Selection

The scheduler picks which entity to act on next using a weighted
sample. **Selector code:** `selena-project/code/scheduled_actions.py`
→ `pick_entities_weighted()` and `entity_deprio_multiplier()`.

### Formula

```
weight(entity) = max(MIN_SELECTION_POWER, entity.power)
               × entity.idle_seconds
               × entity_deprio_multiplier(entity)
```

with `entity_deprio_multiplier` returning the product of all matching
de-prio tag multipliers (multiple tags compose multiplicatively).

### Constants (tunable in `scheduled_actions.py`)

| Constant | Value | Meaning |
|---|---:|---|
| `MIN_SELECTION_POWER` | `10` | Floor on `power` so zero/weak entities still get a baseline chance to be picked. Replaces the older `(power + 1)` safety floor. |
| `DEPRIO_TAG_MULTIPLIERS["sleeping"]` | `0.01` | Sleeping entities are picked 100× less often than awake peers. |
| `DEPRIO_TAG_MULTIPLIERS["meta"]` | `0.01` | Internal/meta entities (e.g. World Clock) are picked 100× less often. |
| `interval_seconds` | `120` (default) | Seconds between scheduler cycles. |
| `actions_per_cycle` | `1` (default) | Entities picked per cycle. |

`DEPRIO_TAG_MULTIPLIERS` is a dict — adding a third de-prio tag is a
one-line change. Multipliers compose multiplicatively (an entity with
both `sleeping` and `meta` would be `0.01 × 0.01 = 0.0001×`).

### Sleeping tag convention

- **No auto-apply.** The `sleeping` tag is **not** auto-applied to
  legendary entities. Selena applies it manually when Arcurus
  instructs.
- **Currently tagged `sleeping`:** Vaelthrix the Endless (dragon,
  legendary), Velora the Undying (hero, legendary).

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

---

## 3. Nearby Entities (LLM context)

The "Nearby Entities" block the LLM sees lists other entities within
**150 units** of the subject, **split into three groups**.
Locations and Characters are **sorted by influence score**;
Factions are **sorted by distance ascending** (nearest first)
and **capped at MAX_NEARBY_FACTIONS (5)**.

**Code:** `build_nearby_entities_str` in
`src/world_data/context_builder.rs`.

### Output shape

```
Nearby Entities:
### Nearby Locations
- **Shadow Ridge Camp** (location) — dist 85.4, power 68, visibility 29, score 1.14
  Hidden bandit encampment.
  Properties: visibility: 29, power: 68, wealth: 19
- **The Sunken Temple** (location) — dist 86.0, power 0, visibility 0, score 0.01
  ...
  Properties: magical_activity: 0, consciousness_active: 0, visibility: 0

### Nearby Characters
- **Mira the Merchant** (character) — dist 120.8, power 20, visibility 12, score 0.26
  Traveling merchant with exotic goods.
  Properties: knowledge: 22, magic_protection: 78, power: 20

### Nearby Factions (5 nearest)
- **Keepers of the Eternal Flame** (faction) — dist 92.3, power 138, visibility 80, score 2.3625
  Ancient order guarding the balance between light and shadow.
  Properties: power: 138, visibility: 80, mana_reserves: 450
- **Ironforge Clan** (faction) — dist 128.1, power 194, visibility 0, score 1.5144
  Dwarven smiths who forge weapons for the realm.
  Properties: power: 194, smithing_skill: 220, ore_reserves: 800
```

### The split

- **Locations** — `entity_type == "location"`. Physical places the
  subject can visit.
- **Characters** — everything else except `location` and `faction`:
  `character`, `hero`, `oracle`, `dragon`, `artifact`,
  `world_clock`, etc. All agent-like / interactive individuals and
  items grouped together.
- **Factions** — `entity_type == "faction"`. Organised groups
  (orders, clans, guilds) pulled out into their own section so the
  LLM can reason about them as collective actors. Capped at 5
  nearest, sorted by distance ascending. If more categories are
  needed, they go in their own `### Nearby …` section.

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
- Other `location` entities that don't appear in radius 150.
- Entities at the exact same coords as the subject (zero distance
  is skipped to avoid divide-by-zero).

### Tunable constants

| Constant | File | Default | Effect of changing |
|---|---|---|---|
| `150.0` (radius) | `build_nearby_entities_str` | 150 | Wider radius → more entities to consider, larger LLM context. |
| `max(1, power + visibility)` floor | `build_nearby_entities_str` | `1` | Lower → distance dominates; higher (e.g. `10`) → power/visibility matter more. |
| Sleeping multiplier | `build_nearby_entities_str` | `0.01` | Higher → sleeping entities compete more with awake peers. Should stay aligned with `DEPRIO_TAG_MULTIPLIERS["sleeping"]` in `scheduled_actions.py`. |
| Number of int props shown in each entry | `format_nearby_entry` (`.take(3)`) | 3 | Higher → more props, larger context. |
| `entity_type` bucketing | `build_nearby_entities_str` | `"location"` and `"faction"` get their own `### Nearby …` sections; everything else stays in Characters | Add a new bucket: create a `match` arm, a `Vec`, a sort, a truncate, and a render block. |
| `MAX_NEARBY_FACTIONS` | `build_nearby_entities_str` (constant) | `5` | Higher → more factions in the LLM prompt per action. Factions are capped because they tend to be the most context-heavy entries (richer descriptions, larger properties). |

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

- **Protected entities** (system entities with `world_clock` or
  `meta` tags): all effects are blocked entirely before this
  pre-pass. The scale is 1.0 for them.
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

`ow_scheduler_config.json` is read **live** every cycle, so changes
take effect on the next cycle (no service restart needed).
`scheduled_actions.py` and Rust files (`entity_history.rs`,
`main.rs`, `context_builder.rs`) require a service restart.
