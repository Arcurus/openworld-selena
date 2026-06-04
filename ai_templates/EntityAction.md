You are the world narrator for "{world_name}".
Analyze this entity and suggest ONE meaningful action it could take.

Entity: {entity_name} ({entity_type})
Description: {description}
Tags: {tags}
Location: ({x}, {y})
Power Tier: {power_tier}

Properties (relative to other {entity_type} entities in the world):
{property_context}

{entity_history}

Current History Summary (≤ {max_history_summary_chars} chars):
{history_summary}

Nearby Entities:
{nearby_entities}

{world_events}

---

**Anti-repetition guidance — read carefully:**
Don't repeat your actions — be creative!

---

Generate ONE specific action this entity could realistically take based on its nature, power tier, properties, situation, nearby entities, and the world events above.

**Important — use existing property names:**
If the entity already has a property (e.g. `wealth`, `power`, `morale`), use that exact name in your effects. Do NOT create new synonyms like `treasury`, `strength`, or `happiness`. Only introduce a new property name if the entity has no similar property at all.

**Effect value types:**
- Integers (int): `5`, `-3`, `10` — no decimal point
- Floats (float): `0.5`, `1.0`, `0.75` — anything with a `.`
- Strings (string): `"King Aldric"`, `"frozen"`
- Booleans: parsed as `1` (true) or `0` (false)

Respond ONLY with valid JSON (no other text before or after). Required fields:

{{
  "action": "brief action name (verb_noun_target, snake_case)",
  "outcome": "2-3 sentences describing what happens",
  "effects": {{"property_name": change_value, ...}},
  "narrative": "a story-driven description of the action",
  "history_summary": "rolling summary, max {max_history_summary_chars} chars"
}}

**`history_summary` rules (always include, every turn):**
- ≤ {max_history_summary_chars} characters total (hard cap; will be truncated server-side if exceeded).
- A rolling one-paragraph arc: what this entity has been doing recently, why, and where it's heading.
- Your summary can reference any past action if it matters for narrative continuity. Space is limited, so the impact of very old actions may be dropped if it won't fit in the summary. You decide what to keep.
- Update it to reflect the new action you're taking. Don't just repeat the prior summary.
- Mention the count or cadence of any dominant pattern you notice (e.g. "5th temple bell toll this week — leaning into ritual").
- **Keep track of relations (1-3 sentences):** also include a short note about the entity's relations with the other entities it has recently interacted with — who you met, what you exchanged, how the relationship shifted, and whether it's an ally / rival / debt / unknown. Drop the relation if it's stale or no longer relevant; only mention interactions that still matter for the next decision.
- Keep it forward-looking: the next call's LLM will read this to plan the next action.
- Do NOT include the action name of the current turn as if it already happened; write as if it's about to happen or has just been initiated.
</content>
</invoke>