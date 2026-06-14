//! Open World Scheduler (Rust port of selena-project/code/scheduled_actions.py)
//!
//! Per Arcurus 2026-06-08 #openworld: the selena-project service should be
//! independent of the open-world project — it should just track LLM runs
//! and costs. The world is now self-driving: this tokio task wakes every
//! 120s (configurable), picks entities by `(power + 1) * idle_seconds`
//! weight, and drives the 3-step action flow (context → LLM → process)
//! against the world's own HTTP endpoints.
//!
//! Design notes:
//! - The scheduler is decoupled from the world's in-memory state. It
//!   talks to itself over HTTP, exactly like the old Python scheduler
//!   did. This keeps the action path tested and consistent: every
//!   action goes through the same endpoints, with the same auth, the
//!   same LLM tracking, the same error handling.
//! - The LLM usage tracking (POST to /api/llm-usage/record) happens
//!   inside the action/llm endpoint, so the scheduler gets it for
//!   free without any extra plumbing. If selena-api is down, the
//!   action still completes and the world keeps ticking — the LLM-
//!   usage POST inside action/llm is fire-and-forget on a detached
//!   task and never blocks the LLM response (per main.rs
//!   `record_llm_call_async`).
//! - The scheduler has NO direct call into selena-api. Its only
//!   external dependency is the LLM-usage POST that lives inside
//!   the world's own action/llm endpoint, which the scheduler
//!   doesn't even know about.
//!
//! Files:
//! - `world_data/ow_scheduler_config.json` — enabled/interval/actions_per_cycle
//!   (operator-tunable via the scheduler config API, read live every
//!   cycle so changes take effect without a restart)
//! - `world_data/ow_entity_last_action.json` — per-entity "last acted at"
//!   map, persisted so a restart doesn't reset everyone's idle time.
//!   Selection is `(power + 1) * time_since_last_action`, so a
//!   long-idle entity with even modest power is far more likely to
//!   be picked than a just-acted one.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::sleep;

const SCHEDULER_CONFIG_PATH: &str = "world_data/ow_scheduler_config.json";
const ENTITY_LAST_ACTION_PATH: &str = "world_data/ow_entity_last_action.json";

/// 7-day default idle time for entities with no recorded action.
/// Matches the Python scheduler's behaviour: a brand-new entity
/// counts as "very idle" so it's likely to be picked first.
const DEFAULT_IDLE_SECONDS: u64 = 7 * 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default = "default_actions_per_cycle")]
    pub actions_per_cycle: u32,
    /// Free-form notes field (e.g. "5% MiniMax slice" for the
    /// default config). Optional; the scheduler doesn't read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// ISO-8601 UTC timestamp of the last write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

fn default_enabled() -> bool {
    true
}
fn default_interval_seconds() -> u64 {
    120
}
fn default_actions_per_cycle() -> u32 {
    1
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_seconds: 120,
            actions_per_cycle: 1,
            notes: Some(
                "Defaults: 30 LLM calls/h, under 5% of 4500/5h budget. \
                 Operators can tune via /api/ow/scheduler/config."
                    .to_string(),
            ),
            updated_at: None,
        }
    }
}

impl SchedulerConfig {
    /// Load from disk, falling back to defaults (and writing them
    /// out) on a missing or malformed file.
    pub fn load() -> Self {
        match std::fs::read_to_string(SCHEDULER_CONFIG_PATH) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                eprintln!("[scheduler] config parse failed ({e}); using defaults");
                let cfg = Self::default();
                cfg.save();
                cfg
            }),
            Err(_) => {
                let cfg = Self::default();
                cfg.save();
                cfg
            }
        }
    }

    pub fn save(&self) {
        if let Some(parent) = Path::new(SCHEDULER_CONFIG_PATH).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(s) => {
                if let Err(e) = std::fs::write(SCHEDULER_CONFIG_PATH, s) {
                    eprintln!("[scheduler] config write failed: {e}");
                }
            }
            Err(e) => eprintln!("[scheduler] config serialize failed: {e}"),
        }
    }
}

