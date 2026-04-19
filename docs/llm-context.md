# LLM Action Context Documentation

**Last Updated:** 2026-04-19 08:52 CEST
**File:** `ai_templates/EntityAction.md`

---

## Currently IN LLM Action Context

| Feature | Template Variable | Status | Description |
|---------|-----------------|--------|-------------|
| World name | `{world_name}` | ✅ Implemented | Name of the world simulation |
| Entity info | `{entity_name}`, `{entity_type}`, `{description}`, `{tags}`, `{x}`, `{y}` | ✅ Implemented | Basic entity identification and location |
| Power tier | `{power_tier}` | ✅ Implemented | Power classification (e.g., "Low", "Medium", "High", "Legendary") |
| Property context | `{property_context}` | ✅ Implemented | Entity properties relative to other entities of same type |
| Entity history | `{entity_history}` | ✅ Implemented | Past actions and events for this entity |
| Nearby entities | `{nearby_entities}` | ✅ Implemented | Other entities within proximity (150 units) |
| World events | `{world_events}` | ✅ Implemented | Active world events affecting the simulation |
| Faction context | `{faction_context}` | ❌ NOT in Context | Faction relationships (ally/enemy/neutral) |
| Custom relationships | `{relationships}` | ❌ NOT in Context | Entity-specific relationships |
| Power-based proximity | `power / (distance + 1)` | ❌ NOT in Context | Nearby entities weighted by power and distance |

---

## Currently NOT IN LLM Action Context

| Feature | Priority | Notes |
|---------|----------|-------|
| Faction relationships (ally/enemy/neutral) | HIGH | Need to add faction context to prompts |
| Entity-specific relationships | MEDIUM | Relationships stored separately |
| Power-weighted nearby calculation | MEDIUM | Use `power / (distance + 1)` formula |
| Multi-target action support | LOW | LLM can already target other entities by name |

---

## Implementation Notes

### Nearby Entities Formula (TODO)
For nearby entities, use:
```
relevance_score = power / (distance + 1)
```
This weights powerful entities more heavily when they're closer.

### Relationship Storage (TODO)
- Faction relationships: stored in `relationships.bin`
- Entity relationships: stored in `entity_relationships.bin`
- Both loaded separately and merged at runtime

### World Events as Entities (TODO)
World events should be wrapped as entities internally:
- Wrapper class maps world events → WorldEntity
- External API still returns them as world events
- Internal context uses full entity representation

---

## Template (Current)

```markdown
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

Nearby Entities:
{nearby_entities}

{world_events}

Generate ONE specific action this entity could realistically take...
```

---

## Questions to Resolve

1. **Why two build_context functions?** - Need to investigate and potentially consolidate
2. **Should we keep entity history in memory or load from save?** - Currently in memory only
3. **How to handle world event cleanup?** - Events should expire after certain conditions
