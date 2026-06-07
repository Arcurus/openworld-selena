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

{history_summary_header}
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

**Multi-entity effects (key format `entityname.property_name`):**
- For effects on the **actor** (yourself), prefix with `self.`: e.g. `self.morale: +5`, `self.power: -2`.
- For effects on **other entities** the action impacts, use their exact name (as it appears in the world) followed by `.property_name`: e.g. `Mira the Merchant.wealth: -3`, `The Sunken Temple.magical_activity: +2`.
- The server resolves the name to the entity, and for now **DRY-RUNS the cross-entity writes** — it parses them, logs what would have happened, and applies only your self-effects. You'll see the dry-run result in the warnings so you can verify the routing next turn.
- **Emit at least one effect per entity you impact in the action** (including yourself), so each entity's history can track the ripple. If your `narrative` mentions a specific entity being affected, that entity should appear in your `effects`.
- Property names on the right-hand side of the dot still follow the "use existing property names" rule above.

**`visibility` (story signal only):**
- A high `visibility` means this entity is *exposed* — it stands out, its presence is felt, and other entities will be very aware of it.
- A negative `visibility` means the entity is *hiding* — withdrawn, concealed, easy to overlook. Other entities will be less aware of it.
- Use it as a narrative cue: how present or absent does this entity feel in the world right now?

Respond ONLY with valid JSON (no other text before or after). **Always include a `history_summary_replace` field — it is the ONLY way to update the entity's rolling history summary.** See the rules section below for how to use it.

{{
  "action": "brief action name (verb_noun_target, snake_case)",
  "outcome": "2-3 sentences describing what happens",
  "effects": {{"entityname.property_name": change_value, ...}},
  "narrative": "a story-driven description of the action",
  "history_summary_replace": {{"old_part": "current text to change", "new_part": "what to change it to"}}
}}

For `effects`, use `self.property_name` for the actor and `other_entity_name.property_name` for any entity the action impacts. Example:

```json
"effects": {
  "self.power": +3,
  "self.morale": +1,
  "Mira the Merchant.wealth": -2,
  "The Sunken Temple.magical_activity": +1
}
```

**History summary budget (look at the `Current History Summary` header above for the live values):**
- **Cap:** {max_history_summary_chars} characters total (hard cap; will be truncated server-side if exceeded).
- **Used / free** is reported in the header above as `used N, F free` (or `OVER by N` if a prior turn wrote too long and the server hasn't truncated yet — in that case your next edit should trim, ideally with a `!ALL!` rewrite).
- Plan your `history_summary_replace` so the *result* (current summary + your edits) stays under the cap.

**Content rules for what the summary should track:**
- A rolling one-paragraph arc: what this entity has been doing recently, why, and where it's heading.
- Your summary can reference any past action if it matters for narrative continuity. Space is limited, so the impact of very old actions may be dropped if it won't fit in the summary. You decide what to keep.
- Update it to reflect the new action you're taking. Don't just repeat the prior summary.
- Mention the count or cadence of any dominant pattern you notice (e.g. "5th temple bell toll this week — leaning into ritual").
- **Keep track of relations (2-4 sentences *per relation*, one entry per entity):** for each recently-interacted entity, write 2-4 dense sentences covering who you met, what you exchanged, how the relationship shifted, and whether it's an ally / rival / debt / unknown. Format as separate short lines (e.g. `→ Mira the Scribe: …`). One entry per entity — never duplicate an entity you've already mentioned. If a relation evolves, **update the existing line in place** with the new state (rewriting it, not appending a second `→ X: …`). You may have several relations in the summary; the 2-4-sentence budget applies to *each* one, not the total. Drop stale relations to make room for new ones.
- Keep it forward-looking: the next call's LLM will read this to plan the next action.
- **The action you are taking this turn has already happened** — your `action` / `outcome` / `effects` / `narrative` fields are the action, and they get applied server-side as part of the same request. So the `history_summary_replace` should record what just happened, in past tense ("After Velora did X, …", "Just now: Vaelthrix stirred because …"). The next LLM call reads this to plan the next action, so write it as a done thing, not a forecast.

**How to use `history_summary_replace` (the only way to update the summary):**

- **Surgical edit (preferred for most turns).** Value is one `{old_part, new_part}` object, or an array of such objects. Each pair replaces the first occurrence of `old_part` with `new_part` in the **current stored** summary (NOT in your own draft), in order. An empty `old_part` (`""`) means "append `new_part` to the end". Result is truncated to {max_history_summary_chars} chars if needed.

  - **Prefer small targeted edits when possible** (single-line corrections, relation updates, status changes) — those are the cheap case.
  - **But the summary must always include everything important** for the next LLM call to plan the next action. If a small-edit preference would cause you to drop or omit important stuff, **do a bigger edit** — prune what's no longer relevant (stale relations, repeated routine actions) and consolidate the things that actually matter. **You choose what to keep.** If the summary needs a structural change (new arc, new chapter, multiple relations shifting at once), use the `!ALL!` convention below.
  - To do a full replace, set `old_part` to the literal string `"!ALL!"` and put the new full summary in `new_part`. **This is also the only way to set the initial summary** when the entity doesn't have one yet (`"!ALL!"` on an empty summary is a no-op, but you can pass the full text as `new_part`).

  Examples:
  - Surgical edit: `history_summary_replace: {{"old_part": "met Mira at the gate", "new_part": "reunited with Mira in the capital"}}`
  - Multiple edits in one turn: `history_summary_replace: [{{"old_part": "former ally of the elves", "new_part": "open conflict with the elves"}}, {{"old_part": "", "new_part": " (recent: dragon sighted)"}}]`
  - Append a tail line: `history_summary_replace: {{"old_part": "", "new_part": "World Clock ticks. Era continues."}}`
  - Full restructure / first write: `history_summary_replace: {{"old_part": "!ALL!", "new_part": "<complete new summary here, same length budget as history_summary>"}}`