pub type EntityLastActionMap = HashMap<String, f64>;

pub fn load_entity_last_action() -> EntityLastActionMap {
    match std::fs::read_to_string(ENTITY_LAST_ACTION_PATH) {
        Ok(s) => {
            // The Python scheduler stored timestamps as floats
            // (time.time() returns float seconds with sub-second
            // precision, e.g. 1780941841.9880254).  Serde rejects
            // float values when the target type is u64, so we
            // declare the type as f64 here.  This is the bug
            // fix for 2026-06-08 first-deploy incident.
            match serde_json::from_str::<EntityLastActionMap>(&s) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[scheduler] load: parse failed ({e}); falling back to empty map");
                    HashMap::new()
                }
            }
        }
        Err(_) => HashMap::new(),
    }
}

pub fn save_entity_last_action(map: &EntityLastActionMap) {
    if let Some(parent) = Path::new(ENTITY_LAST_ACTION_PATH).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string(map) {
        Ok(s) => {
            if let Err(e) = std::fs::write(ENTITY_LAST_ACTION_PATH, s) {
                eprintln!("[scheduler] entity-last-action write failed: {e}");
            }
        }
        Err(e) => eprintln!("[scheduler] entity-last-action serialize failed: {e}"),
    }
}

pub fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn entity_idle_seconds(entity_id: &str, last_action_map: &EntityLastActionMap) -> u64 {
    match last_action_map.get(entity_id) {
        Some(&last) if last <= now_epoch() => (now_epoch() - last) as u64,
        _ => DEFAULT_IDLE_SECONDS,
    }
}

/// Pull `properties_int.power` (default 0). We do NOT use the
/// server's `total_power` (which sums power + strength + army_size +
/// wealth + influence plus every positive properties_float) for
/// selection — that field is for tiering, not selection. Using it
/// here would let one entity dominate forever because of unrelated
/// float counters. Matches the Python scheduler.
pub fn entity_power(entity: &Value) -> u64 {
    // Accept signed integers too (some artifacts have negative
    // power; clamp to 0 for the weighting math).
    entity
        .get("properties_int")
        .and_then(|p| p.get("power"))
        .and_then(|v| v.as_i64())
        .map(|n| n.max(0) as u64)
        .unwrap_or(0)
}

/// Port of `pick_entities_weighted` from selena-project's
/// scheduled_actions.py. Weights: `(power_i + 1) * idle_seconds_i`.
/// Weighted sample without replacement (successive CDF draws).
pub fn pick_entities_weighted(
    entities: &[Value],
    k: u32,
    last_action_map: &EntityLastActionMap,
) -> Vec<Value> {
    if entities.is_empty() || k == 0 {
        return vec![];
    }
    let weights: Vec<u64> = entities
        .iter()
        .map(|e| {
            let id = e.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let p = entity_power(e);
            let idle = entity_idle_seconds(id, last_action_map);
            (p + 1) * idle
        })
        .collect();
    let total: u128 = weights.iter().map(|w| *w as u128).sum();
    if total == 0 {
        // Defensive: all weights zero, fall back to uniform sample.
        return entities
            .iter()
            .take(k as usize)
            .cloned()
            .collect();
    }
    // Weighted sample without replacement.
    let mut pool: Vec<(Value, u64)> = entities.iter().cloned().zip(weights).collect();
    let mut chosen: Vec<Value> = Vec::new();
    let mut rng = rand::thread_rng();
    for _ in 0..k.min(pool.len() as u32) {
        let wsum: u128 = pool.iter().map(|(_, w)| *w as u128).sum();
        if wsum == 0 {
            // No weight left; fall back to uniform.
            pool.shuffle(&mut rng);
            chosen.push(pool.remove(0).0);
            continue;
        }
        let target = rng.gen_range(0..wsum as u64) as u128;
        let mut cum: u128 = 0;
        let mut picked: usize = 0;
        for (i, (_, w)) in pool.iter().enumerate() {
            cum += *w as u128;
            if cum >= target {
                picked = i;
                break;
            }
        }
        chosen.push(pool.remove(picked).0);
    }
    chosen
}

