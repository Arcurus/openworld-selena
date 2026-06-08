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
        execute_cycle(&url, &client, cfg.actions_per_cycle).await;
        sleep(Duration::from_secs(cfg.interval_seconds)).await;
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

}
