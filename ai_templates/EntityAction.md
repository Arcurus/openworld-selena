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
  "history_summary_replace": {{"old_part": "current text to change", "new_part": "what to change it to"}}
}}

**`history_summary` rules (always include, every turn):**
- ≤ {max_history_summary_chars} characters total (hard cap; will be truncated server-side if exceeded).
- A rolling one-paragraph arc: what this entity has been doing recently, why, and where it's heading.
- Your summary can reference any past action if it matters for narrative continuity. Space is limited, so the impact of very old actions may be dropped if it won't fit in the summary. You decide what to keep.
- Update it to reflect the new action you're taking. Don't just repeat the prior summary.
- Mention the count or cadence of any dominant pattern you notice (e.g. "5th temple bell toll this week — leaning into ritual").
- **Keep track of relations (1-3 sentences *per relation*):** for each recently-interacted entity, write **1-3 sentences** covering who you met, what you exchanged, how the relationship shifted, and whether it's an ally / rival / debt / unknown. Format as separate short lines per relation (e.g. `→ Mira the Scribe: …`) — **one line per entity, never two**. If you've already mentioned an entity in a previous summary, update that existing line in place; do not add a second `→ X: …` for the same entity. You may have several relations in the summary; the 1-3-sentence budget applies to *each* one, not the total. Drop stale relations to make room for new ones.
- Keep it forward-looking: the next call's LLM will read this to plan the next action.
- Do NOT include the action name of the current turn as if it already happened; write as if it's about to happen or has just been initiated.
- **`history_summary_replace` (surgical edits, optional):** if you only need to change a small part of the current summary, use this INSTEAD of `history_summary` — you only send the diff, which is much cheaper on tokens. Value is one `{old_part, new_part}` object, or an array of such objects. Each pair replaces the first occurrence of `old_part` with `new_part` in the current summary, in order. An empty `old_part` (`""`) means "append `new_part` to the end". Result is truncated to {max_history_summary_chars} chars if needed.

  **Pick the cheaper one — never send both:** if the change is small (a status update, a single phrase, a new relation line), use `history_summary_replace` alone. If most of the summary needs rewriting (new arc, new chapter, multiple relations shifting), use `history_summary` alone. Sending both wastes tokens because only the replace is applied and the full `history_summary` is dropped.

  Examples:
  - `history_summary_replace: {{"old_part": "met Mira at the gate", "new_part": "reunited with Mira in the capital"}}`
  - `history_summary_replace: [{{"old_part": "former ally of the elves", "new_part": "open conflict with the elves"}}, {{"old_part": "", "new_part": " (recent: dragon sighted)"}}]`
  - `history_summary_replace: {{"old_part": "", "new_part": "World Clock ticks. Era continues."}}`
</content>
</invoke>