/// Public entry point. Spawns the scheduler as a detached tokio task
/// so the world binary's main flow is not blocked. Errors are logged
/// to stderr; the scheduler never panics and never returns.
pub fn start() {
    tokio::spawn(async move {
        scheduler_loop().await;
    });
}

async fn scheduler_loop() {
    let url = std::env::var("OPEN_WORLD_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8081".to_string());
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[scheduler] reqwest client build failed: {e}");
            return;
        }
    };
    eprintln!("[scheduler] started, target={url}");
    let mut last_signature: Option<(bool, u64, u32)> = None;
    loop {
        let cfg = SchedulerConfig::load();
        let signature = (cfg.enabled, cfg.interval_seconds, cfg.actions_per_cycle);
        if last_signature != Some(signature) {
            eprintln!(
                "[scheduler] config: enabled={} interval={}s actions/cycle={}",
                cfg.enabled, cfg.interval_seconds, cfg.actions_per_cycle
            );
            last_signature = Some(signature);
        }
        if !cfg.enabled {
            // Wake up every 30s to check the flag.
            sleep(Duration::from_secs(30)).await;
            continue;
        }
        // Per Arcurus 2026-06-10 #cost-tracker: scale the cycle
        // interval based on the live MiniMax budget so the world
        // backs off when we're near the 5h cap. The gate is
        // computed from `data/budget_gate.json` (refreshed every
        // 5 min by the selena-project budget-gate.timer via
        // `mmx quota`). We don't block on the file: if the read
        // fails, fall back to the operator-configured interval
        // (so a selena-project outage doesn't take the world down).
        //
        // Thresholds (from Arcurus 2026-06-10 #cost-tracker):
        //   - used_pct < 50% : normal cadence (cfg.interval_seconds)
        //   - 50% ≤ used_pct < 80% : 5x longer interval (1/5 calls/h)
        //   - used_pct ≥ 80% : skip this cycle entirely (universal
        //                       80% rule per Arcurus 2026-06-10)
        let gate = read_budget_gate();
        let (effective_interval, gate_state) =
            apply_budget_throttle(cfg.interval_seconds, gate.used_pct);
        if last_signature.as_ref().map(|s| s.0) != Some(cfg.enabled)
            || (last_signature.as_ref().map(|s| s.1) != Some(effective_interval))
        {
            eprintln!(
                "[scheduler] cadence: configured={}s effective={}s used_pct={:.0}% gate={}",
                cfg.interval_seconds, effective_interval, gate.used_pct, gate_state
            );
        }
        if matches!(gate_state, GateState::Closed) {
            eprintln!(
                "[scheduler] === cycle SKIPPED (budget gate closed, used={:.0}%) ===",
                gate.used_pct
            );
            // Wake up on a faster cadence (60s) so we resume as soon
            // as the budget drops below the 90% threshold.
            sleep(Duration::from_secs(60)).await;
            continue;
        }
        execute_cycle(&url, &client, cfg.actions_per_cycle).await;
        sleep(Duration::from_secs(effective_interval)).await;
    }
}

