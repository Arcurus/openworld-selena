# Property Catalog (Operator Reference)

> **Audience:** operators. **NOT loaded into the LLM prompt.** This file is the source of truth for the operator; the LLM-facing dictionary is [`ai_templates/property_docs.md`](../ai_templates/property_docs.md) and the two are intentionally separate (per the design rule: property dictionary = LLM, mechanics catalog = operator).
>
> **Per-property format:**
> - **Summary** — 1-2 sentences, what the property is.
> - **Impact mechanics** — 2-3 sentences, how the property affects the world (formulas, tag rules, surface area in the prompt, where the cap or selection reads it).
> - **LLM dictionary** — pointer to the LLM-facing entry where the narrator learns the schema.
> - **See also** — pointers to the full operator-side reference in [`world-mechanics.md`](world-mechanics.md) and any related code.

---

## Named properties (story signals + generic stats)

### `visibility` (int, signed)

- **Summary:** Per-entity `i64` in `properties_int`. How "seen" or "present" the entity is in the world right now. Negative = hiding, positive = exposing, 0 = neutral.
- **Impact mechanics:** Feeds the nearby-entity influence score: `score = max(1, power + visibility) / distance`. Positive visibility lifts the entity into neighbours' view; negative pulls it down. **Not** used by the action selector (`pick_entities_weighted` only consults `power` + `idle_seconds` + de-prio tags) — visibility is a *display* / *influence* signal, not a *pick-eligibility* signal.
- **LLM dictionary:** [`visibility`](../ai_templates/property_docs.md#visibility-story-signal)
- **See also:** [world-mechanics.md § 4 — The `visibility` Property](world-mechanics.md#4-the-visibility-property) (the deepest operator reference for any property, 80+ lines, including the four-tier semantics — hide / 0 / expose / super-positive `> 500`).

### `corruption` (int, signed)

- **Summary:** Per-entity `i64`. How far the entity has slipped from its original nature. Positive = corrupted, negative = purified, 0 = neutral. Corrupted entities are typically very selfish and spread further corruption, whether consciously or unconsciously.
- **Impact mechanics:** Triggers the auto-tag rule (see [Auto-tag rules](#auto-tag-rules-hidden-from-the-llm) below): when `max(1, power) - corruption < 0` the entity gets the `corrupted` tag. Otherwise purely a story-signal property — no selection or cap formula reads it.
- **LLM dictionary:** [`corruption`](../ai_templates/property_docs.md#corruption-story-signal)
- **See also:** [world-mechanics.md § 12 — Auto-Tag Rules](world-mechanics.md#12-auto-tag-rules-hidden-from-the-llm).

### `influence` (int, signed)

- **Summary:** Per-entity `i64`. Political and social leverage — the ability to sway decisions, broker deals, rally others. Soft power, distinct from `power` (general) and `reputation` (how the world sees the entity).
- **Impact mechanics:** Purely a story-signal property today — no formula, no selection, no auto-tag rule references it. (Future mechanic per Arcurus 2026-06-08: high `influence` may also raise `power`, but the coupling is **not** implemented yet and is **not** in the LLM dictionary.) Used in the LLM's `nearby_entities` block as a sortable column, so the LLM sees it as a number-to-compare, not as a formula input.
- **LLM dictionary:** [`influence`](../ai_templates/property_docs.md#influence-story-signal)
- **See also:** [world-mechanics.md § 3 — Nearby Entities (LLM context)](world-mechanics.md#3-nearby-entities-llm-context) (influence is one of the top-5-by-influence sort keys in the nearby list).

### `suspicion` (int, signed)

- **Summary:** Per-entity `i64`. How suspected the entity is of wrongdoing. Positive = world is watching them, negative = seen as above reproach. Perception, not reality.
- **Impact mechanics:** Triggers the auto-tag rule (see below): when `max(1, power) - suspicion < -1` the entity gets the `suspicious` tag. Otherwise purely a story-signal. The LLM can deliberately shape `suspicion` based on entity behaviour: hidden corruption → suspicion moves down, defiant power → suspicion can legitimately soar.
- **LLM dictionary:** [`suspicion`](../ai_templates/property_docs.md#suspicion-story-signal)
- **See also:** [world-mechanics.md § 12 — Auto-Tag Rules](world-mechanics.md#12-auto-tag-rules-hidden-from-the-llm).

### `power` (int, signed but always non-negative in practice)

- **Summary:** Per-entity `i64`. **Required for every entity.** How powerful the entity is, considering all of its strengths — military, magical, economic, political, anything that gives the entity weight in the world.
- **Impact mechanics:** The single most-load-bearing property. Feeds the **stats cap** (`cap = max(1, base) + max(1, power) * multiplier`, env-var configurable via `OPENWORLD_STATS_CAP_*` — default `base=100`, `multiplier=10`, both shared with the Python [`code/normalize_stats.py`](../code/normalize_stats.py) script). Feeds the **action selector** (`pick_entities_weighted` uses `(power + 1) * seconds_idle` as the selection weight). Feeds the **nearby-entity score** (`score = max(1, power + visibility) / distance`). Feeds the **tag-rule formulas** (every tag rule uses `max(1, power)` or `max(10, power)` as the denominator).
- **LLM dictionary:** [`power`](../ai_templates/property_docs.md#common-generic-properties)
- **See also:** [world-mechanics.md § 6 — Effect Normalization](world-mechanics.md#6-effect-normalization), [§ 6b — Shared source of truth with the Python CLI](world-mechanics.md#6b-shared-source-of-truth-with-the-python-cli), [`code/normalize_stats.py`](../code/normalize_stats.py).

### `wealth` (int, signed but always non-negative in practice)

- **Summary:** Per-entity `i64`. Money, treasure, material resources. Applies to factions, merchants, kingdoms, villages, individual characters — anyone with a treasury or purse.
- **Impact mechanics:** Purely a story-signal property today — no formula, no selection, no tag rule. Merchants and certain guilds care a great deal about it (their survival and power often ride on it).
- **LLM dictionary:** [`wealth`](../ai_templates/property_docs.md#common-generic-properties)

### `morale` (int, signed)

- **Summary:** Per-entity `i64`. Fighting spirit, hope, determination. Negative = despair, very positive = eager / fanatical.
- **Impact mechanics:** Purely a story-signal property today — no formula or selection effect.

### `knowledge` (int, signed)

- **Summary:** Per-entity `i64`. Accumulated lore, secrets learned, lost truths recovered.
- **Impact mechanics:** Purely a story-signal property today — no formula or selection effect.

### `reputation` (int, signed)

- **Summary:** Per-entity `i64`. How the world sees this entity. Longer-term "what do people think of them" — distinct from `visibility` (present-tense "are they here").
- **Impact mechanics:** Purely a story-signal property today — no formula or selection effect.

---

## Auto-tag rules (hidden from the LLM)

These rules run inside `apply_all_effects` after every effect write. They are **not** exposed to the LLM in the prompt — the LLM shouldn't be reasoning about the formula, just narrating. The operator sees the mechanics here; the LLM sees only the post-write state.

| Rule | Formula | Add tag when | Remove tag when | Dead zone |
|---|---|---|---|---|
| **Hidden** | `threshold = max(10, power) / 10 + visibility` | `threshold < 0` | `threshold >= 1` | `[0, 1)` (no change) |
| **Corrupted** | `threshold = max(1, power) - corruption` | `threshold < 0` | `threshold >= 1` | `[0, 1)` (no change) |
| **Suspicious** | `threshold = max(1, power) - suspicion` | `threshold < -1` | `threshold >= 0` | `[-1, 0]` (no change) |
| **Stats-cap warning** (no tag) | `cap = max(1, base) + max(1, power) * multiplier`; `sum = signed sum of non-internal properties_int` | `sum > cap` → emit warning | n/a (warning, not a state) | n/a |

The three tag rules share the same `affected_ids` set in `apply_all_effects` so they run on the same entities in the same call. A single action can toggle all three on the same entity independently (e.g. an entity that goes into hiding AND gets corrupted AND gets publicly suspected at the same time).

The dead zones prevent flicker at the boundary (noisy ±1 writes won't ping-pong the tag). A high-`power` entity needs *more* of the relevant property to trigger the tag (e.g. a power-100 entity needs `corruption > 101` to be tagged; a power-10 entity only needs `corruption > 11`).

**Full implementation:** [world-mechanics.md § 12 — Auto-Tag Rules](world-mechanics.md#12-auto-tag-rules-hidden-from-the-llm), [`src/main.rs` `update_hidden_tag` / `update_corrupted_tag` / `check_stats_cap_warn`](../src/main.rs).

---

## Internal / bookkeeping properties (operator-only)

The LLM doesn't know about these. They don't surface in the action prompt (the property-filter at the context-builder level strips them) and they don't count toward the stats cap (the `/api/internal-properties` endpoint returns the canonical list of these and both the Rust runtime + the Python [`normalize_stats.py`](../code/normalize_stats.py) script filter them out). All are `int`.

| Property | What it tracks | Where it surfaces |
|---|---|---|
| `last_processed_other_tick` | `action_count` at the time the entity's "unprocessed other entities" history was last updated; drives the rolling-history window | Stripped by the property-filter; filtered out of the cap sum |
| `hour` | World-clock hour (0-23), kept in sync with the `world_clock` entity | Read-only by the LLM via `{property_context}` so it knows the time of day |
| `day` | World-clock day counter (monotonic) | Read-only by the LLM via `{property_context}` |
| `total_years` | World-clock total years elapsed (float cast to int) | Read-only by the LLM via `{property_context}` |
| `is_recording` | Boolean (0/1): is this entity currently being recorded for history? | Operator-side; consumed by `format_history_for_llm` |
| `has_history` | Boolean (0/1): does the entity have at least one history entry? | Operator-side; same consumer as `is_recording` |
| `history_entries` | Counter: number of history entries on the entity | Operator-side; same consumer as `is_recording` |
| `actions_today` | Per-day action counter, reset at the day boundary | Surfaced in the LLM's `{property_context}` (relative-vs-peers comparison); not a story signal, it's a rate-limit / observability metric |

**Canonical list:** [`GET /api/internal-properties`](../src/main.rs) (returns the current set as JSON, split by type `int` / `float` / `string`). The Rust binary owns the list (`src/world_data/internal_properties.rs`); the Python script fetches it at startup. When adding a new internal property, add it to **both** the Rust const slice AND the API handler — see [world-mechanics.md § 6b — Shared source of truth](world-mechanics.md#6b-shared-source-of-truth-with-the-python-cli) for the full rule.

**See also:** [world-mechanics.md § 5b — Unprocessed World Actions from Other Entities](world-mechanics.md#5b-unprocessed-world-actions-from-other-entities-llm-context) for the rolling-history mechanic that `last_processed_other_tick` drives.
