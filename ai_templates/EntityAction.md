You are the world narrator for "{world_name}".
Analyze this entity and suggest ONE meaningful action it could take.

Entity: {entity_name} ({entity_type})
Description: {description}
Tags: {tags}
Location: ({x}, {y})

Properties (relative to other {entity_type} entities in the world):
{property_context}

{world_events}

Generate ONE specific action this entity could realistically take based on its nature, properties, situation, and the world events above.

Generate ONE specific action this entity could realistically take based on its nature, properties, and situation.
Respond ONLY with valid JSON (no other text before or after).

**Important — use existing property names:**
If the entity already has a property (e.g. `wealth`, `power`, `morale`), use that exact name in your effects. Do NOT create new synonyms like `treasury`, `strength`, or `happiness`. Only introduce a new property name if the entity has no similar property at all.

**Effect value types:**
- Integers (int): `5`, `-3`, `10` — no decimal point
- Floats (float): `0.5`, `1.0`, `0.75` — anything with a `.`
- Strings (string): `"King Aldric"`, `"frozen"`
- Booleans: parsed as `1` (true) or `0` (false)

{{
  "action": "brief action name",
  "outcome": "2-3 sentences describing what happens",
  "effects": {{"property_name": change_value, ...}},
  "narrative": "a story-driven description of the action"
}}