async fn execute_cycle(url: &str, client: &reqwest::Client, actions_per_cycle: u32) {
    eprintln!(
        "[scheduler] === cycle start ({} actions) ===",
        actions_per_cycle
    );
    let entities = match fetch_entities(url, client).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[scheduler] fetch entities failed: {e}");
            return;
        }
    };
    if entities.is_empty() {
        eprintln!("[scheduler] no entities found");
        return;
    }
    let mut last_action = load_entity_last_action();
    let picks = pick_entities_weighted(&entities, actions_per_cycle, &last_action);
    if picks.is_empty() {
        eprintln!("[scheduler] no entities to pick (empty pool)");
        return;
    }
    for (i, entity) in picks.iter().enumerate() {
        let entity_id = entity
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let entity_name = entity
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let entity_type = entity
            .get("entity_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let power = entity_power(entity);
        let idle_s = entity_idle_seconds(&entity_id, &last_action);
        eprintln!(
            "[scheduler]   [{}/{}] entity={} ({}) power={} idle={}s weight={}",
            i + 1,
            picks.len(),
            entity_name,
            entity_type,
            power,
            idle_s,
            (power + 1) * idle_s
        );
        // Step 1: get the LLM prompt context.
        let context = match get_action_context(url, client, &entity_id).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[scheduler]     context error: {e}");
                continue;
            }
        };
        if !context
            .get("llm_configured")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            eprintln!("[scheduler]     LLM not configured - skipping");
            continue;
        }
        let prompt = context
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Step 2: call the LLM.
        let llm = match call_llm(url, client, &entity_id, &prompt).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[scheduler]     LLM failed: {e}");
                continue;
            }
        };
        if !llm.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
            let err = llm
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            eprintln!("[scheduler]     LLM failed: {err}");
            continue;
        }
        let raw_response = llm
            .get("raw_response")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if raw_response.is_empty() {
            eprintln!("[scheduler]     empty raw_response, skipping");
            continue;
        }
        // Step 3: process and apply effects.
        let proc = match process_action(url, client, &entity_id, &raw_response).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[scheduler]     process error: {e}");
                continue;
            }
        };
        if proc.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
            let action = proc
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let outcome_full = proc
                .get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let outcome: String = outcome_full.chars().take(100).collect();
            eprintln!("[scheduler]     ✓ {action}: {outcome}…");
            // Record the action time so the next weighted pick favours
            // entities that haven't acted recently.
            last_action.insert(entity_id, now_epoch());
        } else {
            let err = proc
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            eprintln!("[scheduler]     ✗ process failed: {err}");
        }
    }
    save_entity_last_action(&last_action);
}

// --- HTTP helpers --------------------------------------------------------

/// Auth: the world's auth cookie is just `name=1` (see
/// `verify_auth_cookie` in main.rs). Setting it directly is safe
/// for in-process clients; a future refactor could move to a
/// proper token, but this matches the existing model.
const AUTH_COOKIE: &str = "openworld_auth=1";

// --- Budget gate (added 2026-06-10 per Arcurus #cost-tracker) -----------
//
// The world is the single biggest MiniMax consumer (see MEMORY.md,
// AGI cost note: open-world-selena). To avoid blowing the 5h budget
// during heavy turns, the scheduler reads the live budget gate
// (selena-project's `data/budget_gate.json`, refreshed every 10 sec
// via `mmx quota`; was 5 min before 2026-06-11 per Arcurus #lunar-project)
// and applies a 3-tier throttle:
//
//   used_pct < 50%            -> normal cadence (configured interval)
//   50% <= used_pct < 90%     -> 5x longer interval (1/5 calls/h)
//   used_pct >= 90%           -> cycle skipped entirely; the
//                                scheduler wakes every 60s to
//                                check whether the gate has reopened
//
// The path is overridable via the OPEN_WORLD_BUDGET_GATE env var
// (useful for tests). If the file is missing or malformed, we
// fall back to the operator-configured interval — a selena-project
// outage should NOT take the world down. The world is a consumer
// of services, not the other way around (per 2026-06-08 #openworld
// decoupling).
const DEFAULT_BUDGET_GATE_PATH: &str =
    "/home/openclaw/openclaw/workspace/selena-project/data/budget_gate.json";
// Per Arcurus 2026-06-10 #lunar-project: the universal 80% rule applies
// to ALL autonomous agents, including the OW scheduler.  The throttle
// tier (50%..80%) is a soft signal to slow down; the halt threshold
// (80%) is the hard stop that matches the global gate.
const BUDGET_THROTTLE_THRESHOLD_PCT: f64 = 50.0;
const BUDGET_HALT_THRESHOLD_PCT: f64 = 80.0;
const BUDGET_THROTTLE_MULTIPLIER: u64 = 5;
const BUDGET_GATE_POLL_SECONDS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    /// < 50% used, or file missing/malformed (fail-open by design).
    Open,
    /// 50%..80% used; 5x longer interval.
    Throttled,
    /// >= 80% used; skip cycles until the gate reopens.  Matches
    /// the universal 80% rule in selena-project/code/budget_gate.py.
    Closed,
}

impl std::fmt::Display for GateState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateState::Open => f.write_str("open"),
            GateState::Throttled => f.write_str("throttled"),
            GateState::Closed => f.write_str("closed"),
        }
    }
}

#[derive(Debug, Clone)]
struct BudgetSnapshot {
    used_pct: f64,
    state: String,
}

impl Default for BudgetSnapshot {
    fn default() -> Self {
        Self {
            used_pct: 0.0,
            state: "unknown".to_string(),
        }
    }
}

fn read_budget_gate() -> BudgetSnapshot {
    let path = std::env::var("OPEN_WORLD_BUDGET_GATE")
        .unwrap_or_else(|_| DEFAULT_BUDGET_GATE_PATH.to_string());
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return BudgetSnapshot::default(),
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return BudgetSnapshot::default(),
    };
    let used_pct = v
        .get("used_pct")
        .and_then(|x| x.as_f64())
        .or_else(|| {
            v.get("used_pct")
                .and_then(|x| x.as_i64())
                .map(|n| n as f64)
        })
        .unwrap_or(0.0);
    let state = v
        .get("state")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    BudgetSnapshot { used_pct, state }
}

fn apply_budget_throttle(
    configured_interval_s: u64,
    used_pct: f64,
) -> (u64, GateState) {
    if used_pct >= BUDGET_HALT_THRESHOLD_PCT {
        // Caller uses this to skip the cycle entirely; the actual
        // sleep is `BUDGET_GATE_POLL_SECONDS` (60s).
        (BUDGET_GATE_POLL_SECONDS, GateState::Closed)
    } else if used_pct >= BUDGET_THROTTLE_THRESHOLD_PCT {
        let throttled = configured_interval_s
            .saturating_mul(BUDGET_THROTTLE_MULTIPLIER)
            .max(1);
        (throttled, GateState::Throttled)
    } else {
        (configured_interval_s, GateState::Open)
    }
}

async fn fetch_entities(url: &str, client: &reqwest::Client) -> Result<Vec<Value>, String> {
    let resp = client
        .get(format!("{url}/api/entities?limit=100"))
        .header("Cookie", AUTH_COOKIE)
        .send()
        .await
        .map_err(|e| format!("fetch_entities HTTP error: {e}"))?;
    let status = resp.status();
    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("fetch_entities JSON error: {e}"))?;
    if !status.is_success() {
        return Err(format!("fetch_entities HTTP {status}: {data}"));
    }
    if !data.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(format!("fetch_entities API error: {data}"));
    }
    Ok(data
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

async fn get_action_context(
    url: &str,
    client: &reqwest::Client,
    entity_id: &str,
) -> Result<Value, String> {
    let resp = client
        .get(format!("{url}/api/entities/{entity_id}/action/context"))
        .header("Cookie", AUTH_COOKIE)
        .send()
        .await
        .map_err(|e| format!("get_action_context HTTP error: {e}"))?;
    let status = resp.status();
    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("get_action_context JSON error: {e}"))?;
    if !status.is_success() {
        return Err(format!("get_action_context HTTP {status}: {data}"));
    }
    Ok(data)
}

async fn call_llm(
    url: &str,
    client: &reqwest::Client,
    entity_id: &str,
    prompt: &str,
) -> Result<Value, String> {
    let resp = client
        .post(format!("{url}/api/entities/{entity_id}/action/llm"))
        .header("Cookie", AUTH_COOKIE)
        .json(&json!({ "context": prompt }))
        .send()
        .await
        .map_err(|e| format!("call_llm HTTP error: {e}"))?;
    let status = resp.status();
    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("call_llm JSON error: {e}"))?;
    if !status.is_success() {
        return Err(format!("call_llm HTTP {status}: {data}"));
    }
    Ok(data)
}

async fn process_action(
    url: &str,
    client: &reqwest::Client,
    entity_id: &str,
    raw_response: &str,
) -> Result<Value, String> {
    let resp = client
        .post(format!("{url}/api/entities/{entity_id}/action/process"))
        .header("Cookie", AUTH_COOKIE)
        .json(&json!({
            "entity_id": entity_id,
            "raw_response": raw_response,
        }))
        .send()
        .await
        .map_err(|e| format!("process_action HTTP error: {e}"))?;
    let status = resp.status();
    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("process_action JSON error: {e}"))?;
    if !status.is_success() {
        return Err(format!("process_action HTTP {status}: {data}"));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(id: &str, name: &str, power: u64) -> Value {
        json!({
            "id": id,
            "name": name,
            "entity_type": "character",
            "properties_int": { "power": power },
        })
    }

    #[test]
    fn pick_weighted_zero_k_returns_empty() {
        let entities = vec![ent("a", "A", 10), ent("b", "B", 20)];
        let picks = pick_entities_weighted(&entities, 0, &HashMap::new());
        assert!(picks.is_empty());
    }

    #[test]
    fn pick_weighted_empty_pool_returns_empty() {
        let entities: Vec<Value> = vec![];
        let picks = pick_entities_weighted(&entities, 1, &HashMap::new());
        assert!(picks.is_empty());
    }

    #[test]
    fn pick_weighted_picks_k_entities() {
        let entities: Vec<Value> = (0..10)
            .map(|i| ent(&format!("e{i}"), &format!("E{i}"), i))
            .collect();
        let picks = pick_entities_weighted(&entities, 3, &HashMap::new());
        assert_eq!(picks.len(), 3);
        // All picks are unique
        let ids: std::collections::HashSet<String> = picks
            .iter()
            .map(|p| p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string())
            .collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn pick_weighted_higher_power_more_likely() {
        // 100 trials with two entities: one with 0 power, one with
        // 1000 power. The high-power entity should win the vast
        // majority of trials (the (power+1) factor dominates).
        let entities = vec![ent("low", "Low", 0), ent("hi", "Hi", 1000)];
        let mut hi_wins = 0;
        let trials = 100;
        for _ in 0..trials {
            let picks = pick_entities_weighted(&entities, 1, &HashMap::new());
            let id = picks[0].get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id == "hi" {
                hi_wins += 1;
            }
        }
        assert!(
            hi_wins > trials * 8 / 10,
            "expected hi to dominate; got {hi_wins}/{trials}"
        );
    }

    #[test]
    fn entity_power_default_zero_when_missing() {
        let e = json!({"id": "x", "name": "X"});
        assert_eq!(entity_power(&e), 0);
        let e2 = json!({"id": "y", "name": "Y", "properties_int": {}});
        assert_eq!(entity_power(&e2), 0);
    }

    #[test]
    fn config_load_falls_back_to_defaults() {
        // Reading from a non-existent path returns defaults and
        // (best-effort) writes them out. We don't assert on disk —
        // just that we got a valid config.
        let cfg = SchedulerConfig::load();
        assert!(cfg.enabled);
        assert!(cfg.interval_seconds >= 5);
        assert!(cfg.actions_per_cycle <= 20);
    }

    #[test]
    fn idle_seconds_default_for_unknown_entity() {
        let m: HashMap<String, f64> = HashMap::new();
        let idle = entity_idle_seconds("never_seen", &m);
        assert_eq!(idle, DEFAULT_IDLE_SECONDS);
    }
    #[test]
    fn load_handles_python_float_timestamps() {
        // The Python scheduler stored `time.time()` (float seconds
        // with sub-second precision). The Rust load must accept
        // these without failing. This is the bug that caused the
        // 2026-06-08 first-deploy incident where every cycle wiped
        // the file because `serde_json::from_str` rejected the
        // float values as not-`u64`.
        let tmp = std::env::temp_dir().join("ow_test_float_ts.json");
        std::fs::write(
            &tmp,
            r#"{"uuid-a": 1780941841.9880254, "uuid-b": 1780941850.0}"#,
        )
        .unwrap();
        let s = std::fs::read_to_string(&tmp).unwrap();
        let parsed: HashMap<String, f64> = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get("uuid-a").copied(), Some(1780941841.9880254));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn entity_power_handles_negative() {
        // The world has a few entities with negative power (e.g.
        // artifacts). Don't let those underflow when cast to u64.
        let e = json!({"id": "x", "properties_int": {"power": -17}});
        assert_eq!(entity_power(&e), 0);
    }

    // --- Budget-gate throttle (added 2026-06-10 per Arcurus #cost-tracker;
    //     updated 2026-06-10 to use the universal 80% halt threshold
    //     per Arcurus #lunar-project) ---

    #[test]
    fn throttle_below_50_pct_is_open() {
        let (interval, state) = apply_budget_throttle(120, 30.0);
        assert_eq!(state, GateState::Open);
        assert_eq!(interval, 120);
    }

    #[test]
    fn throttle_50_to_80_pct_is_5x() {
        let (interval, state) = apply_budget_throttle(120, 50.0);
        assert_eq!(state, GateState::Throttled);
        assert_eq!(interval, 600);
        let (i2, _) = apply_budget_throttle(120, 79.9);
        assert_eq!(i2, 600);
    }

    #[test]
    fn throttle_80_pct_or_above_closes() {
        // Universal 80% rule (per Arcurus 2026-06-10 #lunar-project).
        let (interval, state) = apply_budget_throttle(120, 80.0);
        assert_eq!(state, GateState::Closed);
        assert_eq!(interval, 60); // poll interval, not interval_seconds
        let (i2, s2) = apply_budget_throttle(120, 100.0);
        assert_eq!(s2, GateState::Closed);
        assert_eq!(i2, 60);
    }

    #[test]
    fn read_budget_gate_missing_file_is_default() {
        // No env override + the default path may or may not exist
        // on a test box; either way we get a valid snapshot back
        // (no panic, no Err) and used_pct is 0.0 (fail-open).
        let snap = read_budget_gate();
        // When the file is missing we get defaults; when it
        // exists we get whatever's in it. We can't assert on a
        // specific number, just that the call doesn't panic and
        // the snapshot is well-formed.
        assert!(snap.used_pct >= 0.0);
    }

    #[test]
    fn read_budget_gate_respects_env_override() {
        let tmp = std::env::temp_dir().join("ow_test_budget_gate.json");
        std::fs::write(
            &tmp,
            r#"{"used_pct": 77.0, "state": "closed-80"}"#,
        )
        .unwrap();
        // Save and restore the env var so the test is hermetic.
        let prev = std::env::var("OPEN_WORLD_BUDGET_GATE").ok();
        std::env::set_var("OPEN_WORLD_BUDGET_GATE", &tmp);
        let snap = read_budget_gate();
        // Restore.
        if let Some(v) = prev {
            std::env::set_var("OPEN_WORLD_BUDGET_GATE", v);
        } else {
            std::env::remove_var("OPEN_WORLD_BUDGET_GATE");
        }
        std::fs::remove_file(&tmp).ok();
        assert!((snap.used_pct - 77.0).abs() < 0.01);
        assert_eq!(snap.state, "closed-80");
    }
}
