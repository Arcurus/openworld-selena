mod world_data;
use crate::world_data::default_true;

// ---------------------------------------------------------------------------
// History-summary cap helper (shared by entity GET response + process_action)
// ---------------------------------------------------------------------------
//
// `resolve_history_summary_cap_info` returns (effective_max, source) where
// `source` is "world" (per-world override) or "global" (settings.json
// default). Kept as a thin wrapper around
// `context_builder::resolve_max_history_summary_chars_with_source` so the
// same logic powers the LLM prompt, the process_action truncation, and
// the entity API response.
fn resolve_history_summary_cap_info(
    state: &AppState,
    world: &World,
) -> (u32, String) {
    let (cap, src) = context_builder::resolve_max_history_summary_chars_with_source(
        world,
        state.settings.llm.default_max_history_summary_chars,
    );
    (cap, src.to_string())
}


use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::io::Write;
use std::collections::VecDeque;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put, delete},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;
use tracing_subscriber;
use uuid::Uuid;

use world_data::{World, WorldEntity, persistence::BinaryPersistence, context_builder, action_history_log::{self, ActionHistoryEntry}};

/// Append an entry to world_data/action_history.jsonl.  Used by
/// process_action_handler to keep a durable, append-only record of
/// every world action independent of save.owbl.  Best-effort: a write
/// failure logs to stderr but does not block the action response.
fn append_action_history_jsonl(entry: &ActionHistoryEntry) {
    if let Err(e) = action_history_log::append_entry(entry) {
        eprintln!("[history] failed to append action history: {}", e);
    }
}

// ============================================================================
// LLM call tracking — report to the central selena-api tracker
// ============================================================================
//
// Open World's process_action_handler makes LLM calls.  We want those
// calls to show up in the shared call-tracking / budget-alert system
// (selena-project /api/llm-usage/*), so the per-project / per-provider
// budget breakdown actually reflects OW's activity.
//
// We POST to /api/llm-usage/record with a static bearer token read
// from the LLM_RECORD_TOKEN env var.  The call is fire-and-forget on
// a detached tokio task so it never blocks the LLM response.
//
// If the selena-api is unreachable (network / 5xx / token mismatch),
// we log a warning ONCE per process lifetime and continue.  Restart
// the OW server to re-test the connection.

use std::sync::atomic::{AtomicBool, Ordering};

static LLM_TRACKING_WARNED: AtomicBool = AtomicBool::new(false);

fn tracking_url_and_token() -> (String, String) {
    let base = std::env::var("SELENA_API_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8765".to_string());
    let token = std::env::var("LLM_RECORD_TOKEN").unwrap_or_default();
    (base.trim_end_matches('/').to_string(), token)
}

/// Fire-and-forget POST to /api/llm-usage/record.  Reads token + URL
/// from env at call time so a config change doesn't require a recompile.
///
/// As of 2026-06-04 this also forwards real `usage` token counts
/// (extracted from the MiniMax response) and char counts (input
/// context, output text, optional reasoning/thinking block).  All
/// count fields are optional query params; missing fields are
/// stored as `null` in the event log, which is forward-compatible
/// with older readers.
#[allow(clippy::too_many_arguments)]
fn record_llm_call_async(
    provider: String,
    model: String,
    project: String,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    reasoning_tokens: Option<i64>,
    chars_in: Option<i64>,
    chars_out: Option<i64>,
    chars_reasoning: Option<i64>,
) {
    tokio::spawn(async move {
        let (base, token) = tracking_url_and_token();
        if token.is_empty() {
            // No token configured = tracking disabled.  Warn once.
            if !LLM_TRACKING_WARNED.swap(true, Ordering::SeqCst) {
                eprintln!(
                    "[llm_tracking] LLM_RECORD_TOKEN is not set in the OW \
                     server's env; LLM calls will NOT be reported to \
                     selena-api. Per-project / per-provider budget \
                     breakdowns will be inaccurate. Restart the OW server \
                     after setting the token to re-test. \
                     (provider={}, project={})",
                    provider, project
                );
            }
            return;
        }
        // Build the URL with all optional count fields.  Empty Option
        // => no query param => the receiver stores `null` in the log.
        let mut url = format!(
            "{}/api/llm-usage/record?provider={}&model={}&project={}",
            base,
            urlencoding::encode(&provider),
            urlencoding::encode(&model),
            urlencoding::encode(&project),
        );
        if let Some(v) = tokens_in        { url.push_str(&format!("&tokens_in={}", v)); }
        if let Some(v) = tokens_out       { url.push_str(&format!("&tokens_out={}", v)); }
        if let Some(v) = reasoning_tokens { url.push_str(&format!("&reasoning_tokens={}", v)); }
        if let Some(v) = chars_in         { url.push_str(&format!("&chars_in={}", v)); }
        if let Some(v) = chars_out        { url.push_str(&format!("&chars_out={}", v)); }
        if let Some(v) = chars_reasoning  { url.push_str(&format!("&chars_reasoning={}", v)); }
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn_tracking_once(&format!(
                    "[llm_tracking] failed to build reqwest client: {} \
                     (provider={}, project={})",
                    e, provider, project
                ));
                return;
            }
        };
        match client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                // success — reset the warned flag for future outages
                LLM_TRACKING_WARNED.store(false, Ordering::SeqCst);
            }
            Ok(resp) => {
                warn_tracking_once(&format!(
                    "[llm_tracking] selena-api responded with HTTP {} \
                     (URL: {}); per-project budget breakdown will be \
                     inaccurate. Will retry on next call. \
                     (provider={}, project={})",
                    resp.status(), url, provider, project
                ));
            }
            Err(e) => {
                warn_tracking_once(&format!(
                    "[llm_tracking] could not reach selena-api at {}: {}. \
                     Per-project budget breakdown will be inaccurate until \
                     the API is reachable. Will retry on next call. \
                     (provider={}, project={})",
                    base, e, provider, project
                ));
            }
        }
    });
}

fn warn_tracking_once(msg: &str) {
    if !LLM_TRACKING_WARNED.swap(true, Ordering::SeqCst) {
        eprintln!("{}", msg);
    }
}

// ============================================================================
// Logger (daily rotating logs for errors and LLM calls)
// ============================================================================

struct DailyLogger {
    error_file_path: PathBuf,
    llm_file_path: PathBuf,
    today_date: String,
}

impl DailyLogger {
    fn new(log_dir: PathBuf) -> Self {
        let today = chrono_now_date();
        Self {
            error_file_path: log_dir.join(format!("error-log-{}.log", today)),
            llm_file_path: log_dir.join(format!("llm-log-{}.log", today)),
            today_date: today,
        }
    }
    
    fn ensure_today(&mut self, log_dir: &PathBuf) {
        let today = chrono_now_date();
        if today != self.today_date {
            self.today_date = today.clone();
            self.error_file_path = log_dir.join(format!("error-log-{}.log", today));
            self.llm_file_path = log_dir.join(format!("llm-log-{}.log", today));
        }
    }
    
    fn log_error(&mut self, msg: &str) {
        let timestamp = chrono_now_timestamp();
        let line = format!("[{}] ERROR: {}\n", timestamp, msg);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.error_file_path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
    
    /// `call_label` is the per-call identifier embedded in the visually
    /// scannable header. For `process_action` calls this should be the
    /// action name (e.g. `"descend_to_boundary_fracture of Velora the
    /// Undying"`) so the START header in the log file tells you exactly
    /// which call it is. For the generic `action_llm_handler` (where
    /// the LLM is *deciding* the action) this is just the entity name.
    /// The format (per Arcurus 2026-06-05 #openworld):
    ///   - 3 blank lines as a visual separator from the previous call
    ///   - `** {label} - {timestamp} **` START header
    ///   - `Instruction:` line + the prompt that was sent to the LLM
    ///   - blank line
    ///   - `** Result: {label} - {timestamp} **` END header
    ///   - the LLM's response + parse outcome + extra metadata
    ///   - trailing blank line
    /// The double-asterisk framing and the "Instruction:" / "Result:"
    /// labels make it trivial to scan a long log and find call
    /// boundaries by eye.
    fn log_llm(&mut self, call_label: &str, context: &str, response: &str, time_ms: u64, success: bool, parsing_outcome: &str, extra: &str) {
        let timestamp = chrono_now_timestamp();
        let success_str = if success { "SUCCESS" } else { "FAILED" };
        let label = if call_label.is_empty() { "LLM" } else { call_label };
        let lines = format!(
            "\n\n\n** {} - {} **\nInstruction:\n{}\n\n** Result: {} - {} **\nSuccess: {}\nTime: {} ms\n--- Response ---\n{}\n--- Parsing ---\n{}\n--- Extra ---\n{}\n\n",
            label, timestamp, context, label, timestamp, success_str, time_ms, response, parsing_outcome, extra
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.llm_file_path)
            .and_then(|mut f| f.write_all(lines.as_bytes()));
    }
}

/// Best-effort extraction of the `action` field from a (possibly
/// malformed) LLM JSON response, for use as a visual log label. Returns
/// `None` if the field can't be located, in which case the caller
/// should fall back to a less specific label. This is a quick string
/// scan, not a full JSON parse — the response in this branch is by
/// definition broken, so trying serde_json::from_str would just fail
/// and we'd be back where we started.
fn extract_action_field(response: &str) -> Option<String> {
    // Look for `"action"` (with optional whitespace) followed by `:`.
    let needle = "\"action\"";
    let start = response.find(needle)?;
    let after_key = &response[start + needle.len()..];
    // Skip whitespace and the colon.
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    // Skip whitespace.
    let trimmed = after_colon.trim_start();
    // Expect a `"`.
    if !trimmed.starts_with('"') {
        return None;
    }
    let value = &trimmed[1..];
    // Find the closing `"` (not escaped). Bail on the first unescaped one.
    let mut chars = value.char_indices();
    let mut current_idx = 0;
    let mut prev_was_backslash = false;
    while let Some((i, c)) = chars.next() {
        current_idx = i;
        if prev_was_backslash {
            prev_was_backslash = false;
            continue;
        }
        if c == '\\' {
            prev_was_backslash = true;
            continue;
        }
        if c == '"' {
            return Some(value[..current_idx].to_string());
        }
    }
    None
}

fn chrono_now_date() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let secs = now.as_secs();
    // BUGFIX (2026-06-04): the previous version used `secs as i64` as
    // `days_since_epoch`, which produced garbage years (~4.8M instead of
    // 2026) and broke the one-log-per-day invariant in `DailyLogger`.
    // The correct value is seconds divided by seconds-per-day.
    let days_since_epoch = (secs / 86400) as i64;
    let mut year = 1970;
    let mut month = 1;
    let mut day = 1;
    let mut remaining = days_since_epoch;
    // Simple date calculation: count years, months, days
    let days_in_month: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    while remaining >= 365 {
        let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if is_leap { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        year += 1;
    }
    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let mut dim = days_in_month.to_vec();
    if is_leap { dim[1] = 29; }
    for m in 0..12 {
        if remaining < dim[m] {
            // We're inside month (m+1) — the remaining days are this month's.
            // (Previously month was set only after subtraction, so breaking
            //  on June left month=5=May. Bugfix 2026-06-04.)
            month = (m + 1) as i64;
            break;
        }
        remaining -= dim[m];
        month = (m + 1) as i64;
    }
    day = remaining as i64 + 1;
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn chrono_now_timestamp() -> String {
    // BUGFIX (2026-06-05): this used to be a hand-rolled UTC formatter
    // (epoch → year/month/day/h/m/s). That produced log timestamps in
    // UTC, which on a CEST host diverged from the wall clock by 2 hours
    // and made the log viewer look like the scheduler had stopped when
    // it was actually running fine (per Arcurus #openworld). Switched to
    // chrono::Local so the timestamps match the host's local timezone,
    // consistent with the selena-project api_server.py log which already
    // uses local time. (Note: the daily log filename still uses
    // chrono_now_date which is also local — see comment there for why
    // we keep it local too.) The `clock` feature is enabled on chrono
    // in Cargo.toml to make `Local` available.
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}



// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Deserialize, Clone)]
struct Settings {
    server: ServerSettings,
    world: AppWorldSettings,
    llm: LlmSettings,
    security: SecuritySettings,
    ui: UiSettings,
}

#[derive(Debug, Deserialize, Clone)]
struct ServerSettings {
    host: String,
    port: u16,
}

#[derive(Debug, Deserialize, Clone)]
struct AppWorldSettings {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize, Clone)]
struct LlmSettings {
    provider: String,
    model: String,
    #[serde(default = "default_api_key_name")]
    api_key_name: String,
    #[serde(default = "default_api_url")]
    api_url: String,
    #[serde(default = "default_max_output_tokens")]
    max_output_tokens: u32,
    #[serde(default = "default_llm_timeout_secs")]
    llm_timeout_secs: u64,
    /// Master toggle for LLM calls. When false, no LLM calls are made.
    /// Default: true (calls allowed, limited by max_calls_per_hour).
    #[serde(default = "default_llm_calls_enabled")]
    calls_per_hour_enabled: bool,
    /// Maximum LLM calls allowed in any rolling 1-hour window.
    /// 0 means "no calls allowed" (effectively disabled).
    /// When this limit is reached, no further LLM calls are made
    /// until the oldest call in the window falls outside the 1-hour mark.
    /// Default: 0 (deactivated).
    #[serde(default = "default_max_calls_per_hour")]
    max_calls_per_hour: u32,
    /// Global default for the per-entity `history_summary` cap.
    /// Per-world `WorldSettings.max_history_summary_chars` may override
    /// this; a value of 0 in WorldSettings means "use this global default".
    /// Default: 10000 chars.
    #[serde(default = "default_max_history_summary_chars")]
    default_max_history_summary_chars: u32,
}

fn default_api_url() -> String {
    "https://api.minimax.io/anthropic".to_string()
}

fn default_api_key_name() -> String {
    "MINIMAX_API_KEY".to_string()
}

fn default_max_output_tokens() -> u32 {
    50000
}

fn default_llm_timeout_secs() -> u64 {
    180 // 3 minutes
}

fn default_llm_calls_enabled() -> bool {
    true
}

fn default_max_calls_per_hour() -> u32 {
    0
}

fn default_max_history_summary_chars() -> u32 {
    10000
}

// True if `s` has no meaningful content — only whitespace, punctuation,
// ellipses (`…`), dashes (`—`/`-`), dots, etc. Catches the LLM-emit
// "placebo summary" pattern where the model returns a single placeholder
// character instead of an actual narrative summary. Storing these as
// `Some("…")` produces a useless 1-char `history_summary` that the UI
// shows in place of the real thing. We treat them as `None` instead.
fn is_placeholder_summary(s: &str) -> bool {
    !s.chars().any(|c| c.is_alphanumeric())
}

// ============================================================================
// LLM rate limiter (rolling 1-hour window)
// ============================================================================
//
// Tracks the timestamps of recent LLM calls in a sliding 1-hour window.
// When the window is full (or calls are globally disabled), try_acquire()
// returns a `RateLimitDecision` that the handler can convert into a
// descriptive error response.
//
// `max_calls_per_hour == 0` is treated as "no calls allowed" so an
// unconfigured / zeroed-out limit cleanly disables LLM usage without
// requiring a separate boolean.
//
// `enabled == false` is a master kill-switch that disables calls
// regardless of the limit value.
#[derive(Debug)]
struct LlmRateLimiter {
    enabled: bool,
    max_calls_per_hour: u32,
    /// Timestamps of calls within the current rolling 1-hour window.
    window: VecDeque<Instant>,
}

#[derive(Debug, Clone, Copy)]
enum RateLimitReason {
    /// Master toggle is off (calls_per_hour_enabled = false).
    Disabled,
    /// Limit is set to 0.
    LimitZero,
    /// `max_calls_per_hour` calls already happened in the last hour.
    /// The value is seconds until the oldest call exits the window.
    LimitReached { retry_after_secs: u64 },
}

impl RateLimitReason {
    fn message(&self) -> String {
        match self {
            RateLimitReason::Disabled => {
                "LLM calls are globally disabled (calls_per_hour_enabled = false)".to_string()
            }
            RateLimitReason::LimitZero => {
                "LLM calls are deactivated (max_calls_per_hour = 0)".to_string()
            }
            RateLimitReason::LimitReached { retry_after_secs } => {
                format!(
                    "LLM rate limit reached. Try again in {}s.",
                    retry_after_secs
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RateLimitDecision {
    Allowed,
    Blocked(RateLimitReason),
}

impl LlmRateLimiter {
    fn new(enabled: bool, max_calls_per_hour: u32) -> Self {
        Self {
            enabled,
            max_calls_per_hour,
            window: VecDeque::new(),
        }
    }

    /// Rebuild the limiter when settings change.
    fn reconfigure(&mut self, enabled: bool, max_calls_per_hour: u32) {
        self.enabled = enabled;
        self.max_calls_per_hour = max_calls_per_hour;
        // Existing window is still meaningful; do not clear it.
    }

    /// Decide whether the next call is allowed. If allowed, ALSO records it
    /// in the rolling window. Callers must treat the decision atomically:
    /// if it says Blocked, no call should be made.
    fn try_acquire(&mut self) -> RateLimitDecision {
        if !self.enabled {
            return RateLimitDecision::Blocked(RateLimitReason::Disabled);
        }
        if self.max_calls_per_hour == 0 {
            return RateLimitDecision::Blocked(RateLimitReason::LimitZero);
        }

        let now = Instant::now();
        // Prune anything older than 1 hour.
        while let Some(&front) = self.window.front() {
            if now.duration_since(front) >= Duration::from_secs(3600) {
                self.window.pop_front();
            } else {
                break;
            }
        }

        if (self.window.len() as u32) >= self.max_calls_per_hour {
            // Compute retry_after from the oldest entry.
            let retry_after = self
                .window
                .front()
                .map(|t| {
                    let elapsed = now.duration_since(*t);
                    if elapsed >= Duration::from_secs(3600) {
                        0
                    } else {
                        3600 - elapsed.as_secs()
                    }
                })
                .unwrap_or(0);
            return RateLimitDecision::Blocked(RateLimitReason::LimitReached {
                retry_after_secs: retry_after,
            });
        }

        self.window.push_back(now);
        RateLimitDecision::Allowed
    }

    /// Read-only snapshot for the status endpoint.
    fn status(&self) -> LlmRateLimitStatus {
        let now = Instant::now();
        let mut pruned = self.window.clone();
        while let Some(&front) = pruned.front() {
            if now.duration_since(front) >= Duration::from_secs(3600) {
                pruned.pop_front();
            } else {
                break;
            }
        }
        let calls_in_last_hour = pruned.len() as u32;
        let retry_after_secs = pruned.front().map(|t| {
            let elapsed = now.duration_since(*t);
            if elapsed >= Duration::from_secs(3600) {
                0
            } else {
                3600 - elapsed.as_secs()
            }
        });
        LlmRateLimitStatus {
            enabled: self.enabled,
            max_calls_per_hour: self.max_calls_per_hour,
            calls_in_last_hour,
            retry_after_secs,
        }
    }
}

#[derive(Debug, Serialize, Clone)]
struct LlmRateLimitStatus {
    enabled: bool,
    max_calls_per_hour: u32,
    calls_in_last_hour: u32,
    /// Seconds until the oldest call in the window expires (None if window is empty).
    retry_after_secs: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
struct SecuritySettings {
    #[serde(default = "default_password_var_name")]
    password_var_name: String,
    #[serde(default = "default_cookie_name")]
    cookie_name: String,
    #[serde(default = "default_cookie_duration")]
    cookie_duration_secs: u64,
}

fn default_password_var_name() -> String {
    "WEB_PASSWORD".to_string()
}

fn default_cookie_name() -> String {
    "openworld_auth".to_string()
}

fn default_cookie_duration() -> u64 {
    3600
}

#[derive(Debug, Deserialize, Clone)]
struct UiSettings {
    title: String,
}

/// Application state
#[derive(Clone)]
struct AppState {
    world: Arc<RwLock<World>>,
    settings: Settings,
    save_path: String,
    env_path: String,
    logger: Arc<std::sync::Mutex<DailyLogger>>,
    /// Sliding 1-hour LLM-call limiter. Shared across handlers so all
    /// LLM-touching endpoints count against the same budget.
    llm_rate_limiter: Arc<std::sync::Mutex<LlmRateLimiter>>,
}

// ============================================================================
// Request/Response types
// ============================================================================

#[derive(Debug, Deserialize)]
struct CreateEntityRequest {
    entity_type: String,
    name: String,
    description: Option<String>,
    x: f64,
    y: f64,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EntityResponse {
    success: bool,
    data: Option<WorldEntity>,
    error: Option<String>,
    /// Resolved effective cap (per-world override, or global default from
    /// settings.json). 0 means "use the global default" — clients should
    /// treat 0 as "unknown" and fall back to the LLM template's value.
    /// Added 2026-06-04 so the web client can show the real cap on the
    /// 📋 History Summary card instead of hard-coding "~500".
    #[serde(skip_serializing_if = "Option::is_none")]
    max_history_summary_chars: Option<u32>,
    /// The source of the cap: "world" (per-world override), "global"
    /// (settings.json default), or "fallback" (no global, using the
    /// hard-coded default in code). Helps the UI explain which knob
    /// controls the cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_history_summary_source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateEntityRequest {
    name: Option<String>,
    description: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    entity_type: Option<String>,
    tags: Option<String>,
    near_x: Option<f64>,
    near_y: Option<f64>,
    radius: Option<f64>,
    limit: Option<usize>,
    /// Filter by system entities only (world_clock or meta-tagged)
    system: Option<bool>,
    /// Whether to include system entities (default: true)
    include_system: Option<bool>,
}

#[derive(Debug, Serialize)]
struct WorldResponse {
    success: bool,
    data: Option<WorldInfo>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorldInfo {
    name: String,
    description: String,
    entity_count: usize,
    action_count: u64,
    last_world_action: Option<String>,
    properties_int: std::collections::HashMap<String, i64>,
    properties_float: std::collections::HashMap<String, f64>,
    properties_string: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize)]
struct SaveStatus {
    saved: bool,
    path: String,
}

#[derive(Debug, Deserialize)]
struct CreateWorldRequest {
    name: String,
    description: Option<String>,
    generate_sample: bool,
}

#[derive(Debug, Serialize)]
struct WorldStatus {
    has_save: bool,
    save_path: Option<String>,
    save_size: Option<u64>,
    save_modified: Option<String>,
}

// ============================================================================
// Helper functions
// ============================================================================

fn success_json<T: Serialize>(data: T) -> Json<T> {
    Json(data)
}

fn error_json(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({
        "success": false,
        "error": message
    });
    (status, Json(body)).into_response()
}

/// Convert a `RateLimitDecision` into either:
/// - `None` if the call is allowed (and recorded), or
/// - `Some(response)` if blocked — a 429 JSON response describing why.
fn enforce_llm_rate_limit(
    limiter: &std::sync::Mutex<LlmRateLimiter>,
    context: &str,
) -> Option<Response> {
    let mut guard = match limiter.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.try_acquire() {
        RateLimitDecision::Allowed => None,
        RateLimitDecision::Blocked(reason) => {
            let message = reason.message();
            // Best-effort log; don't block the response on logging.
            eprintln!("🚫 LLM call blocked ({}): {}", context, message);
            let body = match reason {
                RateLimitReason::LimitReached { retry_after_secs } => serde_json::json!({
                    "success": false,
                    "error": message,
                    "rate_limited": true,
                    "retry_after_secs": retry_after_secs,
                }),
                _ => serde_json::json!({
                    "success": false,
                    "error": message,
                    "rate_limited": true,
                }),
            };
            Some((StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response())
        }
    }
}

// ============================================================================
// World endpoints
// ============================================================================

/// Snapshot the current save.owbl to `world_data/backups/save-<label>-<TS>.owbl`.
/// Returns the path of the new snapshot (relative to the project root).
///
/// `label` is short tag like "auto" or "manual" or "pre-restore".
/// Called by:
///   - POST /api/world/backup         (auth, manual on-demand)
///   - create_world_handler          (auto, before overwriting)
///   - anywhere we want a safety net
fn snapshot_save(label: &str) -> Result<std::path::PathBuf, String> {
    let save_path = std::path::Path::new("world_data/save.owbl");
    if !save_path.exists() {
        return Err("no save file to snapshot".to_string());
    }
    let backup_dir = std::path::Path::new("world_data/backups");
    std::fs::create_dir_all(backup_dir).map_err(|e| format!("mkdir: {}", e))?;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let dest = backup_dir.join(format!("save-{}-{}.owbl", label, ts));
    std::fs::copy(save_path, &dest).map_err(|e| format!("copy: {}", e))?;
    Ok(dest)
}

async fn get_world(State(state): State<AppState>) -> impl IntoResponse {
    let world = state.world.read().await;
    let stats = world.calculate_stats();

    success_json(serde_json::json!({
        "success": true,
        "data": {
            "name": world.name,
            "description": world.description,
            "entity_count": world.entity_count(),
            "action_count": world.action_count,
            "last_world_action": world.last_world_action.map(|dt: chrono::DateTime<chrono::Utc>| dt.to_rfc3339()),
            "properties_int": world.properties_int,
            "properties_float": world.properties_float,
            "properties_string": world.properties_string,
        },
        "stats": stats,
    }))
}

// Update world properties
#[derive(Debug, Deserialize)]
struct UpdateWorldRequest {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    properties_int: std::collections::HashMap<String, i64>,
    #[serde(default)]
    properties_float: std::collections::HashMap<String, f64>,
    #[serde(default)]
    properties_string: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct AddWorldEventRequest {
    name: String,
    description: String,
    #[serde(default)]
    influence: String,
    #[serde(default = "default_true")]
    active: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateWorldEventRequest {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    influence: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

async fn update_world_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<UpdateWorldRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    if let Some(name) = req.name {
        world.name = name;
    }
    if let Some(description) = req.description {
        world.description = description;
    }
    
    // Update properties
    for (key, value) in req.properties_int {
        world.properties_int.insert(key, value);
    }
    for (key, value) in req.properties_float {
        world.properties_float.insert(key, value);
    }
    for (key, value) in req.properties_string {
        world.properties_string.insert(key, value);
    }
    
    // Save after update
    drop(world);
    let world = state.world.read().await;
    match crate::world_data::persistence::BinaryPersistence::save_world(&world, &state.save_path) {
        Ok(()) => success_json(serde_json::json!({
            "success": true,
            "message": "World updated and saved"
        })).into_response(),
        Err(e) => error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to save: {}", e)),
    }
}

// Get world stats
async fn get_world_stats(State(state): State<AppState>) -> Response {
    let world = state.world.read().await;
    let stats = world.calculate_stats();

    success_json(serde_json::json!({
        "success": true,
        "stats": stats
    })).into_response()
}

// ============================================================================
// LLM status / config endpoints
// ============================================================================
//
// `GET  /api/llm/status`  — read-only snapshot of the rate limiter.
// `POST /api/llm/config`  — update enabled / max_calls_per_hour at runtime.
//                          Persists to settings.json so the new value
//                          survives a restart.
//
// Both endpoints require authentication like other mutating endpoints.

async fn llm_status_handler(State(state): State<AppState>) -> Response {
    let api_key = read_env_var(&state.env_path, &state.settings.llm.api_key_name);
    let status = llm_rate_limit_status(&state);
    let calls_blocked = !status.enabled || status.max_calls_per_hour == 0;
    let limit_reached = status.calls_in_last_hour >= status.max_calls_per_hour
        && status.max_calls_per_hour > 0;

    success_json(serde_json::json!({
        "success": true,
        "llm_configured": api_key.is_some(),
        "rate_limit": status,
        "calls_blocked": calls_blocked,
        "limit_reached": limit_reached
    })).into_response()
}

#[derive(Debug, Deserialize)]
struct LlmConfigUpdate {
    #[serde(default)]
    calls_per_hour_enabled: Option<bool>,
    #[serde(default)]
    max_calls_per_hour: Option<u32>,
}

async fn llm_config_update_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LlmConfigUpdate>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie").and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    if req.calls_per_hour_enabled.is_none() && req.max_calls_per_hour.is_none() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "Provide calls_per_hour_enabled and/or max_calls_per_hour",
        );
    }

    // Mutate the live limiter.
    {
        let mut guard = match state.llm_rate_limiter.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let new_enabled = req
            .calls_per_hour_enabled
            .unwrap_or(state.settings.llm.calls_per_hour_enabled);
        let new_max = req
            .max_calls_per_hour
            .unwrap_or(state.settings.llm.max_calls_per_hour);
        guard.reconfigure(new_enabled, new_max);
    }

    // Persist to settings.json (so the value survives restarts).
    match persist_llm_settings(&state) {
        Ok(_) => {
            let status = llm_rate_limit_status(&state);
            success_json(serde_json::json!({
                "success": true,
                "message": "LLM rate limit updated",
                "rate_limit": status
            })).into_response()
        }
        Err(e) => error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Updated limiter but failed to persist settings.json: {}", e),
        ),
    }
}

/// Rewrite the `llm` section of settings.json with the values currently
/// held in AppState. The limiter is the source of truth for the live
/// process; we mirror it back to disk so a restart picks up the new values.
fn persist_llm_settings(state: &AppState) -> Result<(), String> {
    let path = std::path::Path::new("settings.json");
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut root: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let status = llm_rate_limit_status(state);
    root["llm"]["calls_per_hour_enabled"] = serde_json::json!(status.enabled);
    root["llm"]["max_calls_per_hour"] = serde_json::json!(status.max_calls_per_hour);
    let serialized = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(path, serialized).map_err(|e| e.to_string())?;
    Ok(())
}

// ============================================================================
// World Events endpoints
// ============================================================================

// Get all world events
async fn get_world_events(State(state): State<AppState>) -> Response {
    let world = state.world.read().await;
    
    success_json(serde_json::json!({
        "success": true,
        "events": world.active_events
    })).into_response()
}

// Add a new world event
async fn add_world_event(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<AddWorldEventRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    let event = crate::world_data::WorldEvent {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name,
        description: req.description,
        influence: req.influence,
        active: req.active,
    };
    
    world.active_events.push(event.clone());
    
    success_json(serde_json::json!({
        "success": true,
        "event": event
    })).into_response()
}

// Update a world event
async fn update_world_event(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(event_id): axum::extract::Path<String>,
    Json(req): Json<UpdateWorldEventRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    // Find the event
    if let Some(event) = world.active_events.iter_mut().find(|e| e.id == event_id) {
        if let Some(name) = req.name {
            event.name = name;
        }
        if let Some(description) = req.description {
            event.description = description;
        }
        if let Some(influence) = req.influence {
            event.influence = influence;
        }
        if let Some(active) = req.active {
            event.active = active;
        }
        
        success_json(serde_json::json!({
            "success": true,
            "event": event.clone()
        })).into_response()
    } else {
        error_json(StatusCode::NOT_FOUND, "World event not found")
    }
}

// Delete a world event
async fn delete_world_event(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(event_id): axum::extract::Path<String>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    // Find and remove the event
    if let Some(pos) = world.active_events.iter().position(|e| e.id == event_id) {
        world.active_events.remove(pos);
        
        success_json(serde_json::json!({
            "success": true,
            "message": "World event deleted"
        })).into_response()
    } else {
        error_json(StatusCode::NOT_FOUND, "World event not found")
    }
}

// ============================================================================
// Entity endpoints
// ============================================================================

async fn list_entities(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    let world = state.world.read().await;
    
    let mut entities: Vec<&WorldEntity> = world.entities.values().collect();
    
    if let Some(ref q) = query.q {
        let q_lower = q.to_lowercase();
        entities.retain(|e: &&WorldEntity| e.name.to_lowercase().contains(&q_lower));
    }
    
    if let Some(ref entity_type) = query.entity_type {
        entities.retain(|e: &&WorldEntity| e.entity_type == *entity_type);
    }
    
    if let Some(ref tags_str) = query.tags {
        let tags: Vec<String> = tags_str.split(',').map(|s| s.trim().to_string()).collect();
        entities.retain(|e: &&WorldEntity| tags.iter().all(|t| e.has_tag(t)));
    }
    
    if let (Some(x), Some(y), Some(r)) = (query.near_x, query.near_y, query.radius) {
        entities.retain(|e: &&WorldEntity| {
            let dx = e.x - x;
            let dy = e.y - y;
            (dx * dx + dy * dy).sqrt() <= r
        });
    }
    
    // System entity filter: system=true returns ONLY system entities;
    // include_system=false excludes them. System entities are identified
    // by WorldEntity::is_system_entity() (world_clock type or "meta" tag).
    if let Some(true) = query.system {
        entities.retain(|e: &&WorldEntity| e.is_system_entity());
    } else if let Some(false) = query.include_system {
        entities.retain(|e: &&WorldEntity| !e.is_system_entity());
    }
    // Default (include_system unset or true): return all entities
    
    let limit = query.limit.unwrap_or(100);
    entities.truncate(limit);
    
    success_json(serde_json::json!({
        "success": true,
        "count": entities.len(),
        "data": entities
    }))
}

async fn get_entity(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<Uuid>,
) -> Response {
    let world = state.world.read().await;
    let (max_cap, max_source) = resolve_history_summary_cap_info(&state, &world);

    match world.get_entity(&id) {
        Some(entity) => success_json(EntityResponse {
            success: true,
            data: Some(entity.clone()),
            error: None,
            max_history_summary_chars: Some(max_cap),
            max_history_summary_source: Some(max_source),
        }).into_response(),
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

/// GET /api/entities/:id/history?limit=N
///
/// Returns the durable action history for one entity, most recent first.
/// Merges the in-memory entity.history (fast path) with the append-only
/// world_data/action_history.jsonl log (durable path that survives
/// save.owbl replacement) and dedupes by timestamp+action.
async fn get_entity_history(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie").and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50)
        .min(500);

    // Confirm the entity exists; surface 404 if not.
    let entity_name = {
        let world = state.world.read().await;
        match world.get_entity(&id) {
            Some(e) => e.name.clone(),
            None => return error_json(StatusCode::NOT_FOUND, "Entity not found"),
        }
    };

    // Read the durable JSONL log (most recent first).
    let log_entries = action_history_log::load_for_entity(&id.to_string(), limit);
    let total_in_log = action_history_log::count_for_entity(&id.to_string());

    success_json(serde_json::json!({
        "success": true,
        "entity_id": id.to_string(),
        "entity_name": entity_name,
        "count": log_entries.len(),
        "total_in_log": total_in_log,
        "limit": limit,
        "entries": log_entries,
    })).into_response()
}

// ---------------------------------------------------------------------------
// History-summary partial replace
// ---------------------------------------------------------------------------
//
// `POST /api/entities/:id/history-summary/replace`
// Body: { "old_part": "...", "new_part": "...", "not_found_is_error": false }
//
// Applies a single `history_summary_replace` operation to the entity's
// current `history_summary` and persists the result. The actual replace
// logic lives in `apply_history_summary_replaces` (shared with the
// LLM-emit path) so the API, CLI, and LLM behaviors stay in lockstep.
// Conventions (mirrored from the LLM template, ai_templates/EntityAction.md):
//   - `old_part == "!ALL!"` → full replace: discard current, set the
//     summary to `new_part`. Use this when a full restructure is needed
//     and you want to make sure important things don't get lost by
//     rewriting the whole thing in one shot.
//   - `old_part == ""`       → append: `new_part` is added to the end of
//     the current summary (or becomes the new summary if there isn't
//     one yet). No warning logged.
//   - `old_part != ""`       → find-replace: replace the first
//     occurrence of `old_part` with `new_part`. If not found, the call
//     either returns 404 (when `not_found_is_error = true`) or a
//     200-success with a warning and no change made (default lenient
//     behavior; controlled by `not_found_is_error`).
//   - After the replace, the result is truncated to the effective cap
//     (per-world override or global default) if it goes over, with a
//     warning logged.
//
// Per Arcurus 2026-06-04 (#openworld): "add a way to replace one part of
// the history with another one instead of replacing the full one. the
// replace should also cut if the new summary is too long and log a
// warning."
//
// 2026-06-04: empty `old_part` was made the explicit "append" path,
// matching the LLM-emit `history_summary_replace` command. API, CLI, and
// LLM path now have the same affordances.
//
// 2026-06-05: `old_part == "!ALL!"` was added as the full-replace
// convention (per Arcurus). The handler now delegates to
// `apply_history_summary_replaces` so the !ALL! branch and every other
// rule is identical for API and LLM callers.
//
// Response shape (success):
//   { success: true, history_summary, history_summary_chars,
//     truncated: bool, warning: Option<String>,
//     max_chars, max_chars_source }
//
// Errors:
//   404 NOT_FOUND       — entity doesn't exist
//   404 NOT_FOUND       — `old_part` not found in current summary
//                         (only when `not_found_is_error = true`; the
//                          default is success + warning for ergonomics)
//   401 UNAUTHORIZED    — missing/invalid auth cookie
#[derive(Debug, Deserialize)]
struct HistorySummaryReplaceRequest {
    old_part: String,
    new_part: String,
    /// If true (default false), `old_part` not in summary returns 404
    /// instead of warning. Default is warning so the UI can be lenient.
    #[serde(default)]
    not_found_is_error: bool,
}

async fn history_summary_replace_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<HistorySummaryReplaceRequest>,
) -> Response {
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie").and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    // 2026-06-04: empty `old_part` is now the explicit "append"
    // convention — `new_part` is added to the end of the current
    // summary (or becomes the new summary if there isn't one). Empty
    // `new_part` is still a valid "delete" op. No warning logged
    // for the append path; the only warning surfaced is for
    // truncation, which is handled later in this handler.
    //
    // No 400 is returned for either field being empty; both are
    // valid operations.

    let (max_cap, max_source) = {
        let world = state.world.read().await;
        resolve_history_summary_cap_info(&state, &world)
    };

    let mut world = state.world.write().await;

    // Do all the entity mutations inside a block so the &mut borrow
    // on `entity` is released before we call save_world(&world) and
    // log_error (both of which would conflict with the live borrow).
    //
    // 2026-06-05: this handler now delegates the actual replace logic
    // to `apply_history_summary_replaces` (the shared function used by
    // the LLM-emit path), so the API and LLM behaviors are guaranteed
    // to stay in lockstep — including the `!ALL!` full-replace
    // convention. The only API-specific logic that remains here is the
    // `not_found_is_error` toggle, the response shape, and the log
    // mirror.
    let (final_summary, truncated, entity_name_for_log, warnings) = {
        let entity = match world.get_entity_mut(&id) {
            Some(e) => e,
            None => return error_json(StatusCode::NOT_FOUND, "Entity not found"),
        };

        let current = entity.history_summary.clone().unwrap_or_default();

        // Single-element replace chain. The shared function handles
        // empty / "!ALL!" / found / not-found / truncation identically
        // for both API and LLM callers.
        let result = apply_history_summary_replaces(
            Some(&current),
            &[HistorySummaryReplace {
                old_part: req.old_part.clone(),
                new_part: req.new_part.clone(),
            }],
            max_cap as usize,
        );

        // API-only: map the "not found" warning to either an error
        // (strict) or a soft no-op warning (lenient), per the
        // `not_found_is_error` request flag.
        let not_found = !req.old_part.is_empty()
            && req.old_part != "!ALL!"
            && result.warnings.iter().any(|w| w.contains("not found"));
        if not_found && req.not_found_is_error {
            return error_json(
                StatusCode::NOT_FOUND,
                &format!(
                    "old_part not found in current summary (length {})",
                    current.len()
                ),
            );
        }
        if not_found && !req.not_found_is_error {
            // Lenient: no-op, but warn so the caller knows nothing changed.
            return success_json(serde_json::json!({
                "success": true,
                "history_summary": entity.history_summary,
                "history_summary_chars": current.chars().count(),
                "truncated": false,
                "warning": "old_part not found in current summary; no change made",
                "max_chars": max_cap,
                "max_chars_source": max_source,
            })).into_response();
        }

        // Apply the new summary to the entity. `result.new_summary` is
        // `None` if the chain produced empty content (kept as None
        // rather than Some("") to preserve the "never had a summary"
        // vs "had a summary that was just emptied" distinction).
        entity.history_summary = result.new_summary.clone();
        let final_summary = result.new_summary.clone().unwrap_or_default();
        (final_summary, result.truncated, entity.name.clone(), result.warnings)
        // entity borrow released at end of block
    };

    // Persist to save.owbl so the replace survives a restart.
    if let Err(e) = BinaryPersistence::save_world(&world, &state.save_path) {
        // Don't fail the request — the in-memory state is already updated.
        // Just surface a warning.
        return success_json(serde_json::json!({
            "success": true,
            "history_summary": final_summary,
            "history_summary_chars": final_summary.chars().count(),
            "truncated": truncated,
            "warning": format!("In-memory summary updated, but save failed: {}", e),
            "max_chars": max_cap,
            "max_chars_source": max_source,
        })).into_response();
    }

    // Mirror warnings (truncation, etc.) to the error log so they're
    // discoverable via /api/logs as well as the API response.
    if !warnings.is_empty() {
        if let Ok(mut logger) = state.logger.lock() {
            logger.ensure_today(&PathBuf::from("logs"));
            for w in &warnings {
                logger.log_error(&format!(
                    "[history-summary-replace] entity={} (id={}): {}",
                    entity_name_for_log, id, w
                ));
            }
        }
    }

    success_json(serde_json::json!({
        "success": true,
        "history_summary": final_summary,
        "history_summary_chars": final_summary.chars().count(),
        "truncated": truncated,
        "warning": warnings.first().cloned(),
        "max_chars": max_cap,
        "max_chars_source": max_source,
    })).into_response()
}


async fn create_entity(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateEntityRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    let mut entity = WorldEntity::new(&req.entity_type, &req.name, req.x, req.y);
    
    if let Some(desc) = req.description {
        entity.description = desc;
    }
    
    for tag in req.tags {
        entity.add_tag(&tag);
    }
    
    let id = world.add_entity(entity.clone());
    
    success_json(serde_json::json!({
        "success": true,
        "id": id.to_string(),
        "data": entity
    })).into_response()
}

async fn update_entity(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<UpdateEntityRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    let (max_cap, max_source) = resolve_history_summary_cap_info(&state, &world);

    match world.get_entity_mut(&id) {
        Some(entity) => {
            if let Some(name) = req.name {
                entity.name = name;
            }
            if let Some(desc) = req.description {
                entity.description = desc;
            }
            if let Some(x) = req.x {
                entity.x = x;
            }
            if let Some(y) = req.y {
                entity.y = y;
            }
            if let Some(tags) = req.tags {
                entity.tags = tags;
            }
            
            success_json(EntityResponse {
                success: true,
                data: Some(entity.clone()),
                error: None,
                max_history_summary_chars: Some(max_cap),
                max_history_summary_source: Some(max_source),
            }).into_response()
        }
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

async fn delete_entity(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    match world.remove_entity(&id) {
        Some(_) => success_json(serde_json::json!({
            "success": true,
            "message": "Entity deleted"
        })).into_response(),
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

// Request struct for processing LLM response
#[derive(Debug, Deserialize)]
struct ProcessActionRequest {
    raw_response: String,
    entity_id: Uuid,
}

// Request for calling LLM with provided context
#[derive(Debug, Deserialize)]
struct CallLlmRequest {
    context: String,
}

// Generate action context (Step 1)
async fn action_context_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let world = state.world.read().await;
    
    // Find the entity
    let entity = match world.entities.get(&id) {
        Some(e) => e,
        None => return error_json(StatusCode::NOT_FOUND, "Entity not found"),
    };

    // Build the full action context (DRY helper shared with entity_action)
    let ctx = context_builder::build_action_context(
        &world,
        entity,
        state.settings.llm.default_max_history_summary_chars,
    );
    
    // Read the AI template
    let template = match tokio::fs::read_to_string("ai_templates/EntityAction.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    
    // Render the prompt
    let world_name = state.world.read().await.name.clone();
    let prompt = context_builder::build_action_prompt(&world_name, entity, &ctx, &template);
    
    // Check if LLM is configured
    let api_key = read_env_var(&state.env_path, &state.settings.llm.api_key_name);
    let llm_configured = api_key.is_some();
    
    success_json(serde_json::json!({
        "success": true,
        "entity_id": id.to_string(),
        "entity_name": entity.name,
        "entity_type": entity.entity_type,
        "description": entity.description,
        "properties": entity.properties_int,
        "property_context": ctx.prop_context,
        "world_events": ctx.world_events_str,
        "prompt": prompt,
        "llm_configured": llm_configured,
        "llm_rate_limit": llm_rate_limit_status(&state)
    })).into_response()
}

/// Snapshot of the current rate-limit state (no side effects).
fn llm_rate_limit_status(state: &AppState) -> LlmRateLimitStatus {
    match state.llm_rate_limiter.lock() {
        Ok(g) => g.status(),
        Err(poisoned) => poisoned.into_inner().status(),
    }
}

// Call LLM with provided context (Step 2)
async fn action_llm_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<CallLlmRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    // Rate-limit gate: block if disabled or hourly cap reached.
    if let Some(resp) = enforce_llm_rate_limit(
        &state.llm_rate_limiter,
        &format!("action_llm_handler entity={}", id),
    ) {
        return resp;
    }

    // Check if entity exists
    let entity_name = {
        let world = state.world.read().await;
        match world.entities.get(&id) {
            Some(e) => e.name.clone(),
            None => return error_json(StatusCode::NOT_FOUND, "Entity not found"),
        }
    };

    // Check if LLM is configured
    let api_key = read_env_var(&state.env_path, &state.settings.llm.api_key_name);
    if api_key.is_none() {
        let err = "LLM not configured. Add LLM_API_KEY to your .env file.".to_string();
        if let Ok(mut logger) = state.logger.lock() {
            logger.ensure_today(&PathBuf::from("logs"));
            logger.log_error(&err);
            logger.log_llm(
                "LLM",
                &req.context,
                "",
                0,
                false,
                &err,
                "LLM not configured"
            );
        }
        return success_json(serde_json::json!({
            "success": false,
            "llm_configured": false,
            "error": err,
            "raw_response": "",
            "message": "LLM not configured",
            "time_ms": 0u64
        })).into_response();
    }
    
    let api_url = &state.settings.llm.api_url.clone();
    let model = &state.settings.llm.model.clone();
    let api_key = api_key.unwrap();
    let max_tokens = state.settings.llm.max_output_tokens;
    let timeout_secs = state.settings.llm.llm_timeout_secs;
    
    // Build full URL: base + /v1/messages (Anthropic-compatible endpoint like OpenLife)
    let full_url = format!("{}/v1/messages", api_url.trim_end_matches('/'));
    
    let client = reqwest::Client::new();
    let system_prompt = "You are the world narrator for the Open World simulation. Respond ONLY with valid JSON (no other text before or after). Format: {\"action\":\"brief action name\",\"outcome\":\"2-3 sentences\",\"effects\":{{\"property_name\":change_value}},\"narrative\":\"story description\"}";
    let llm_request = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": req.context}
        ]
    });
    
    let start = std::time::Instant::now();
    let response = client
        .post(&full_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(timeout_secs))
        .json(&llm_request)
        .send()
        .await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    
    match response {
        Ok(res) => {
            let status_code = res.status();
            let body_text = res.text().await.unwrap_or_default();
            
            // Try to parse as JSON
            let body: serde_json::Value = match serde_json::from_str(&body_text) {
                Ok(v) => v,
                Err(_) => {
                    let err = format!("Failed to parse response: {}", body_text);
                    if let Ok(mut logger) = state.logger.lock() {
                        logger.ensure_today(&PathBuf::from("logs"));
                        logger.log_error(&err);
                        logger.log_llm(
                            "LLM",
                            &req.context,
                            &body_text,
                            elapsed_ms,
                            false,
                            "Response parse error",
                            &format!("Status: {}", status_code)
                        );
                    }
                    return success_json(serde_json::json!({
                        "success": false,
                        "error": format!("Failed to parse response: {}", body_text),
                        "llm_response_error": body_text,
                        "llm_status": status_code.to_string(),
                        "time_ms": elapsed_ms
                    })).into_response();
                }
            };
            
            // Check MiniMax error in base_resp (like OpenLife)
            if let Some(base_resp) = body.get("base_resp") {
                if let Some(status_code_val) = base_resp.get("status_code").and_then(|v| v.as_i64()) {
                    if status_code_val != 0 {
                        let msg = base_resp.get("status_msg").and_then(|v| v.as_str()).unwrap_or("Unknown error");
                        let err = format!("MiniMax API error ({}): {}", status_code_val, msg);
                        if let Ok(mut logger) = state.logger.lock() {
                            logger.ensure_today(&PathBuf::from("logs"));
                            logger.log_error(&err);
                            logger.log_llm(
                                "LLM",
                                &req.context,
                                &body_text,
                                elapsed_ms,
                                false,
                                &err,
                                "MiniMax API error"
                            );
                        }
                        return success_json(serde_json::json!({
                            "success": false,
                            "error": format!("MiniMax API error ({}): {}", status_code_val, msg),
                            "llm_response_error": body_text,
                            "llm_status": status_code.to_string(),
                            "time_ms": elapsed_ms
                        })).into_response();
                    }
                }
            }
            
            // Extract content blocks (MiniMax Anthropic-compatible format)
            let mut raw_response = String::new();
            let mut reasoning: Option<String> = None;
            
            if let Some(content) = body.get("content").and_then(|v| v.as_array()) {
                for block in content {
                    if let Some(block_type) = block.get("type").and_then(|v| v.as_str()) {
                        match block_type {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    raw_response.push_str(text);
                                }
                            },
                            "thinking" => {
                                if let Some(thinking) = block.get("thinking").and_then(|v| v.as_str()) {
                                    reasoning = Some(thinking.to_string());
                                }
                            },
                            _ => {}
                        }
                    }
                }
            }
            
            // Fallback: try OpenAI-style choices format
            if raw_response.is_empty() {
                if let Some(choices) = body.get("choices").and_then(|v| v.as_array()) {
                    if let Some(first) = choices.first() {
                        if let Some(msg) = first.get("message") {
                            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                                raw_response = content.to_string();
                            }
                        }
                    }
                }
            }
            
            if raw_response.is_empty() {
                let err = "Empty response from LLM".to_string();
                if let Ok(mut logger) = state.logger.lock() {
                    logger.ensure_today(&PathBuf::from("logs"));
                    logger.log_error(&err);
                    logger.log_llm(
                        "LLM",
                        &req.context,
                        &body_text,
                        elapsed_ms,
                        false,
                        &err,
                        "No content blocks found"
                    );
                }
                return success_json(serde_json::json!({
                    "success": false,
                    "error": "Empty response from LLM",
                    "raw_response": "",
                    "reasoning": reasoning,
                    "time_ms": elapsed_ms
                })).into_response();
            }
            
            // Log successful LLM call
            if let Ok(mut logger) = state.logger.lock() {
                logger.ensure_today(&PathBuf::from("logs"));
                logger.log_llm(
                    "LLM",
                    &req.context,
                    &raw_response,
                    elapsed_ms,
                    true,
                    "Success - response received",
                    &format!("Reasoning: {:?}", reasoning)
                );
            }

            // Report this LLM call back to the central selena-api tracker
            // so per-project / per-provider / per-model budgets reflect OW.
            // Fire-and-forget; does not block the response.
            //
            // As of 2026-06-04 this Rust-side call is the *sole* recorder
            // for OW entity-action LLM calls. The Python scheduler in
            // selena-project/code/scheduled_actions.py used to also call
            // record_llm_call here, but that produced phantom duplicates
            // in the events log (the scheduler is a *client* of this
            // server, not a peer recorder). Removing the scheduler's
            // record call was done in the same audit.
            //
            // We extract the real `usage` block from the MiniMax response
            // (Anthropic-compatible shape: `input_tokens`,
            // `output_tokens`) and the char counts of the request /
            // response / reasoning block, then forward them as query
            // params so the cost tracker can show real spend (not just
            // call counts) for the OW project.
            let (ti, to) = (
                body.get("usage").and_then(|u| u.get("input_tokens")).and_then(|v| v.as_i64()),
                body.get("usage").and_then(|u| u.get("output_tokens")).and_then(|v| v.as_i64()),
            );
            // Reasoning tokens (Anthropic / OpenAI both expose a
            // `completion_tokens_details.reasoning_tokens` style field;
            // the Anthropic-compatible shape often omits it, in which
            // case we approximate from the `thinking` block length if
            // present. 1 char ≈ 1 token for CJK reasoning text and
            // ~0.25 token/char for English — we just send the char
            // count and let the tracker divide if it needs to.  Best-
            // effort: stay None on anything that doesn't parse.
            let rt = body.get("usage")
                .and_then(|u| u.get("reasoning_tokens"))
                .and_then(|v| v.as_i64());
            let ci = req.context.len() as i64;
            let co = raw_response.len() as i64;
            let cr = reasoning.as_ref().map(|s| s.len() as i64).unwrap_or(0);
            record_llm_call_async(
                state.settings.llm.provider.clone(),
                model.clone(),
                "open-world-selena".to_string(),
                ti,
                to,
                rt,
                Some(ci),
                Some(co),
                Some(cr),
            );
            
            success_json(serde_json::json!({
                "success": true,
                "llm_configured": true,
                "entity_id": id.to_string(),
                "entity_name": entity_name,
                "raw_response": raw_response,
                "reasoning": reasoning,
                "time_ms": elapsed_ms
            })).into_response()
        }
        Err(e) => {
            let err = format!("Request failed: {}", e);
            if let Ok(mut logger) = state.logger.lock() {
                logger.ensure_today(&PathBuf::from("logs"));
                logger.log_error(&err);
                logger.log_llm(
                    "LLM",
                    &req.context,
                    "",
                    elapsed_ms,
                    false,
                    &err,
                    "Network/request error"
                );
            }
            success_json(serde_json::json!({
                "success": false,
                "error": format!("Request failed: {}", e),
                "llm_response_error": e.to_string(),
                "time_ms": elapsed_ms
            })).into_response()
        }
    }
}

// Entity action endpoint - generates an action for an entity
async fn entity_action(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    // Require authentication (LLM calls cost money)
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let world = state.world.read().await;
    
    // Find the entity
    let entity = match world.entities.get(&id) {
        Some(e) => e,
        None => return error_json(StatusCode::NOT_FOUND, "Entity not found"),
    };
    
    // Check for debug and process_only modes
    let is_debug = params.get("debug").map(|v| v == "true").unwrap_or(false);
    let process_only = params.get("process").map(|v| v == "true").unwrap_or(false);
    
    // If process_only, user should use the /action/process endpoint instead
    if process_only {
        return error_json(StatusCode::BAD_REQUEST, "Use /api/entities/:id/action/process for processing");
    }

    // Build the full action context (DRY helper shared with action_context_handler)
    let ctx = context_builder::build_action_context(
        &world,
        entity,
        state.settings.llm.default_max_history_summary_chars,
    );
    
    // Read the AI template
    let template = match tokio::fs::read_to_string("ai_templates/EntityAction.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    
    // Render the prompt
    let world_name = state.world.read().await.name.clone();
    let prompt = context_builder::build_action_prompt(&world_name, entity, &ctx, &template);
    
    // Check if LLM is configured
    let api_key = read_env_var(&state.env_path, &state.settings.llm.api_key_name);
    if api_key.is_none() {
        return success_json(serde_json::json!({
            "success": true,
            "action": "Development Mode",
            "outcome": "LLM not configured. Configure API key via .env file to enable AI-powered actions.",
            "effects": {},
            "narrative": format!("{} is considering their next move in the world, but the AI oracle has not yet awakened. Configure the LLM settings to bring the world to life.", entity.name),
            "debug": {
                "entity_id": id.to_string(),
                "entity_name": entity.name,
                "entity_type": entity.entity_type,
                "properties": entity.properties_int,
                "property_context": ctx.prop_context,
                "prompt": prompt,
                "llm_configured": false
            }
        })).into_response();
    }
    
    // Make LLM API call
    let api_url = &state.settings.llm.api_url;
    let model = &state.settings.llm.model;
    let api_key = api_key.unwrap();

    // Rate-limit gate (covers the legacy /api/entities/:id/action endpoint).
    if let Some(resp) = enforce_llm_rate_limit(
        &state.llm_rate_limiter,
        &format!("entity_action entity={}", id),
    ) {
        return resp;
    }
    
    let client = reqwest::Client::new();
    let llm_request = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": prompt}
        ]
    });
    
    let response = client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&llm_request)
        .send()
        .await;
    
    match response {
        Ok(res) => {
            if !res.status().is_success() {
                let status = res.status().to_string();
                let body = res.text().await.unwrap_or_default();
                let err = format!("LLM API error: {} - {}", status, body);
                // Log the full prompt + the API error so the failure is
                // discoverable via the 📜 Logs viewer (per Arcurus 2026-06-04:
                // 'log the full call instructions not only the results').
                if let Ok(mut logger) = state.logger.lock() {
                    logger.ensure_today(&PathBuf::from("logs"));
                    logger.log_error(&err);
                    logger.log_llm(
                        &entity.name,
                        &prompt,
                        &body,
                        0,
                        false,
                        &err,
                        &format!("Status: {} (entity_action)", status),
                    );
                }
                return success_json(serde_json::json!({
                    "success": false,
                    "error": err,
                    "debug": {
                        "entity_id": id.to_string(),
                        "entity_name": entity.name,
                        "prompt": prompt,
                        "llm_response_error": body,
                        "llm_status": status
                    }
                })).into_response();
            }
            
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            let raw_response = body["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
            
            // If debug mode, return raw response without processing
            if is_debug {
                return success_json(serde_json::json!({
                    "success": true,
                    "debug_mode": true,
                    "entity_id": id.to_string(),
                    "prompt": prompt,
                    "raw_response": raw_response,
                    "llm_response": body,
                    "message": "Debug mode: Raw LLM response returned. Call with ?process=true and raw_response in body to apply effects."
                })).into_response();
            }
            
            // Parse and apply the LLM response
            match parse_llm_action_response(&raw_response) {
                Ok((action_data, repair_warning)) => {
                    // Apply effects to entity
                    drop(world); // Release read lock
                    let mut world = state.world.write().await;
                    if let Some(entity) = world.entities.get_mut(&id) {
                        let mut applied_effects = std::collections::HashMap::new();
                        for (prop_key, change_val) in &action_data.effects {
                            let change = match parse_effect_value(change_val) {
                                Some(c) => c,
                                None => continue,
                            };
                            if !entity.properties_int.contains_key(prop_key) {
                                entity.properties_int.insert(prop_key.clone(), 0);
                            }
                            if let Some(value) = entity.properties_int.get_mut(prop_key) {
                                let old_value = *value;
                                *value = (*value as f64 + change) as i64;
                                applied_effects.insert(prop_key.clone(), *value - old_value);
                            }
                        }

                        // Surface the tolerant-repair warning (if any)
                        // in the response so the auto-call path is
                        // also observable. Auto-call responses
                        // historically had no `warnings` field, so
                        // we add it. It is empty Vec on the common
                        // (strict-parse-ok) path.
                        let response_warnings: Vec<String> =
                            repair_warning.into_iter().collect();

                        success_json(serde_json::json!({
                            "success": true,
                            "action": action_data.action,
                            "outcome": action_data.outcome,
                            "effects_applied": applied_effects,
                            "narrative": action_data.narrative,
                            "warnings": response_warnings
                        })).into_response()
                    } else {
                        error_json(StatusCode::NOT_FOUND, "Entity not found")
                    }
                }
                Err(e) => {
                    success_json(serde_json::json!({
                        "success": false,
                        "error": format!("Failed to parse LLM response: {}", e),
                        "raw_response": raw_response,
                        "debug": {
                            "entity_id": id.to_string(),
                            "entity_name": entity.name,
                            "prompt": prompt
                        }
                    })).into_response()
                }
            }
        }
        Err(e) => {
            success_json(serde_json::json!({
                "success": false,
                "error": format!("Failed to call LLM: {}", e),
                "debug": {
                    "entity_id": id.to_string(),
                    "entity_name": entity.name,
                    "prompt": prompt
                }
            })).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// `history_summary_replace` (LLM-emit surgical edits)
//
// The LLM can include this field in its response (instead of, or in
// addition to, `history_summary`) to make small corrections to the
// current history_summary without rewriting the whole thing. Per
// Arcurus 2026-06-04 (#openworld), this is the LLM-driven analog of
// the API endpoint `POST /api/entities/:id/history-summary/replace`.
//
// The field accepts either a single `{old_part, new_part}` object or
// an array of such objects, so the LLM can do one edit or many in a
// single turn. The deserializer uses `#[serde(untagged)]` on an enum
// to handle both shapes transparently.
//
// The full behavior (per Arcurus 2026-06-04, extended 2026-06-05):
//   - Both `history_summary` AND `history_summary_replace` present:
//     replace wins, `history_summary` is dropped, warning logged.
//   - Only `history_summary_replace` present: apply chain in order.
//   - Only `history_summary` present: full replace (existing behavior).
//   - Neither present: no change to summary, warning logged.
//   - For each replace:
//       * `old_part == "!ALL!"`: full replace — set state to `new_part`,
//         ignoring current. No warning. Subsequent replaces in the
//         chain still apply (e.g. [!ALL! + append] is valid).
//       * `old_part == ""`: append `new_part` to current; no warning.
//         If current is empty, `new_part` becomes the new summary.
//       * `old_part != ""` + found: replace first occurrence.
//       * `old_part != ""` + not found: skip, warning logged.
//       * `old_part != ""` + current is empty: skip, warning logged.
//   - After chain: truncate to `max_chars` if over (warning logged).
//
// Both the LLM-emit path and the `POST /api/entities/:id/history-summary/replace`
// API endpoint funnel through this function, so the !ALL! convention
// (and every other rule) is identical for LLM and API callers. The
// API handler adds the `not_found_is_error` toggle on top of the
// "not found" warning.
//
// The LLM template wording (ai_templates/EntityAction.md) intentionally
// omits the edge-case semantics above so the instructions stay on-point
// and terse. The error/warn rows are operator-visible only via the
// 📜 Logs viewer.

#[derive(Debug, Clone, Deserialize)]
pub struct HistorySummaryReplace {
    pub old_part: String,
    pub new_part: String,
}

/// Accepts either a single `{old_part, new_part}` object or an array
/// of such objects. Untagged so the LLM can use either shape freely.
///
/// Custom `Deserialize` (instead of `#[derive(Deserialize)]` +
/// `#[serde(untagged)]`) so we can tolerate the empty shapes `{}`
/// and `[]` — observed 2026-06-05 in llm-log: the world clock
/// emitted `"history_summary_replace":{}` alongside a full
/// `history_summary`, and the default untagged decoder rejected
/// the response with "data did not match any variant of untagged
/// enum HistorySummaryReplaceOneOrMany" (which caused the entire
/// LLM response to be dropped, losing the `history_summary` too).
/// Both `{}` and `[]` are now treated as `Many(vec![])` — a no-op
/// replace chain — so the rest of the response still parses and
/// the `history_summary` field (if present) is still applied.
#[derive(Debug)]
pub enum HistorySummaryReplaceOneOrMany {
    Single(HistorySummaryReplace),
    Many(Vec<HistorySummaryReplace>),
}

impl HistorySummaryReplaceOneOrMany {
    /// Flatten to a `Vec<HistorySummaryReplace>` regardless of the
    /// original shape. Single becomes a vec of one; Many is cloned.
    pub fn into_vec(self) -> Vec<HistorySummaryReplace> {
        match self {
            HistorySummaryReplaceOneOrMany::Single(r) => vec![r],
            HistorySummaryReplaceOneOrMany::Many(rs) => rs,
        }
    }
}

impl<'de> Deserialize<'de> for HistorySummaryReplaceOneOrMany {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            // Tolerate `{}` and `[]` from the LLM (a stub empty replace).
            serde_json::Value::Object(ref map) if map.is_empty() => {
                Ok(HistorySummaryReplaceOneOrMany::Many(Vec::new()))
            }
            serde_json::Value::Array(ref items) if items.is_empty() => {
                Ok(HistorySummaryReplaceOneOrMany::Many(Vec::new()))
            }
            // Otherwise defer to the original untagged dispatch.
            other => {
                #[derive(Deserialize)]
                #[serde(untagged)]
                enum Raw {
                    Single(HistorySummaryReplace),
                    Many(Vec<HistorySummaryReplace>),
                }
                let raw = Raw::deserialize(other).map_err(|e| {
                    D::Error::custom(format!(
                        "history_summary_replace: expected \
                         {{old_part, new_part}} or array, or empty \
                         {{}}/[]; got: {}",
                        e
                    ))
                })?;
                Ok(match raw {
                    Raw::Single(s) => HistorySummaryReplaceOneOrMany::Single(s),
                    Raw::Many(v) => HistorySummaryReplaceOneOrMany::Many(v),
                })
            }
        }
    }
}

/// Result of applying a chain of replaces to a history summary.
pub struct ApplyReplaceResult {
    /// New summary string, or None if the chain produced no content
    /// (e.g. all replaces were no-ops on an empty current).
    pub new_summary: Option<String>,
    /// True if the result was truncated to fit `max_chars`.
    pub truncated: bool,
    /// Per-replace warnings (old_part not found, current empty, etc.).
    /// Operator-facing, surfaced via the LLM log + response.
    pub warnings: Vec<String>,
}

/// Apply a chain of `HistorySummaryReplace` operations to a current
/// history_summary. Pure function (no I/O, no logging) — the caller
/// is responsible for logging the warnings and applying the new
/// summary to the entity.
///
/// `current` is `Option<&str>` so the caller can pass
/// `entity.history_summary.as_deref()` directly. `None` and `Some("")`
/// are treated identically (no current content to search or append to).
///
/// See the behavior matrix above for the full semantics.
///
/// (Per Arcurus 2026-06-04 #openworld: the strict find() below was
/// upgraded to use `find_replace_range` so a stray period or run of
/// extra spaces in old_part does NOT trigger the 'old_part not found'
/// warning. The warning should fire only when the phrase is truly
/// absent or meaningfully different.)
/// Find the byte range in `haystack` that matches `needle`, with light
/// fuzziness for trivial mismatches:
///
///   1. Strict substring match (fast path).
///   2. Whitespace-normalized match: collapse runs of whitespace to a
///      single space on both sides, tokenize, and accept if a window of
///      needle tokens appears as a contiguous subsequence of haystack
///      tokens. This handles cases like "met  the  dragon" vs
///      "met the dragon", or "  met the dragon" vs "met the dragon".
///   3. Trailing-punctuation strip: if the normalized match still
///      fails, strip trailing `.,;:!?"')` from each side of `needle`
///      and try the strict match again. This handles "saw a dragon."
///      vs "saw a dragon", or "scout, ready" vs "scout, ready."
///
/// Returns `(start, end)` byte indices in `haystack` (safe to slice on,
/// since both branches land on token boundaries), or `None` if no
/// match is found. No warnings are emitted by this helper — the caller
/// decides what to do with `None`.
///
/// Implementation note: we tokenize manually (not via regex) so we
/// keep the original byte positions for the slice. O(n) in haystack
/// length, no allocations beyond a small Vec of token slices.
fn find_replace_range(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    // 1. Strict match (fast path).
    if let Some(idx) = haystack.find(needle) {
        return Some((idx, idx + needle.len()));
    }

    // Tokenize: split on whitespace, keep byte ranges of each token
    // in the original `s` so we can map back to byte positions.
    // Free function (not a closure) to avoid lifetime elision issues.
    fn tokenize(s: &str) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut token_start: Option<usize> = None;
        for (i, c) in s.char_indices() {
            if c.is_whitespace() {
                if let Some(start) = token_start {
                    out.push((start, i));
                    token_start = None;
                }
            } else if token_start.is_none() {
                token_start = Some(i);
            }
        }
        if let Some(start) = token_start {
            out.push((start, s.len()));
        }
        out
    }

    // 2. Whitespace-normalized match.
    let h_tokens = tokenize(haystack);
    let n_tokens = tokenize(needle);
    if n_tokens.is_empty() || h_tokens.len() < n_tokens.len() {
        // fall through to punctuation strip
    } else {
        'outer: for window in h_tokens.windows(n_tokens.len()) {
            for (h, n) in window.iter().zip(n_tokens.iter()) {
                if &haystack[h.0..h.1] != &needle[n.0..n.1] {
                    continue 'outer;
                }
            }
            let start = window.first().unwrap().0;
            let end = window.last().unwrap().1;
            return Some((start, end));
        }
    }

    // 3. Trailing-punctuation strip on needle, then strict retry.
    let n_stripped: &str = needle.trim_end_matches(|c: char| {
        matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | ')')
    });
    if n_stripped != needle && !n_stripped.is_empty() {
        if let Some(idx) = haystack.find(n_stripped) {
            return Some((idx, idx + n_stripped.len()));
        }
    }

    None
}

pub fn apply_history_summary_replaces(
    current: Option<&str>,
    replaces: &[HistorySummaryReplace],
    max_chars: usize,
) -> ApplyReplaceResult {
    let mut warnings: Vec<String> = Vec::new();
    let mut state: String = current.unwrap_or("").to_string();

    for (i, rep) in replaces.iter().enumerate() {
        if rep.old_part == "!ALL!" {
            // Full replace convention. The history is rewritten in
            // full from `new_part`; the existing `state` is discarded.
            // This is the `old_part == "!ALL!"` branch — the LLM or
            // API caller uses this when they need to do a full
            // restructure of the summary (per Arcurus 2026-06-05).
            // No warning is logged: the call is by design. Subsequent
            // replaces in the chain still apply (e.g. [!ALL! + append]).
            state = rep.new_part.clone();
            continue;
        } else if rep.old_part.is_empty() {
            // Append path. Per the rule: no warning logged.
            if state.is_empty() {
                // No current content: new_part IS the new state.
                state = rep.new_part.clone();
            } else {
                state.push_str(&rep.new_part);
            }
        } else {
            if state.is_empty() {
                // Non-empty old_part but nothing to search in: skip
                // with a warning. The LLM probably wanted to do a
                // find-replace but the summary is empty; the safest
                // behavior is to skip and let the operator decide.
                warnings.push(format!(
                    "history_summary_replace[{}]: current summary is empty; cannot search for non-empty old_part; skipped",
                    i
                ));
                continue;
            }
            match find_replace_range(&state, &rep.old_part) {
                Some((idx, end)) => {
                    // find() returns a char-boundary by definition,
                    // so the byte index is safe to slice at.
                    let mut s = String::with_capacity(state.len() + rep.new_part.len());
                    s.push_str(&state[..idx]);
                    s.push_str(&rep.new_part);
                    s.push_str(&state[end..]);
                    state = s;
                }
                None => {
                    // Per the rule: not-found is a WARNING, not an
                    // error. The rest of the chain still applies.
                    warnings.push(format!(
                        "history_summary_replace[{}]: old_part not found in current summary; skipped",
                        i
                    ));
                }
            }
        }
    }

    // Truncate if over cap. Same strategy as the API endpoint: cut
    // from the END (keeps the start + any freshly inserted new_part
    // intact), append "…" on the boundary.
    let (final_summary, truncated) = if state.chars().count() > max_chars {
        let keep = max_chars.saturating_sub(1);
        let cut: String = state.chars().take(keep).collect();
        let truncated_str = format!("{}…", cut);
        (truncated_str, true)
    } else {
        (state, false)
    };

    if truncated {
        warnings.push(format!(
            "history_summary exceeded {} chars after replace chain; truncated.",
            max_chars
        ));
    }

    // Don't set to Some("") if there's no content. Keeps the
    // distinction between "never had a summary" (None) and
    // "had a summary that was just emptied" (Some("")).
    let new_summary = if final_summary.is_empty() {
        None
    } else {
        Some(final_summary)
    };

    ApplyReplaceResult {
        new_summary,
        truncated,
        warnings,
    }
}

// Helper struct for parsing LLM response
#[derive(Debug, serde::Deserialize)]
struct LlmActionResponse {
    action: String,
    outcome: String,
    #[serde(default)]
    effects: std::collections::HashMap<String, serde_json::Value>,
    narrative: String,
    /// Per-entity rolling history summary. The LLM is instructed
    /// (in EntityAction.md) to always include an updated version.
    /// Optional: if the LLM omits it the existing summary is left
    /// untouched. Hard-capped server-side to
    /// `world.settings.max_history_summary_chars` (truncated with "…").
    #[serde(default)]
    history_summary: Option<String>,
    /// 2026-06-04: LLM-emit surgical-edits command. Either a single
    /// `{old_part, new_part}` object or an array of such objects.
    /// If present, the replace chain wins over `history_summary`
    /// (which is dropped with a warning). See
    /// `apply_history_summary_replaces` for full semantics.
    #[serde(default)]
    history_summary_replace: Option<HistorySummaryReplaceOneOrMany>,
}

// Parse a JSON value as an effect change (handles string "+5", "5", or number 5)
fn parse_effect_value(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            // Remove leading + if present, try to parse
            let cleaned = s.trim_start_matches('+');
            cleaned.parse::<f64>().ok()
        }
        _ => None,
    }
}

// Magnitude bounds applied to LLM-supplied effect values to keep
// them from poisoning entity state. See todo 2df49bd8.
pub(crate) const MAX_INT_ABS: i64 = 1_000_000_000;
pub(crate) const MAX_FLOAT_ABS: f64 = 1.0e9;
pub(crate) const MAX_DELTA_ABS: f64 = 1.0e6;

/// Reject obviously-broken LLM effect values: non-finite (NaN / Inf)
/// numbers, or any delta whose absolute value exceeds MAX_DELTA_ABS.
/// Returns a human-readable reason when the value should be skipped,
/// or None when the value is in range and may be applied.
pub(crate) fn magnitude_check(v: &serde_json::Value) -> Option<String> {
    let f = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None, // bool / null / array / object handled elsewhere
    };
    if !f.is_finite() {
        return Some(format!("non-finite value ({})", f));
    }
    if f.abs() > MAX_DELTA_ABS {
        return Some(format!("|{}| > MAX_DELTA_ABS ({})", f, MAX_DELTA_ABS as i64));
    }
    None
}

/// True when a freshly-computed int result is outside the safe
/// magnitude bound and should be skipped rather than written.
pub(crate) fn int_oversize(v: i64) -> bool {
    v.abs() > MAX_INT_ABS
}

/// True when a freshly-computed float result is outside the safe
/// magnitude bound (or non-finite) and should be skipped.
pub(crate) fn float_oversize(v: f64) -> bool {
    !v.is_finite() || v.abs() > MAX_FLOAT_ABS
}

// Parse the JSON response from LLM
//
// On success, returns the parsed `LlmActionResponse` together with
// an optional repair warning. The warning is `Some(_)` when the
// tolerant `fix_known_malformed_patterns` regex-fixup actually fired
// and rescued a strict-parse failure, and `None` when the strict
// `serde_json` parse succeeded on the first try. Returning the
// warning (rather than swallowing it) keeps the recovery observable:
// the World Clock's recurring `,"":"old_part"|"new_part"` empty-key
// malformation (per Arcurus 2026-06-05 #openworld) used to be
// silently rescued by the regex, which made the bug invisible to
// the operator. Callers should surface the warning in the response
// payload and/or in the LLM-call log.
fn parse_llm_action_response(raw: &str) -> Result<(LlmActionResponse, Option<String>), String> {
    // Try to extract JSON from the response (in case there's extra text)
    let json_str = if let Some(start) = raw.find('{') {
        if let Some(end) = raw.rfind('}') {
            &raw[start..=end]
        } else {
            raw
        }
    } else {
        raw
    };

    // First try the strict parse.
    match serde_json::from_str::<LlmActionResponse>(json_str) {
        Ok(parsed) => Ok((parsed, None)),
        Err(e) => {
            // Strict parse failed. Try a tolerant fixup pass before
            // giving up — see `fix_known_malformed_patterns` for the
            // specific patterns we know how to repair. Per Arcurus
            // 2026-06-05 #openworld: the World Clock entity has been
            // emitting a `history_summary_replace` array whose first
            // element has a spurious `"" : "new_part"` key+value
            // before the actual `"new_part"` field, e.g.
            //   {"old_part":"...","":"new_part":"..."}
            // instead of
            //   {"old_part":"...","new_part":"..."}
            // which serde rejects. Rather than rejecting the whole
            // response (and losing the `action`, `effects`, `narrative`,
            // and the `history_summary` that often comes with it), we
            // run a conservative regex repair and retry.
            let repaired = fix_known_malformed_patterns(json_str);
            if repaired != json_str {
                match serde_json::from_str::<LlmActionResponse>(&repaired) {
                    Ok(parsed) => {
                        // Successful repair — return the parsed value
                        // together with a warning string so the caller
                        // can surface it in the response and / or the
                        // LLM-call log. The warning is descriptive
                        // (says "this was a regex repair of the
                        // known empty-key bug") so a future operator
                        // grepping the JSONL log for it can tell at
                        // a glance how often the World Clock bug
                        // recurs.
                        Ok((parsed, Some(format!(
                            "parse_llm_action_response: LLM response matched a known \
                             malformed pattern and was repaired (regex fixup of the \
                             \"\":\"old_part\"|\"new_part\" empty-key bug seen in \
                             history_summary_replace)."
                        ))))
                    }
                    Err(e2) => Err(format!(
                        "JSON parse error: {} - Input (after repair attempt): {}",
                        e2, repaired
                    )),
                }
            } else {
                Err(format!("JSON parse error: {} - Input: {}", e, json_str))
            }
        }
    }
}

/// Conservative regex-based repair for known LLM JSON malformation
/// patterns. Only touches the specific shapes we've actually seen in
/// the field; if a different bug appears later, the strict parse
/// still fails and we surface the error normally. Returns the
/// (possibly modified) string. If no repair was needed, returns the
/// input unchanged.
///
/// Currently repairs:
///   1. `,"":"new_part":"VALUE"` → `,"new_part":"VALUE"`
///   2. `,"":"old_part":"VALUE"` → `,"old_part":"VALUE"`
/// i.e. the LLM emitted a spurious empty-string key whose value is
/// the *name* of the next real key, then forgot the comma and wrote
/// `: actual_value` directly. This is the World Clock's recurring
/// `history_summary_replace` bug (per Arcurus 2026-06-05 #openworld).
fn fix_known_malformed_patterns(s: &str) -> String {
    // Conservative: only match the two known key names. If the
    // bogus value is anything else we leave the string alone so
    // we don't accidentally mangle a real (if weird) object.
    // No Lazy/OnceLock caching: this only runs on the parse-error
    // path, which is rare, so the regex construction cost is
    // negligible. (We avoid pulling in once_cell just for this.)
    let pattern = regex::Regex::new(r#","":\s*"(old_part|new_part)":"#)
        .expect("fix_known_malformed_patterns: regex compiles");
    pattern.replace_all(s, |caps: &regex::Captures| {
        format!(r#","{}":"#, &caps[1])
    }).into_owned()
}

// Process LLM action response - apply effects from raw LLM response
async fn process_action_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ProcessActionRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let entity_id = req.entity_id;
    let raw_response = &req.raw_response;
    
    // Parse the LLM response
    match parse_llm_action_response(raw_response) {
        Ok((action_data, repair_warning)) => {
            // Apply effects to entity
            let mut world = state.world.write().await;
            // Capture the history-summary cap BEFORE taking a mutable
            // borrow of the entity (Rust borrow checker: we can't
            // immutably borrow `world.settings` while `entity` is
            // borrowed mutably). Resolved effective cap: per-world
            // override (if non-zero) or the global default from
            // `settings.json → llm.default_max_history_summary_chars`.
            let max_summary_chars = context_builder::resolve_max_history_summary_chars(
                &world,
                state.settings.llm.default_max_history_summary_chars,
            ) as usize;
            if let Some(entity) = world.entities.get_mut(&entity_id) {
                let mut applied_effects = std::collections::HashMap::new();
                let mut new_values = std::collections::HashMap::new();
                let mut warnings: Vec<String> = Vec::new();

                // Surface the tolerant-repair warning (if any) first
                // so it shows up at the top of the warnings vec in
                // the response and the LLM-call log. This makes
                // the regex-fixup recovery visible to operators
                // (per Arcurus 2026-06-05 #openworld: the World
                // Clock's empty-key bug is recurring, and we need
                // to know when our repair actually fires).
                if let Some(w) = repair_warning {
                    warnings.push(w);
                }

                // System entities (world_clock, meta-tagged) are protected
                // from LLM-driven property writes to prevent integer/float
                // corruption of world state. The action is still recorded
                // in history and the durable JSONL log, but effects are
                // rejected with a warning. See todo c7f3bc27.
                let protected_entity = entity.is_system_entity();
                if protected_entity {
                    warnings.push(format!(
                        "Entity is a system entity (type={}, tags={:?}); LLM effect writes blocked.",
                        entity.entity_type, entity.tags
                    ));
                }

                for (prop_key, change_val) in &action_data.effects {
                    if protected_entity {
                        // Skip writing effects on system entities but keep
                        // tracking the count for the response payload.
                        warnings.push(format!(
                            "Skipped effect on system entity: {}={:?}",
                            prop_key, change_val
                        ));
                        continue;
                    }

                    // Magnitude guard: LLM occasionally emits huge garbage
                    // values (e.g. 1e18 for "power") that get added to the
                    // property and corrupt the entity forever. Cap the
                    // absolute size of both the delta and the result so a
                    // single bad response can't poison world state. See
                    // todo 2df49bd8.
                    if let Some(reason) = magnitude_check(change_val) {
                        warnings.push(format!(
                            "Skipped effect on '{}': {} (value={:?})",
                            prop_key, reason, change_val
                        ));
                        continue;
                    }

                    // Determine value type and convert appropriately
                    let (int_val, float_val, string_val) = match change_val {
                        // Bool -> int (true=1, false=0)
                        serde_json::Value::Bool(b) => {
                            let v = if *b { 1 } else { 0 };
                            (Some(v as i64), None, None)
                        }
                        // Number -> int or float. Anything with a decimal point is float.
                        serde_json::Value::Number(n) => {
                            if n.is_i64() {
                                // Pure integer -> int property
                                (Some(n.as_i64().unwrap()), None, None)
                            } else {
                                // Float (including 1.0, 0.5, etc.) -> float property
                                (None, n.as_f64(), None)
                            }
                        }
                        // String -> parse as number or string
                        serde_json::Value::String(s) => {
                            let trimmed = s.trim();
                            // Try parsing as integer first
                            if let Ok(v) = trimmed.parse::<i64>() {
                                (Some(v), None, None)
                            // Then as float (anything with a decimal point)
                            } else if let Ok(v) = trimmed.parse::<f64>() {
                                if v.fract() == 0.0 && v.abs() < 1e15 && !trimmed.contains('.') {
                                    // Looks like int in string form (no decimal point)
                                    (Some(v as i64), None, None)
                                } else {
                                    // Has decimal point or looks like a float -> float property
                                    (None, Some(v), None)
                                }
                            } else {
                                // Not a number -> string property
                                (None, None, Some(trimmed.to_string()))
                            }
                        }
                        // Null, Array, Object -> skip with warning
                        _ => {
                            warnings.push(format!("Unsupported effect type for '{}': {:?}", prop_key, change_val));
                            continue;
                        }
                    };
                    
                    // Check which property stores this key (int, float, or string)
                    let is_in_int = entity.properties_int.contains_key(prop_key);
                    let is_in_float = entity.properties_float.contains_key(prop_key);
                    let is_in_string = entity.properties_string.contains_key(prop_key);
                    
                    // Apply to the existing property type, or create the right type
                    if let Some(val) = int_val {
                        if is_in_float {
                            warnings.push(format!("Type mismatch: '{}' is float, tried to set int ({}). Skipped.", prop_key, val));
                            continue;
                        }
                        if is_in_string {
                            warnings.push(format!("Type mismatch: '{}' is string, tried to set int ({}). Skipped.", prop_key, val));
                            continue;
                        }
                        
                        // Get or create the int property
                        let old_val = *entity.properties_int.get(prop_key).unwrap_or(&0);
                        let new_val = old_val.checked_add(val).unwrap_or_else(|| {
                            warnings.push(format!(
                                "Integer overflow on '{}' (old={}, delta={}); clamped to {}",
                                prop_key, old_val, val, MAX_INT_ABS
                            ));
                            MAX_INT_ABS
                        });
                        if int_oversize(new_val) {
                            warnings.push(format!(
                                "Skipped oversize int write on '{}': new_val={} (|val| > {})",
                                prop_key, new_val, MAX_INT_ABS
                            ));
                            continue;
                        }
                        entity.properties_int.insert(prop_key.clone(), new_val);
                        applied_effects.insert(prop_key.clone(), serde_json::json!(new_val));
                        new_values.insert(prop_key.clone(), serde_json::json!(new_val));
                    } else if let Some(val) = float_val {
                        if is_in_int {
                            warnings.push(format!("Type mismatch: '{}' is int, tried to set float ({}). Skipped.", prop_key, val));
                            continue;
                        }
                        if is_in_string {
                            warnings.push(format!("Type mismatch: '{}' is string, tried to set float ({}). Skipped.", prop_key, val));
                            continue;
                        }
                        
                        let old_val = *entity.properties_float.get(prop_key).unwrap_or(&0.0);
                        let new_val = old_val + val;
                        if float_oversize(new_val) {
                            warnings.push(format!(
                                "Skipped oversize float write on '{}': new_val={}",
                                prop_key, new_val
                            ));
                            continue;
                        }
                        entity.properties_float.insert(prop_key.clone(), new_val);
                        applied_effects.insert(prop_key.clone(), serde_json::json!(new_val));
                        new_values.insert(prop_key.clone(), serde_json::json!(new_val));
                    } else if let Some(ref val) = string_val {
                        if is_in_int {
                            warnings.push(format!("Type mismatch: '{}' is int, tried to set string value={:?}. Skipped.", prop_key, val));
                            continue;
                        }
                        if is_in_float {
                            warnings.push(format!("Type mismatch: {} is float, tried to set string (val=\"{}\"). Skipped.", prop_key, val));
                            continue;
                        }
                        
                        // String: set the value (not additive, just replace)
                        entity.properties_string.insert(prop_key.clone(), val.clone());
                        applied_effects.insert(prop_key.clone(), serde_json::json!(val));
                        new_values.insert(prop_key.clone(), serde_json::json!(val));
                    }
                }

                // NOTE: log_llm() + history_entry build are deferred
                // to AFTER the history_summary handling below, so the
                // `warnings` vec they capture includes "Both" / "Neither"
                // / truncation warnings. Moved per Arcurus 2026-06-04
                // (#openworld): the old position logged `Warnings: []`
                // because history_summary handling hadn't run yet.

                // Record the action in the entity's in-memory history.
                // Per Arcurus 2026-06-03 (#openworld): "create new file
                // saving the history of the world actions for a given
                // entity and display it in the open world selena web
                // interface if you open the entity".  Two stores:
                //   (a) entity.history (in-memory + save.owbl)
                //   (b) world_data/action_history.jsonl (durable, independent
                //       of save.owbl, survives save corruption).
                use world_data::entity_history::add_to_history;
                let history_timestamp = chrono::Utc::now();
                add_to_history(
                    entity,
                    &action_data.action,
                    &action_data.narrative,
                    &action_data.outcome,
                );

                // Build the durable JSONL entry. Deferred to AFTER
                // history_summary handling so its `warnings` field
                // captures the full set (including the "Both dropped"
                // / "Neither skipped" / "truncated" warnings from
                // apply_history_summary_replaces).
                // see NOTE above — will be built right before log_llm().

                // Apply the LLM-supplied history summary. Three cases
                // (per Arcurus 2026-06-04 #openworld):
                //   1. history_summary_replace present → replace wins.
                //      If history_summary is also present, it's dropped
                //      with a warning.
                //   2. Only history_summary present → full replace
                //      (existing behavior; truncated if over cap).
                //   3. Neither present → no change to summary, warning
                //      logged.
                //
                // We hard-cap the length to max_summary_chars so a
                // runaway LLM can't bloat the save file. Truncation is
                // a soft contract (truncate with "…"), not a reject.
                let mut summary_truncated = false;
                let applied_summary: Option<String> =
                    if let Some(replaces_one_or_many) = action_data.history_summary_replace {
                        // Case 1: history_summary_replace (LLM-emit
                        // surgical edits). replace wins; if the LLM
                        // also sent a full `history_summary`, drop it
                        // with a warning.
                        if action_data.history_summary.is_some() {
                            warnings.push(
                                "Both history_summary and history_summary_replace present; \
                                 using replace (history_summary dropped)".to_string()
                            );
                        }
                        // Borrow current state immutably, then release
                        // the borrow before assigning to the entity.
                        let replaces_vec: Vec<HistorySummaryReplace> =
                            replaces_one_or_many.into_vec();
                        let result = {
                            let current = entity.history_summary.as_deref();
                            apply_history_summary_replaces(
                                current,
                                &replaces_vec,
                                max_summary_chars,
                            )
                            // current borrow released here
                        };
                        for w in result.warnings {
                            warnings.push(w);
                        }
                        if result.truncated {
                            summary_truncated = true;
                        }
                        entity.history_summary = result.new_summary.clone();
                        result.new_summary
                    } else if let Some(s) = action_data.history_summary {
                        // Case 2: full replace (existing behavior).
                        let trimmed = s.trim();
                        if trimmed.is_empty() || is_placeholder_summary(trimmed) {
                            // LLM emitted a "placebo" summary (just "…",
                            // "—", ".", or other non-meaningful content)
                            // or sent an empty string. Either way, treat
                            // as no summary so the entity has a clear
                            // "no summary" state (None) instead of a
                            // useless 1-char placeholder. The 124-entry
                            // Shadow Crown bug (history_summary="…" while
                            // history.len()=124) was caused by the missing
                            // is_placeholder_summary check here.
                            entity.history_summary = None;
                            None
                        } else if trimmed.chars().count() > max_summary_chars {
                            let cut: String =
                                trimmed.chars().take(max_summary_chars.saturating_sub(1)).collect();
                            // If max_summary_chars was 0 (degenerate),
                            // `cut` is empty and `format!("{}…", "")`
                            // would produce a bare "…". Guard against
                            // that so we don't store another placeholder.
                            let final_summary = if cut.is_empty() {
                                String::new()
                            } else {
                                format!("{}…", cut)
                            };
                            if final_summary.is_empty() {
                                entity.history_summary = None;
                                summary_truncated = true;
                                None
                            } else {
                                entity.history_summary = Some(final_summary.clone());
                                summary_truncated = true;
                                Some(final_summary)
                            }
                        } else {
                            entity.history_summary = Some(trimmed.to_string());
                            Some(trimmed.to_string())
                        }
                    } else {
                        // Case 3: neither field present. No change to
                        // summary; surface a warning so the operator
                        // can see the LLM skipped it.
                        warnings.push(
                            "Neither history_summary nor history_summary_replace in LLM response; \
                             summary not updated".to_string()
                        );
                        entity.history_summary.clone()
                    };
                if summary_truncated {
                    warnings.push(format!(
                        "LLM history_summary exceeded {} chars; truncated.",
                        max_summary_chars
                    ));
                }

                // --- Deferred writes (history_entry + log_llm) ---
                // Now that `warnings` is fully populated (effects
                // warnings + history_summary "Both"/"Neither"/"truncated"
                // warnings), build the durable JSONL entry and write
                // the LLM log. This replaces the old position that
                // ran BEFORE history_summary handling, where the
                // logged warnings vec was always empty.
                // (Per Arcurus 2026-06-04 #openworld.)
                let history_entry = ActionHistoryEntry {
                    entity_id: entity_id.to_string(),
                    entity_name: entity.name.clone(),
                    timestamp: history_timestamp,
                    action: action_data.action.clone(),
                    outcome: action_data.outcome.clone(),
                    details: action_data.narrative.clone(),
                    effects: action_data.effects.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    warnings: warnings.clone(),
                };
                let parsing_outcome = format!(
                    "Applied {} effects. Warnings: {:?}",
                    applied_effects.len(),
                    warnings
                );
                if let Ok(mut logger) = state.logger.lock() {
                    logger.ensure_today(&PathBuf::from("logs"));
                    logger.log_llm(
                        // New visual format: label is the action name +
                        // entity, so the log header reads e.g.
                        // "** descend_to_boundary_fracture of Velora the
                        // Undying - 2026-06-05 18:47:59 **" and the call
                        // boundary is obvious at a glance.
                        &format!("{} of {}", action_data.action, entity.name),
                        &format!("Process action for entity {}: {}", entity_id, action_data.action),
                        raw_response,
                        0,
                        true,
                        &parsing_outcome,
                        &format!("Effects: {:?}", action_data.effects)
                    );
                }

                // Release the borrow by extracting the response payload.
                let response_json = serde_json::json!({
                    "success": true,
                    "action": action_data.action,
                    "outcome": action_data.outcome,
                    "effects_applied": applied_effects,
                    "new_values": new_values,
                    "narrative": action_data.narrative,
                    "history_summary": applied_summary,
                    "warnings": warnings,
                });
                let response = success_json(response_json).into_response();
                // `entity` and `world` mutable borrows end here.

                // Bump world action counters. These fields are persisted
                // in save.owbl (see World.rs:114,118 and persistence.rs
                // read/write), but no code path was ever setting them —
                // both stayed at their defaults (0 and None) forever.
                // The API at /api/ always reported "action_count": 0
                // and "last_world_action": null regardless of activity,
                // even though world_data/action_history.jsonl had 1958+
                // entries. This fixes that long-standing observability
                // bug. Relates to: e23e3910 (P6, World mechanics
                // improvements).
                world.action_count = world.action_count.saturating_add(1);
                world.last_world_action = Some(chrono::Utc::now());

                // Persist world after applying LLM action effects.
                if let Err(save_err) = BinaryPersistence::save_world(&world, &state.save_path) {
                    eprintln!("[process_action] auto-save failed: {}", save_err);
                    if let Ok(mut logger) = state.logger.lock() {
                        logger.log_error(&format!("auto-save after process_action failed: {}", save_err));
                    }
                }

                // Append to the durable JSONL log.
                append_action_history_jsonl(&history_entry);

                return response;
            } else {
                error_json(StatusCode::NOT_FOUND, "Entity not found")
            }
        }
        Err(e) => {
            // Log to error log and LLM log
            let err_msg = format!("Failed to parse LLM response: {}", e);
            if let Ok(mut logger) = state.logger.lock() {
                logger.ensure_today(&PathBuf::from("logs"));
                logger.log_error(&err_msg);
                // Best-effort action-name extraction for the visual
                // header. The response is malformed (that's why we're
                // in this branch), but the `"action": "..."` field
                // is usually intact and parseable up to the point of
                // failure. Falls back to `entity:{id}` if extraction
                // fails, so the header is never empty.
                let action_from_response = extract_action_field(raw_response);
                let log_label = match action_from_response {
                    Some(action) => format!("{} of entity:{}", action, req.entity_id),
                    None => format!("entity:{}", req.entity_id),
                };
                logger.log_llm(
                    &log_label,
                    &format!("Process action for entity {}", req.entity_id),
                    raw_response,
                    0,
                    false,
                    &err_msg,
                    &format!("Parse error: {}", e)
                );
            }
            
            success_json(serde_json::json!({
                "success": false,
                "error": err_msg,
                "raw_response": raw_response
            })).into_response()
        }
    }
}

// ============================================================================
// Property endpoints
// ============================================================================

#[derive(Debug, Deserialize)]
struct SetPropertyRequest {
    value: PropertyValueJson,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PropertyValueJson {
    Int(i64),
    Float(f64),
    String(String),
}

async fn set_int_property(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath((id, key)): AxumPath<(Uuid, String)>,
    Json(req): Json<SetPropertyRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    match world.get_entity_mut(&id) {
        Some(entity) => {
            let val = match req.value {
                PropertyValueJson::Int(i) => i,
                PropertyValueJson::Float(f) => f as i64,
                PropertyValueJson::String(s) => s.parse().unwrap_or(0),
            };
            entity.set_int(&key, val);
            
            success_json(serde_json::json!({
                "success": true,
                "data": entity.get_int(&key)
            })).into_response()
        }
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

async fn set_float_property(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath((id, key)): AxumPath<(Uuid, String)>,
    Json(req): Json<SetPropertyRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    match world.get_entity_mut(&id) {
        Some(entity) => {
            let val = match req.value {
                PropertyValueJson::Int(i) => i as f64,
                PropertyValueJson::Float(f) => f,
                PropertyValueJson::String(s) => s.parse().unwrap_or(0.0),
            };
            entity.set_float(&key, val);
            
            success_json(serde_json::json!({
                "success": true,
                "data": entity.get_float(&key)
            })).into_response()
        }
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

async fn set_string_property(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath((id, key)): AxumPath<(Uuid, String)>,
    Json(req): Json<SetPropertyRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    match world.get_entity_mut(&id) {
        Some(entity) => {
            let val = match req.value {
                PropertyValueJson::Int(i) => i.to_string(),
                PropertyValueJson::Float(f) => f.to_string(),
                PropertyValueJson::String(s) => s,
            };
            entity.set_string(&key, &val);
            
            success_json(serde_json::json!({
                "success": true,
                "data": entity.get_string(&key)
            })).into_response()
        }
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

// DELETE int property
async fn delete_int_property(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath((id, key)): AxumPath<(Uuid, String)>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    match world.get_entity_mut(&id) {
        Some(entity) => {
            entity.properties_int.remove(&key);
            success_json(serde_json::json!({
                "success": true,
                "message": "Property deleted"
            })).into_response()
        }
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

// DELETE float property
async fn delete_float_property(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath((id, key)): AxumPath<(Uuid, String)>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    match world.get_entity_mut(&id) {
        Some(entity) => {
            entity.properties_float.remove(&key);
            success_json(serde_json::json!({
                "success": true,
                "message": "Property deleted"
            })).into_response()
        }
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

// DELETE string property
async fn delete_string_property(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    AxumPath((id, key)): AxumPath<(Uuid, String)>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let mut world = state.world.write().await;
    
    match world.get_entity_mut(&id) {
        Some(entity) => {
            entity.properties_string.remove(&key);
            success_json(serde_json::json!({
                "success": true,
                "message": "Property deleted"
            })).into_response()
        }
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
}

// ============================================================================
// Static File Handlers
// ============================================================================

async fn readme_handler() -> Response {
    match tokio::fs::read_to_string("README.md").await {
        Ok(content) => (
            StatusCode::OK,
            [("Content-Type", "text/markdown")],
            content
        ).into_response(),
        Err(_) => error_json(StatusCode::NOT_FOUND, "README.md not found"),
    }
}

// Backup endpoint
async fn create_backup_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    use chrono::Local;
    use std::process::Command;
    
    let now = Local::now();
    // Use timestamp to ensure unique filenames
    let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
    let filename = format!("open-world-backup-{}.tar.gz", timestamp);
    let path = format!("/home/openclaw/openclaw/workspace/{}", filename);
    
    // Create backup excluding target directory
    let output = Command::new("tar")
        .args(["czf", &path, "--exclude=target", "open-world"])
        .current_dir("/home/openclaw/openclaw/workspace")
        .output();
    
    match output {
        Ok(result) if result.status.success() => {
            // Get file size
            let metadata = std::fs::metadata(&path).ok();
            let size = metadata.map(|m| m.len()).unwrap_or(0);
            let size_mb = size as f64 / 1_048_576.0;
            
            success_json(serde_json::json!({
                "success": true,
                "filename": filename,
                "path": path,
                "size_bytes": size,
                "size_mb": format!("{:.2} MB", size_mb)
            })).into_response()
        }
        Ok(result) => {
            let error = String::from_utf8_lossy(&result.stderr);
            error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Backup failed: {}", error))
        }
        Err(e) => {
            error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to run backup: {}", e))
        }
    }
}

// Get backup info
async fn get_backups_handler() -> Response {
    use std::fs;
    
    let workspace = "/home/openclaw/openclaw/workspace";
    let mut backups = Vec::new();
    
    if let Ok(entries) = fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("open-world-backup-") && name.ends_with(".tar.gz") {
                if let Ok(metadata) = entry.metadata() {
                    let size = metadata.len();
                    let size_mb = size as f64 / 1_048_576.0;
                    let modified = metadata.modified().ok()
                        .map(|t| {
                            let datetime: chrono::DateTime<chrono::Utc> = t.into();
                            datetime.format("%Y-%m-%d %H:%M:%S UTC").to_string()
                        });
                    backups.push(serde_json::json!({
                        "name": name,
                        "size_bytes": size,
                        "size_mb": format!("{:.2} MB", size_mb),
                        "modified": modified
                    }));
                }
            }
        }
    }
    
    // Sort by name descending (newest first)
    backups.sort_by(|a, b| b["name"].as_str().unwrap_or("").cmp(a["name"].as_str().unwrap_or("")));
    
    success_json(serde_json::json!({
        "success": true,
        "backups": backups
    })).into_response()
}

// Delete backup file
async fn delete_backup_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(filename): axum::extract::Path<String>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let path = format!("/home/openclaw/openclaw/workspace/{}", filename);
    
    // Verify the file exists and is a backup file
    if !filename.starts_with("open-world-backup-") || !filename.ends_with(".tar.gz") {
        return error_json(StatusCode::BAD_REQUEST, "Invalid backup filename");
    }
    
    // Check if file exists
    match std::fs::metadata(&path) {
        Ok(_) => {
            // Delete the file
            match std::fs::remove_file(&path) {
                Ok(_) => {
                    success_json(serde_json::json!({
                        "success": true,
                        "message": "Backup deleted successfully"
                    })).into_response()
                }
                Err(e) => error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to delete backup: {}", e)),
            }
        }
        Err(_) => error_json(StatusCode::NOT_FOUND, "Backup file not found"),
    }
}

// Download backup file with authentication
async fn download_backup_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(filename): axum::extract::Path<String>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required to download backup");
    }
    
    // Validate filename is a backup file
    if !filename.starts_with("open-world-backup-") || !filename.ends_with(".tar.gz") {
        return error_json(StatusCode::BAD_REQUEST, "Invalid backup filename");
    }
    
    let path = format!("/home/openclaw/openclaw/workspace/{}", filename);
    let path = std::path::Path::new(&path);
    
    match tokio::fs::read(&path).await {
        Ok(content) => {
            let filename_header = format!("attachment; filename=\"{}\"", filename);
            let mut response = axum::http::Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "application/gzip")
                .header(axum::http::header::CONTENT_DISPOSITION, &filename_header)
                .header(axum::http::header::CONTENT_LENGTH, content.len())
                .body(axum::body::Body::from(content))
                .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create response"));
            response
        }
        Err(_) => error_json(StatusCode::NOT_FOUND, "Backup file not found"),
    }
}

// Download save file with authentication
async fn download_save_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required to download save file");
    }
    
    let path = &state.save_path;
    let path = std::path::Path::new(path);
    
    match tokio::fs::read(&path).await {
        Ok(content) => {
            let filename = "save.owbl";
            let filename_header = format!("attachment; filename=\"{}\"", filename);
            axum::http::Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
                .header(axum::http::header::CONTENT_DISPOSITION, &filename_header)
                .header(axum::http::header::CONTENT_LENGTH, content.len())
                .body(axum::body::Body::from(content))
                .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create response"))
        }
        Err(_) => error_json(StatusCode::NOT_FOUND, "Save file not found"),
    }
}

// Download project file with authentication
async fn download_project_file_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required to download file");
    }
    
    // Validate path - must be within open-world directory and not escape it
    let base_path = "/home/openclaw/openclaw/workspace/open-world";
    let requested_path = format!("{}/{}", base_path, path);
    let canonical = match std::path::Path::new(&requested_path).canonicalize() {
        Ok(p) => p,
        Err(_) => return error_json(StatusCode::NOT_FOUND, "File not found"),
    };
    let base_canonical = std::path::Path::new(base_path).canonicalize().unwrap_or_default();
    
    // Security check: ensure the path is within open-world directory
    if !canonical.starts_with(&base_canonical) {
        return error_json(StatusCode::FORBIDDEN, "Access denied");
    }
    
    // Skip target directory
    if path.contains("/target/") || path.starts_with("target/") {
        return error_json(StatusCode::FORBIDDEN, "Access denied");
    }
    
    match tokio::fs::read(&canonical).await {
        Ok(content) => {
            let filename = std::path::Path::new(&path).file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            let filename_header = format!("attachment; filename=\"{}\"", filename);
            
            // Determine content type based on extension
            let content_type = if filename.ends_with(".rs") {
                "text/plain"
            } else if filename.ends_with(".html") {
                "text/html"
            } else if filename.ends_with(".css") {
                "text/css"
            } else if filename.ends_with(".json") {
                "application/json"
            } else if filename.ends_with(".md") {
                "text/markdown"
            } else if filename.ends_with(".toml") {
                "application/toml"
            } else {
                "application/octet-stream"
            };
            
            axum::http::Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, content_type)
                .header(axum::http::header::CONTENT_DISPOSITION, &filename_header)
                .header(axum::http::header::CONTENT_LENGTH, content.len())
                .body(axum::body::Body::from(content))
                .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create response"))
        }
        Err(_) => error_json(StatusCode::NOT_FOUND, "File not found"),
    }
}

// Get file info for project files
async fn get_files_handler() -> Response {
    use std::fs;
    use std::path::Path;
    
    let open_world = "/home/openclaw/openclaw/workspace/open-world";
    let mut files = Vec::new();
    
    // Recursive function to collect all files
    fn collect_files(dir: &Path, base: &Path, files: &mut Vec<serde_json::Value>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                
                // Skip target directory
                if name == "target" || name == ".git" {
                    continue;
                }
                
                if path.is_dir() {
                    collect_files(&path, base, files);
                } else if path.is_file() {
                    if let Ok(metadata) = entry.metadata() {
                        let size = metadata.len();
                        let size_mb = size as f64 / 1_048_576.0;
                        let modified = metadata.modified().ok()
                            .map(|t| {
                                let datetime: chrono::DateTime<chrono::Utc> = t.into();
                                datetime.format("%Y-%m-%d %H:%M UTC").to_string()
                            });
                        
                        // Get relative path
                        let rel_path = path.strip_prefix(base)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        
                        // Skip large binary files
                        if size < 10_000_000 { // Less than 10MB
                            files.push(serde_json::json!({
                                "path": rel_path,
                                "name": name,
                                "size_bytes": size,
                                "size_mb": if size_mb >= 0.01 { format!("{:.2} MB", size_mb) } else { format!("{} B", size) },
                                "modified": modified
                            }));
                        }
                    }
                }
            }
        }
    }
    
    collect_files(Path::new(open_world), Path::new(open_world), &mut files);
    
    // Sort by path
    files.sort_by(|a, b| {
        let path_a = a["path"].as_str().unwrap_or("");
        let path_b = b["path"].as_str().unwrap_or("");
        path_a.cmp(path_b)
    });
    
    success_json(serde_json::json!({
        "success": true,
        "files": files
    })).into_response()
}

// ============================================================================
// World Management Endpoints
// ============================================================================

async fn save_world_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    let world = state.world.read().await.clone();
    drop(state.world.read().await);
    
    match BinaryPersistence::save_world(&world, &state.save_path) {
        Ok(()) => {
            let metadata = std::fs::metadata(&state.save_path).ok();
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = metadata.as_ref()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let datetime: chrono::DateTime<chrono::Utc> = t.into();
                    datetime.format("%Y-%m-%d %H:%M:%S UTC").to_string()
                })
                .unwrap_or_else(|| "Unknown".to_string());
            
            success_json(serde_json::json!({
                "success": true,
                "saved": true,
                "path": state.save_path,
                "size_bytes": size,
                "size_mb": format!("{:.2} MB", size as f64 / 1_048_576.0),
                "modified": modified
            })).into_response()
        }
        Err(e) => error_json(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

async fn world_status_handler(State(state): State<AppState>) -> Response {
    let save_path = &state.save_path;
    let has_save = std::path::Path::new(save_path).exists();
    
    let (save_size, save_modified) = if has_save {
        if let Ok(metadata) = std::fs::metadata(save_path) {
            let size = metadata.len();
            let modified = metadata.modified()
                .ok()
                .map(|t| {
                    let datetime: chrono::DateTime<chrono::Utc> = t.into();
                    datetime.format("%Y-%m-%d %H:%M:%S UTC").to_string()
                })
                .unwrap_or_else(|| "Unknown".to_string());
            (Some(size), Some(modified))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };
    
    success_json(serde_json::json!({
        "success": true,
        "has_save": has_save,
        "save_path": if has_save { Some(save_path.clone()) } else { None },
        "save_size": save_size,
        "save_size_mb": save_size.map(|s| format!("{:.2} MB", s as f64 / 1_048_576.0)),
        "save_modified": save_modified
    })).into_response()
}

async fn load_world_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    // Load world from save file
    match BinaryPersistence::load_world(&state.save_path) {
        Ok(world) => {
            let entity_count = world.entity_count();
            let mut w = state.world.write().await;
            *w = world;
            drop(w);
            
            success_json(serde_json::json!({
                "success": true,
                "message": "World loaded from save file",
                "entity_count": entity_count
            })).into_response()
        }
        Err(e) => error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to load world: {}", e)),
    }
}

async fn create_world_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateWorldRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    // Safety net: snapshot the existing save before we overwrite it.
    let pre_snapshot = snapshot_save("pre-create");
    let mut world = World::new(&req.name);

    // Generate sample entities if requested.
    // The 7 canonical sample entities (Oak Valley Village, Shadow Ridge
    // Camp, Elder Moonthorn, Whisperwood Forest, Silverstream Keep,
    // Ironforge Clan, Mira the Merchant) live in
    // `World::seed_sample_entities()` — the only place their definitions
    // should be edited. This endpoint is just a thin caller.
    if req.generate_sample {
        let _added = world.seed_sample_entities();
    }
    
    // Save the new world
    let entity_count = world.entity_count();
    match BinaryPersistence::save_world(&world, &state.save_path) {
        Ok(()) => {
            // Replace world in state
            let mut w = state.world.write().await;
            *w = world;
            drop(w); // Release write lock

            success_json(serde_json::json!({
                "success": true,
                "message": "World created successfully",
                "entity_count": entity_count,
                "pre_create_snapshot": pre_snapshot
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "(none - first create)".to_string()),
            })).into_response()
        }
        Err(e) => error_json(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

/// POST /api/world/backup  (auth required)
///
/// Manually snapshot the current save.owbl into world_data/backups/.
async fn backup_world_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie").and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    match snapshot_save("manual") {
        Ok(path) => success_json(serde_json::json!({
            "success": true,
            "message": "World snapshot created",
            "snapshot_path": path.display().to_string(),
            "size_bytes": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        })).into_response(),
        Err(e) => error_json(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

/// GET /api/world/backups  (auth required)
///
/// List all snapshots in world_data/backups/ with size + mtime.
async fn list_backups_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie").and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    let dir = std::path::Path::new("world_data/backups");
    let mut entries: Vec<serde_json::Value> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if let Ok(meta) = entry.metadata() {
                let modified = meta.modified().ok().map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.to_rfc3339()
                });
                entries.push(serde_json::json!({
                    "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    "size_bytes": meta.len(),
                    "modified": modified,
                }));
            }
        }
    }
    entries.sort_by(|a, b| {
        let am = a.get("modified").and_then(|v| v.as_str()).unwrap_or("");
        let bm = b.get("modified").and_then(|v| v.as_str()).unwrap_or("");
        bm.cmp(am)
    });
    success_json(serde_json::json!({
        "success": true,
        "count": entries.len(),
        "snapshots": entries,
    })).into_response()
}

// ============================================================================
// Log endpoints (LLM call log + error log, one file per day)
// ============================================================================
//
// `DailyLogger` writes to `logs/llm-log-YYYY-MM-DD.log` and
// `logs/error-log-YYYY-MM-DD.log`, with `ensure_today` rotating at midnight.
// These endpoints expose that directory over the API so the web client can
// show a "Logs" panel with a file picker + content viewer.
//
// Per Arcurus 2026-06-04 (#openworld): "make a button in open world selena
// where i can view the full world action log".
//
// 2026-06-04 BUGFIX: `chrono_now_date` / `chrono_now_timestamp` were
// dividing by 365 instead of 86400, producing year ~4.8M in filenames.
// Fixed at the source; old broken files are still on disk but new writes
// land in correctly-named per-day files. The endpoints below list both,
// so the user can see the pre-fix history too.

/// `GET /api/logs` — list all log files in `logs/`, sorted newest-first.
/// Returns `{ success, count, logs: [{ name, size_bytes, modified,
/// kind: "llm"|"error" }] }`.
async fn list_logs_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie").and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    let dir = std::path::Path::new("logs");
    let mut entries: Vec<serde_json::Value> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name() {
                Some(n) => n.to_string_lossy().to_string(),
                None => continue,
            };
            // Only show .log files (skip .log.bak, etc.)
            if !name.ends_with(".log") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let modified = meta.modified().ok().map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.to_rfc3339()
                });
                let kind = if name.starts_with("llm-log-") {
                    "llm"
                } else if name.starts_with("error-log-") {
                    "error"
                } else {
                    "other"
                };
                entries.push(serde_json::json!({
                    "name": name,
                    "size_bytes": meta.len(),
                    "modified": modified,
                    "kind": kind,
                }));
            }
        }
    }
    entries.sort_by(|a, b| {
        let am = a.get("modified").and_then(|v| v.as_str()).unwrap_or("");
        let bm = b.get("modified").and_then(|v| v.as_str()).unwrap_or("");
        bm.cmp(am)
    });
    success_json(serde_json::json!({
        "success": true,
        "count": entries.len(),
        "logs": entries,
    })).into_response()
}

/// `GET /api/logs/:filename?tail=N&max_bytes=M` — return the content of a
/// single log file, with optional `tail=N` (last N lines) and
/// `max_bytes=M` (last M bytes) trimming for big files. Filename is
/// sanitized: must start with `llm-log-` or `error-log-` and end with
/// `.log`; no path traversal (`..`, `/`).
async fn get_log_file_tail_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(filename): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie").and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    if filename.contains('/') || filename.contains('\\') || filename.contains("..")
        || !(filename.starts_with("llm-log-") || filename.starts_with("error-log-"))
        || !filename.ends_with(".log")
    {
        return error_json(StatusCode::BAD_REQUEST, "Invalid log filename");
    }

    let tail: Option<usize> = params.get("tail").and_then(|s| s.parse().ok());
    let max_bytes: Option<usize> = params.get("max_bytes").and_then(|s| s.parse().ok());

    let path = std::path::Path::new("logs").join(&filename);
    if !path.exists() {
        return error_json(StatusCode::NOT_FOUND, "Log file not found");
    }
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to read log: {}", e),
        ),
    };

    let (content, truncated) = trim_log_content(&raw, tail, max_bytes);
    let truncated_by = if truncated {
        if let Some(t) = tail { format!("tail={}", t) }
        else if let Some(m) = max_bytes { format!("max_bytes={}", m) }
        else { "none".to_string() }
    } else { "none".to_string() };

    success_json(serde_json::json!({
        "success": true,
        "name": filename,
        "size_bytes": raw.len(),
        "content": content,
        "truncated": truncated,
        "truncated_by": truncated_by,
    })).into_response()
}

/// Trim log content to the last `tail` lines OR the last `max_bytes`,
/// whichever is more restrictive. Both are optional. Returns
/// `(content, was_truncated)`.
fn trim_log_content(
    raw: &str,
    tail: Option<usize>,
    max_bytes: Option<usize>,
) -> (String, bool) {
    let mut out = raw.to_string();
    let mut truncated = false;

    if let Some(t) = tail {
        let total_lines = out.matches('\n').count() + 1;
        if total_lines > t {
            // Skip the first (total_lines - t) lines.
            let to_skip = total_lines - t;
            let mut skipped = 0;
            let mut start_idx = 0;
            for (i, ch) in out.char_indices() {
                if ch == '\n' {
                    skipped += 1;
                    if skipped == to_skip {
                        start_idx = i + 1;
                        break;
                    }
                }
            }
            out = out[start_idx..].to_string();
            truncated = true;
        }
    }
    if let Some(m) = max_bytes {
        if out.len() > m {
            // Keep last m bytes, but align to a UTF-8 char boundary.
            let start = out.len() - m;
            let aligned = floor_char_boundary(&out, start);
            out = out[aligned..].to_string();
            truncated = true;
        }
    }
    (out, truncated)
}

/// Find the largest char-boundary index <= `idx` in `s`.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ============================================================================
// Main
// ============================================================================

fn load_settings() -> Settings {
    let settings_path = PathBuf::from("settings.json");
    
    if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).expect("Failed to read settings");
        serde_json::from_str(&content).expect("Failed to parse settings")
    } else {
        // Default settings
        Settings {
            server: ServerSettings {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            world: AppWorldSettings {
                name: "New World".to_string(),
                description: "A new world waiting to be explored".to_string(),
            },
            llm: LlmSettings {
                provider: "minimax-portal".to_string(),
                model: "MiniMax-M2.7-highspeed".to_string(),
                api_key_name: "MINIMAX_API_KEY".to_string(),
                api_url: "https://api.minimax.io/anthropic".to_string(),
                max_output_tokens: 50000,
                llm_timeout_secs: 180,
                calls_per_hour_enabled: true,
                max_calls_per_hour: 0,
                default_max_history_summary_chars: default_max_history_summary_chars(),
            },
            security: SecuritySettings {
                password_var_name: "WEB_PASSWORD".to_string(),
                cookie_name: "openworld_auth".to_string(),
                cookie_duration_secs: 3600,
            },
            ui: UiSettings {
                title: "Open World".to_string(),
            },
        }
    }
}

fn ensure_env_file(env_path: &str, settings: &Settings) {
    let placeholder_key = "placeholder";
    let api_key_name = &settings.llm.api_key_name;
    let password_var_name = &settings.security.password_var_name;
    
    if !std::path::Path::new(env_path).exists() {
        // Create .env with placeholders
        let content = format!(
            "# Open World Environment Variables\n# Do not commit this file to version control!\n\n{}={}\n{}={}\n",
            api_key_name, placeholder_key,
            password_var_name, placeholder_key
        );
        std::fs::write(env_path, content).expect("Failed to create .env file");
        println!("🔐 Created {} with placeholders", env_path);
    }
}

fn read_env_var(env_path: &str, var_name: &str) -> Option<String> {
    if let Ok(content) = std::fs::read_to_string(env_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == var_name {
                    let value = value.trim();
                    if value.is_empty() || value == "placeholder" {
                        return None;
                    }
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn write_env_var(env_path: &str, var_name: &str, value: &str) -> Result<(), String> {
    let mut content = if let Ok(c) = std::fs::read_to_string(env_path) {
        c
    } else {
        String::new()
    };
    
    // Check if the variable already exists
    let mut found = false;
    let mut lines: Vec<String> = Vec::new();
    
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            lines.push(line.to_string());
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            if key.trim() == var_name {
                lines.push(format!("{}={}", var_name, value));
                found = true;
            } else {
                lines.push(line.to_string());
            }
        } else {
            lines.push(line.to_string());
        }
    }
    
    if !found {
        lines.push(format!("{}={}", var_name, value));
    }
    
    std::fs::write(env_path, lines.join("\n")).map_err(|e| e.to_string())
}

// ============================================================================
// Authentication helper
// ============================================================================

fn verify_auth_cookie(cookies_header: Option<&str>, cookie_name: &str) -> bool {
    if let Some(cookies) = cookies_header {
        for cookie in cookies.split(';') {
            let cookie = cookie.trim();
            if let Some((name, value)) = cookie.split_once('=') {
                if name == cookie_name && value == "1" {
                    return true;
                }
            }
        }
    }
    false
}

fn require_auth(state: &AppState, request: &axum::http::Request<axum::body::Body>) -> Option<Response> {
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = request.headers()
        .get("cookie")
        .and_then(|v| v.to_str().ok());
    
    if !verify_auth_cookie(cookies, cookie_name) {
        return Some(error_json(StatusCode::UNAUTHORIZED, "Authentication required"));
    }
    None
}

// ============================================================================
// .env / Security endpoints
// ============================================================================

#[derive(Debug, Deserialize)]
struct EnvConfigureRequest {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VerifyPasswordRequest {
    password: String,
}

#[derive(Debug, Deserialize)]
struct UpdateEnvVariablesRequest {
    variables: std::collections::HashMap<String, String>,
}

/// Check if .env is configured (has real values, not placeholders)
async fn env_status_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key_name = &state.settings.llm.api_key_name;
    let password_var_name = &state.settings.security.password_var_name;
    let cookie_name = &state.settings.security.cookie_name;
    
    let api_key = read_env_var(&state.env_path, api_key_name);
    let password = read_env_var(&state.env_path, password_var_name);
    
    // Check if user is currently authenticated (has valid cookie)
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    let is_authenticated = verify_auth_cookie(cookies, cookie_name);
    
    success_json(serde_json::json!({
        "success": true,
        "needs_config": api_key.is_none() || password.is_none(),
        "has_api_key": api_key.is_some(),
        "has_password": password.is_some(),
        "is_authenticated": is_authenticated,
        "api_key_name": api_key_name,
        "password_var_name": password_var_name,
        "llm_rate_limit": llm_rate_limit_status(&state)
    })).into_response()
}

/// Configure .env with API key and password
async fn configure_env_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EnvConfigureRequest>,
) -> Response {
    let api_key_name = &state.settings.llm.api_key_name;
    let password_var_name = &state.settings.security.password_var_name;
    let cookie_name = &state.settings.security.cookie_name;
    
    // Check if password is already configured (not placeholder)
    let password_is_set = read_env_var(&state.env_path, password_var_name).is_some();
    
    // If password is set, require authentication
    if password_is_set {
        let cookies = headers.get("cookie")
            .and_then(|v| v.to_str().ok());
        if !verify_auth_cookie(cookies, cookie_name) {
            return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
        }
    }
    
    // Process API key if provided
    if let Some(api_key) = &req.api_key {
        if api_key.is_empty() || api_key == "placeholder" {
            return error_json(StatusCode::BAD_REQUEST, "Invalid API key");
        }
        if let Err(e) = write_env_var(&state.env_path, api_key_name, api_key) {
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to write API key: {}", e));
        }
    }
    
    // Process password if provided
    if let Some(password) = &req.password {
        if password.is_empty() || password == "placeholder" {
            return error_json(StatusCode::BAD_REQUEST, "Invalid password");
        }
        if let Err(e) = write_env_var(&state.env_path, password_var_name, password) {
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to write password: {}", e));
        }
    }
    
    println!("🔐 .env configured");
    
    success_json(serde_json::json!({
        "success": true,
        "message": "Configuration saved"
    })).into_response()
}

/// Verify password for file access
async fn verify_password_handler(
    State(state): State<AppState>,
    Json(req): Json<VerifyPasswordRequest>,
) -> Response {
    let password_var_name = &state.settings.security.password_var_name;
    let cookie_name = &state.settings.security.cookie_name;
    let cookie_duration = state.settings.security.cookie_duration_secs;
    
    let stored_password = read_env_var(&state.env_path, password_var_name);
    
    match stored_password {
        Some(stored) if stored == req.password => {
            // Password correct - return success with cookie info
            success_json(serde_json::json!({
                "success": true,
                "verified": true,
                "cookie_name": cookie_name,
                "cookie_max_age": cookie_duration
            })).into_response()
        }
        Some(_) => {
            error_json(StatusCode::UNAUTHORIZED, "Invalid password")
        }
        None => {
            error_json(StatusCode::BAD_REQUEST, "Password not configured")
        }
    }
}

/// Get .env variables (for authenticated users)
async fn get_env_variables_handler(State(state): State<AppState>) -> Response {
    // Read all variables from .env
    let mut variables = std::collections::HashMap::new();
    
    if let Ok(content) = std::fs::read_to_string(&state.env_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                // Mask sensitive values
                let display_value = if value == "placeholder" || value.is_empty() {
                    "".to_string()
                } else if key.to_lowercase().contains("password") || key.to_lowercase().contains("secret") || key.to_lowercase().contains("key") {
                    "********".to_string()
                } else {
                    value
                };
                variables.insert(key, display_value);
            }
        }
    }
    
    success_json(serde_json::json!({
        "success": true,
        "variables": variables
    })).into_response()
}

/// Update .env variables
async fn update_env_variables_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<UpdateEnvVariablesRequest>,
) -> Response {
    // Require authentication
    let cookie_name = &state.settings.security.cookie_name;
    let cookies = headers.get("cookie")
        .and_then(|v| v.to_str().ok());
    if !verify_auth_cookie(cookies, cookie_name) {
        return error_json(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    
    for (key, value) in &req.variables {
        if let Err(e) = write_env_var(&state.env_path, key, value) {
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to update {}: {}", key, e));
        }
    }
    
    success_json(serde_json::json!({
        "success": true,
        "message": "Variables updated"
    })).into_response()
}

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();
    
    // Load settings
    let settings = load_settings();
    println!("⚙️  Settings loaded from settings.json");
    
    // Determine save path and env path
    let save_path = "world_data/save.owbl".to_string();
    let env_path = ".env".to_string();
    let log_dir = PathBuf::from("logs");
    
    // Create logs directory
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        println!("⚠️  Failed to create logs directory: {}", e);
    } else {
        println!("📁 Logs directory: {:?}", log_dir);
    }
    
    // Ensure .env file exists with placeholders
    ensure_env_file(&env_path, &settings);
    
    // Initialize logger
    let logger = Arc::new(std::sync::Mutex::new(DailyLogger::new(log_dir.clone())));
    
    // Load world from save file or create new
    let mut world = if BinaryPersistence::save_exists(&save_path) {
        println!("📂 Loading world from save file...");
        match BinaryPersistence::load_world(&save_path) {
            Ok(mut w) => {
                // Ensure clock entity exists (for old save files)
                w.create_clock_entity();
                // Sanitize int properties on system entities. This cleans
                // up garbage values that were written by LLM effects before
                // the c7f3bc27 upstream protection landed (todo 2df49bd8).
                let repairs = w.sanitize_system_entities();
                if !repairs.is_empty() {
                    println!("🧹 Sanitized {} system-entity int property repair(s):", repairs.len());
                    for (id, key, old, new) in &repairs {
                        println!("   • {} :: {} :: {} -> {}", id, key, old, new);
                    }
                }
                // Sanitize int and float properties on non-system entities
                // (e.g. an old dragon entity whose `power` was written as
                // `-4.05e18` before c7f3bc27). Wired in alongside the
                // system-entity sanitizer so the on-disk corruption gets
                // cleaned up on every load. See todo 2df49bd8.
                let (int_repairs, float_repairs) = w.sanitize_non_system_entity_properties();
                if !int_repairs.is_empty() {
                    println!("🧹 Sanitized {} non-system int property repair(s):", int_repairs.len());
                    for (id, key, old, new) in &int_repairs {
                        println!("   • {} :: {} :: {} -> {}", id, key, old, new);
                    }
                }
                if !float_repairs.is_empty() {
                    println!("🧹 Sanitized {} non-system float property repair(s):", float_repairs.len());
                    for (id, key, old, new) in &float_repairs {
                        println!("   • {} :: {} :: {} -> {}", id, key, old, new);
                    }
                }
                // Sync time from clock entity to world_time
                w.sync_time_from_clock();
                // Advance time based on real elapsed time since last save
                let old_time = w.world_time.detailed_time();
                w.world_time.update_from_real_time();
                let new_time = w.world_time.detailed_time();
                // Sync advanced time back to clock entity
                w.sync_time_to_clock();
                println!("✅ World loaded: {} ({} entities)", w.name, w.entity_count());
                println!("⏰ Time: {} → {}", old_time, new_time);
                w
            }
            Err(e) => {
                println!("⚠️  Failed to load world: {}. Creating new world.", e);
                World::new(&settings.world.name)
            }
        }
    } else {
        println!("🆕 No save file found. Creating new world: {}", settings.world.name);
        World::new(&settings.world.name)
    };
    
    let state = AppState {
        world: Arc::new(RwLock::new(world)),
        settings: settings.clone(),
        save_path,
        env_path,
        logger,
        llm_rate_limiter: Arc::new(std::sync::Mutex::new(LlmRateLimiter::new(
            settings.llm.calls_per_hour_enabled,
            settings.llm.max_calls_per_hour,
        ))),
    };

    println!(
        "🌍   LLM rate limit: enabled={}, max_calls_per_hour={}",
        settings.llm.calls_per_hour_enabled, settings.llm.max_calls_per_hour
    );
    
    let addr = format!("{}:{}", settings.server.host, settings.server.port);
    
    // Build router with API and static files
    let app = Router::new()
        // API routes first (take precedence)
        .route("/api/", get(get_world))
        .route("/api/entities", get(list_entities))
        .route("/api/entities", post(create_entity))
        .route("/api/entities/:id", get(get_entity))
        .route("/api/entities/:id", put(update_entity))
        .route("/api/entities/:id", axum::routing::delete(delete_entity))
        .route("/api/entities/:id/history", get(get_entity_history))
        .route("/api/entities/:id/history-summary/replace", post(history_summary_replace_handler))
        .route("/api/entities/:id/action", post(entity_action))
        .route("/api/entities/:id/action/context", get(action_context_handler))
        .route("/api/entities/:id/action/llm", post(action_llm_handler))
        .route("/api/entities/:id/action/process", post(process_action_handler))
        .route("/api/entities/:id/properties/int/:key", put(set_int_property))
        .route("/api/entities/:id/properties/int/:key", axum::routing::delete(delete_int_property))
        .route("/api/entities/:id/properties/float/:key", put(set_float_property))
        .route("/api/entities/:id/properties/float/:key", axum::routing::delete(delete_float_property))
        .route("/api/entities/:id/properties/string/:key", put(set_string_property))
        .route("/api/entities/:id/properties/string/:key", axum::routing::delete(delete_string_property))
        // Serve documentation files
        .nest_service("/docs", ServeDir::new("docs"))
        .route("/README.md", get(readme_handler))
        // Protected file downloads
        .route("/api/backups/:filename", get(download_backup_handler))
        .route("/api/backups/:filename", axum::routing::delete(delete_backup_handler))
        .route("/api/world/save/download", get(download_save_handler))
        .route("/api/files/*path", get(download_project_file_handler))
        // Backup endpoints
        .route("/api/backup/create", post(create_backup_handler))
        .route("/api/backups", get(get_backups_handler))
        .route("/api/files", get(get_files_handler))
        // Log endpoints (per-day LLM call log + error log viewer)
        .route("/api/logs", get(list_logs_handler))
        .route("/api/logs/:filename", get(get_log_file_tail_handler))
        // World management endpoints
        .route("/api/world/save", post(save_world_handler))
        .route("/api/world/backup", post(backup_world_handler))
        .route("/api/world/backups", get(list_backups_handler))
        .route("/api/world/status", get(world_status_handler))
        .route("/api/world/create", post(create_world_handler))
        .route("/api/world/load", post(load_world_handler))
        .route("/api/world", get(get_world))
        .route("/api/world", put(update_world_handler))
        .route("/api/world/stats", get(get_world_stats))
        // World events endpoints
        .route("/api/world/events", get(get_world_events))
        .route("/api/world/events", post(add_world_event))
        .route("/api/world/events/:id", put(update_world_event))
        .route("/api/world/events/:id", delete(delete_world_event))
        // LLM rate-limit endpoints
        .route("/api/llm/status", get(llm_status_handler))
        .route("/api/llm/config", post(llm_config_update_handler))
        // .env / security endpoints
        .route("/api/env/status", get(env_status_handler))
        .route("/api/env/configure", post(configure_env_handler))
        .route("/api/env/verify-password", post(verify_password_handler))
        .route("/api/env/variables", get(get_env_variables_handler))
        .route("/api/env/variables", put(update_env_variables_handler))
        // Serve web client (fallback for SPA)
        .fallback_service(ServeDir::new("web-client"))
        .with_state(state);
    
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap();
    
    println!();
    println!("🌍 ═══════════════════════════════════════════════════");
    println!("🌍   Open World Server");
    println!("🌍 ═══════════════════════════════════════════════════");
    println!("🌍");
    println!("🌍   Web UI:     http://localhost:{}/", settings.server.port);
    println!("🌍   World:      {}", settings.world.name);
    println!("🌍");
    println!("🌍   API Endpoints:");
    println!("🌍     GET    /api/                          - World info");
    println!("🌍     GET    /api/entities                 - List entities");
    println!("🌍     POST   /api/entities                 - Create entity");
    println!("🌍     GET    /api/entities/:id             - Get entity");
    println!("🌍     PUT    /api/entities/:id             - Update entity");
    println!("🌍     DELETE /api/entities/:id             - Delete entity");
    println!("🌍     PUT    /api/entities/:id/properties/int/:key   - Set int");
    println!("🌍     DELETE /api/entities/:id/properties/int/:key   - Del int");
    println!("🌍     PUT    /api/entities/:id/properties/float/:key - Set float");
    println!("🌍     DELETE /api/entities/:id/properties/float/:key - Del float");
    println!("🌍     PUT    /api/entities/:id/properties/string/:key - Set string");
    println!("🌍     DELETE /api/entities/:id/properties/string/:key - Del string");
    println!("🌍");
    println!("🌍   Query params: ?q=search, ?entity_type=x, ?tags=a,b, ?near_x=x&near_y=y&radius=r");
    println!("🌍 ═══════════════════════════════════════════════════");
    
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod effect_guard_tests {
    use super::*;

    #[test]
    fn magnitude_check_accepts_normal_values() {
        // Power, morale, wealth deltas — typical LLM outputs.
        assert!(magnitude_check(&serde_json::json!(5)).is_none());
        assert!(magnitude_check(&serde_json::json!(-12.5)).is_none());
        assert!(magnitude_check(&serde_json::json!("+3")).is_none());
        assert!(magnitude_check(&serde_json::json!("42")).is_none());
        assert!(magnitude_check(&serde_json::json!(0)).is_none());
    }

    #[test]
    fn magnitude_check_rejects_huge_deltas() {
        // The exact pattern that produced the world_clock corruption
        // (todo 2df49bd8) was 1e18 floats being added to ints.
        let r = magnitude_check(&serde_json::json!(1e18));
        assert!(r.is_some(), "1e18 must be rejected");
        assert!(r.unwrap().contains("MAX_DELTA_ABS"));
    }

    #[test]
    fn magnitude_check_rejects_string_deltas() {
        assert!(magnitude_check(&serde_json::json!("9999999999")).is_some());
    }

    #[test]
    fn magnitude_check_rejects_nan_and_inf() {
        // serde_json cannot directly express NaN/Inf, but a string
        // parses to f64::INFINITY; bypass via a huge literal.
        let r = magnitude_check(&serde_json::json!(1e308));
        assert!(r.is_some(), "huge finite values are still rejected by the delta cap");
    }

    #[test]
    fn int_oversize_classifies() {
        assert!(!int_oversize(0));
        assert!(!int_oversize(999_999_999));
        assert!(int_oversize(1_000_000_001));
        assert!(int_oversize(-1_000_000_001));
    }

    #[test]
    fn float_oversize_classifies() {
        assert!(!float_oversize(0.0));
        assert!(!float_oversize(1.0e9 - 1.0));
        assert!(float_oversize(1.0e9 + 1.0));
        assert!(float_oversize(f64::INFINITY));
    }
}

#[cfg(test)]
mod history_summary_replace_tests {
    use super::*;

    fn r(old: &str, new: &str) -> HistorySummaryReplace {
        HistorySummaryReplace {
            old_part: old.to_string(),
            new_part: new.to_string(),
        }
    }

    // -- matrix row 1: old_part="" + existing summary + non-empty new_part → append --
    #[test]
    fn append_to_existing_summary() {
        let result = apply_history_summary_replaces(
            Some("hello world"),
            &[r("", " (appended)")],
            10_000,
        );
        assert_eq!(result.new_summary.as_deref(), Some("hello world (appended)"));
        assert!(!result.truncated);
        assert!(result.warnings.is_empty(), "no warning expected, got: {:?}", result.warnings);
    }

    // -- matrix row 2: old_part="" + None summary + non-empty new_part → new summary --
    #[test]
    fn empty_old_part_with_none_summary_creates_new() {
        let result = apply_history_summary_replaces(
            None,
            &[r("", "fresh start")],
            10_000,
        );
        assert_eq!(result.new_summary.as_deref(), Some("fresh start"));
        assert!(!result.truncated);
        assert!(result.warnings.is_empty());
    }

    // -- matrix row 3: old_part="" + non-empty new_part + Some("") summary → new summary --
    // (Some("") is treated identically to None per the helper's contract)
    #[test]
    fn empty_old_part_with_empty_string_summary_creates_new() {
        let result = apply_history_summary_replaces(
            Some(""),
            &[r("", "replacement")],
            10_000,
        );
        assert_eq!(result.new_summary.as_deref(), Some("replacement"));
    }

    // -- matrix row 4: both old_part and new_part empty → no-op (Some("") result is
    //    treated as no content, returns None per the empty-as-None rule) --
    #[test]
    fn both_empty_is_noop() {
        let result = apply_history_summary_replaces(
            Some("existing content"),
            &[r("", "")],
            10_000,
        );
        assert_eq!(result.new_summary.as_deref(), Some("existing content"));
        assert!(!result.truncated);
    }

    // -- matrix row 5: non-empty old_part + found → first-occurrence replace --
    #[test]
    fn find_and_replace_works() {
        let result = apply_history_summary_replaces(
            Some("foo bar foo"),
            &[r("foo", "BAZ")],
            10_000,
        );
        // First occurrence replaced; second "foo" remains.
        assert_eq!(result.new_summary.as_deref(), Some("BAZ bar foo"));
        assert!(!result.truncated);
        assert!(result.warnings.is_empty());
    }

    // -- matrix row 6: non-empty old_part + not found → warning, skip --
    #[test]
    fn not_found_logs_warning() {
        let result = apply_history_summary_replaces(
            Some("foo bar"),
            &[r("DOES_NOT_EXIST", "X")],
            10_000,
        );
        // No change to summary.
        assert_eq!(result.new_summary.as_deref(), Some("foo bar"));
        // Warning logged.
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("old_part not found"));
    }

    // -- matrix row 7: non-empty old_part + None summary → warning, skip --
    #[test]
    fn non_empty_old_part_with_none_summary_warns_and_skips() {
        let result = apply_history_summary_replaces(
            None,
            &[r("X", "Y")],
            10_000,
        );
        // No change (no summary to start with, can't search).
        assert_eq!(result.new_summary, None);
        // Warning logged.
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("current summary is empty"));
    }

    // -- matrix row 8: multi-replace chain (array) -- both apply in order --
    #[test]
    fn chain_of_replaces_applies_in_order() {
        let result = apply_history_summary_replaces(
            Some("foo bar"),
            &[r("foo", "FOO"), r("bar", "BAR")],
            10_000,
        );
        assert_eq!(result.new_summary.as_deref(), Some("FOO BAR"));
        assert!(result.warnings.is_empty());
    }

    // -- matrix row 9: chain with one not-found in the middle -- other commands still apply --
    #[test]
    fn chain_with_not_found_in_middle_continues_with_rest() {
        let result = apply_history_summary_replaces(
            Some("foo bar"),
            &[
                r("foo", "FOO"),
                r("DOES_NOT_EXIST", "X"),  // skipped with warning
                r("bar", "BAR"),
            ],
            10_000,
        );
        // First and third still apply; second is skipped.
        assert_eq!(result.new_summary.as_deref(), Some("FOO BAR"));
        // One warning for the skipped command.
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("history_summary_replace[1]"));
    }

    // -- matrix row 10: empty old_part after a successful replace continues from new state --
    #[test]
    fn chain_with_empty_old_part_after_replace() {
        let result = apply_history_summary_replaces(
            Some("foo"),
            &[
                r("foo", "FOO"),           // state: "FOO"
                r("", " (note)"),          // append: "FOO (note)"
            ],
            10_000,
        );
        assert_eq!(result.new_summary.as_deref(), Some("FOO (note)"));
        assert!(result.warnings.is_empty());
    }

    // -- matrix row 11: truncation when result exceeds cap --
    #[test]
    fn truncation_when_over_cap() {
        // Start with one char, replace with 200. Result is 200 chars.
        // Cap is 100, so truncate to 99 + "…".
        let result = apply_history_summary_replaces(
            Some("x"),
            &[r("x", &"y".repeat(200))],
            100,
        );
        assert!(result.truncated);
        assert!(result.warnings.iter().any(|w| w.contains("exceeded")));
        // Truncated to cap-1 chars + "…".
        let s = result.new_summary.unwrap();
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 100);
    }

    // -- matrix row 12: empty result after a successful "delete everything" is treated
    //    as None (per the empty-as-None rule) --
    #[test]
    fn delete_to_empty_results_in_none() {
        let result = apply_history_summary_replaces(
            Some("ONLY_THIS"),
            &[r("ONLY_THIS", "")],
            10_000,
        );
        assert_eq!(result.new_summary, None);
    }

    // -- matrix row 13: !ALL! full-replace discards the current summary
    //    and sets it to new_part (regression guard for 920fae9 / Arcurus
    //    2026-06-05: !ALL! convention was added so callers can request a
    //    full restructure without the API/LLM paths diverging). --
    #[test]
    fn all_full_replace_discards_current() {
        let result = apply_history_summary_replaces(
            Some("old narrative that is being thrown away"),
            &[r("!ALL!", "completely fresh summary, new arc")],
            10_000,
        );
        assert_eq!(
            result.new_summary.as_deref(),
            Some("completely fresh summary, new arc")
        );
        // !ALL! is by-design, no warning expected.
        assert!(result.warnings.is_empty(), "got warnings: {:?}", result.warnings);
        assert!(!result.truncated);
    }

    // -- matrix row 14: !ALL! followed by an append in the same chain
    //    still applies the append (the [.!ALL! + append] combo is the
    //    canonical "replace the whole summary, then add a tag line"
    //    pattern that the LLM uses after a chapter break). --
    #[test]
    fn all_full_replace_then_append() {
        let result = apply_history_summary_replaces(
            Some("stale content"),
            &[
                r("!ALL!", "fresh chapter 1 content"),
                r("", " (next: investigate ruins)"),
            ],
            10_000,
        );
        assert_eq!(
            result.new_summary.as_deref(),
            Some("fresh chapter 1 content (next: investigate ruins)")
        );
        assert!(result.warnings.is_empty());
    }

    // -- matrix row 15: !ALL! with empty new_part results in None
    //    (the empty-as-None rule still applies to !ALL! — a full replace
    //    to "" is equivalent to "no summary"). --
    #[test]
    fn all_full_replace_with_empty_new_part_results_in_none() {
        let result = apply_history_summary_replaces(
            Some("existing summary content"),
            &[r("!ALL!", "")],
            10_000,
        );
        assert_eq!(result.new_summary, None);
        assert!(result.warnings.is_empty());
    }

    // -- matrix row 16: !ALL! is also a no-op when new_part is empty AND
    //    there is no current summary (chain order still respected). --
    #[test]
    fn all_full_replace_with_none_summary_and_empty_new_part() {
        let result = apply_history_summary_replaces(
            None,
            &[r("!ALL!", "")],
            10_000,
        );
        assert_eq!(result.new_summary, None);
    }

    // -- edge: into_vec on Single --
    #[test]
    fn one_or_many_into_vec_single() {
        let one = HistorySummaryReplaceOneOrMany::Single(r("a", "b"));
        assert_eq!(one.into_vec().len(), 1);
    }

    // -- edge: into_vec on Many --
    #[test]
    fn one_or_many_into_vec_many() {
        let many = HistorySummaryReplaceOneOrMany::Many(vec![r("a", "b"), r("c", "d")]);
        assert_eq!(many.into_vec().len(), 2);
    }

    // -- edge: empty `{}` from the LLM is tolerated and deserializes
    //    to a no-op Many(vec![]) (regression: 2026-06-05 00:52:19 the
    //    world clock emitted `"history_summary_replace":{}` and the
    //    default untagged decoder dropped the entire LLM response).
    #[test]
    fn one_or_many_deserializes_empty_object_as_noop() {
        let parsed: HistorySummaryReplaceOneOrMany =
            serde_json::from_str("{}").expect("empty object should parse");
        assert_eq!(parsed.into_vec().len(), 0);
    }

    // -- edge: empty `[]` from the LLM is tolerated and deserializes
    //    to a no-op Many(vec![]) (symmetry with the `{}` case).
    #[test]
    fn one_or_many_deserializes_empty_array_as_noop() {
        let parsed: HistorySummaryReplaceOneOrMany =
            serde_json::from_str("[]").expect("empty array should parse");
        assert_eq!(parsed.into_vec().len(), 0);
    }

    // -- edge: a single object with both fields still deserializes
    //    to Single (regression guard for the rewrite).
    #[test]
    fn one_or_many_deserializes_single_object() {
        let parsed: HistorySummaryReplaceOneOrMany =
            serde_json::from_str(r#"{"old_part":"a","new_part":"b"}"#)
                .expect("single object should parse");
        let v = parsed.into_vec();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].old_part, "a");
        assert_eq!(v[0].new_part, "b");
    }

    // -- edge: an array of two objects still deserializes to Many
    //    (regression guard for the rewrite).
    #[test]
    fn one_or_many_deserializes_array_of_two() {
        let parsed: HistorySummaryReplaceOneOrMany = serde_json::from_str(
            r#"[{"old_part":"a","new_part":"b"},{"old_part":"c","new_part":"d"}]"#,
        )
        .expect("array should parse");
        let v = parsed.into_vec();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].old_part, "c");
    }

    // -- edge: a non-empty object missing `new_part` still errors
    //    (regression guard: we only relaxed the EMPTY case, real
    //    malformed payloads should still be rejected so the operator
    //    notices).
    #[test]
    fn one_or_many_rejects_partial_object() {
        let result: Result<HistorySummaryReplaceOneOrMany, _> =
            serde_json::from_str(r#"{"old_part":"a"}"#);
        assert!(result.is_err(), "missing new_part must still fail");
    }

    // -- find_replace_range: strict matches --
    #[test]
    fn find_replace_range_strict_match() {
        let h = "the hero met the dragon at dawn";
        let r = find_replace_range(h, "met the dragon");
        assert_eq!(r, Some((9, 23)));
    }

    #[test]
    fn find_replace_range_strict_no_match() {
        let h = "the hero met the dragon at dawn";
        assert_eq!(find_replace_range(h, "killed the dragon"), None);
    }

    // -- find_replace_range: whitespace fuzziness --
    #[test]
    fn find_replace_range_collapses_extra_spaces() {
        let h = "the hero met  the  dragon at dawn";   // double spaces in haystack
        let r = find_replace_range(h, "met the dragon");
        assert!(r.is_some(), "should match despite double spaces");
        let (s, e) = r.unwrap();
        assert_eq!(&h[s..e], "met  the  dragon"); // range covers the original
    }

    #[test]
    fn find_replace_range_collapses_leading_trailing_spaces() {
        let h = "met the dragon at dawn";
        let r = find_replace_range(h, "  met the dragon  ");
        assert!(r.is_some());
    }

    // -- find_replace_range: trailing-punctuation fuzziness --
    #[test]
    fn find_replace_range_strips_trailing_period() {
        let h = "met the dragon at dawn";
        // needle has a trailing period the haystack doesn't
        let r = find_replace_range(h, "met the dragon.");
        assert!(r.is_some(), "should match despite trailing period in needle");
    }

    #[test]
    fn find_replace_range_strips_trailing_comma() {
        let h = "scout, ready for orders";
        let r = find_replace_range(h, "scout, ready,");
        assert!(r.is_some());
    }

    // -- find_replace_range: still rejects when truly different --
    #[test]
    fn find_replace_range_rejects_meaningful_diff() {
        let h = "the hero met the dragon at dawn";
        // different word — should not match
        assert_eq!(find_replace_range(h, "met a wyvern"), None);
    }

    // -- find_replace_range: empty needle returns Some(0,0) per Rust's
    //    str::find semantics; the caller in apply_history_summary_replaces
    //    already special-cases empty old_part in a separate branch
    //    (the append path), so find_replace_range never sees "" in
    //    production. Document the behavior so we don't regress. --
    #[test]
    fn find_replace_range_empty_needle_returns_zero_zero() {
        let h = "the hero met the dragon";
        assert_eq!(find_replace_range(h, ""), Some((0, 0)));
    }

    // -- is_placeholder_summary: detects LLM "placebo summaries" --
    // Regression guard for the Shadow Crown bug (2026-06-05): the entity
    // had history_summary="…" (1 char) while history.len()=124 because
    // an earlier LLM emit stored the ellipsis as a real summary.
    #[test]
    fn is_placeholder_summary_detects_ellipsis_alone() {
        assert!(is_placeholder_summary("…"));
        assert!(is_placeholder_summary("… "));
        assert!(is_placeholder_summary(" …"));
    }

    #[test]
    fn is_placeholder_summary_detects_dash_alone() {
        assert!(is_placeholder_summary("—"));
        assert!(is_placeholder_summary("-"));
        assert!(is_placeholder_summary(" -- "));
    }

    #[test]
    fn is_placeholder_summary_detects_punctuation_only() {
        assert!(is_placeholder_summary("..."));
        assert!(is_placeholder_summary(".,;:"));
        assert!(is_placeholder_summary("   "));
    }

    #[test]
    fn is_placeholder_summary_accepts_real_summaries() {
        assert!(!is_placeholder_summary("The hero drew their sword."));
        assert!(!is_placeholder_summary("a")); // single alphanum is enough
        assert!(!is_placeholder_summary("…but tomorrow the dragon wakes"));
        // numeric-only is also meaningful (e.g., "12 victories")
        assert!(!is_placeholder_summary("42"));
    }
}

#[cfg(test)]
mod fix_known_malformed_patterns_tests {
    use super::fix_known_malformed_patterns;

    #[test]
    fn repairs_empty_key_before_new_part() {
        // The exact pattern from the 2026-06-05 15:42 World Clock
        // error log: the LLM emitted
        //   {"old_part":"...","":"new_part":"..."}
        // instead of
        //   {"old_part":"...","new_part":"..."}
        let broken = r#"{"old_part":"Currently broadcasting...Post-Harmonization Era.","":"new_part":"Now actively collecting..."}"#;
        let repaired = fix_known_malformed_patterns(broken);
        let parsed: serde_json::Value = serde_json::from_str(&repaired)
            .expect("repaired string must parse");
        assert_eq!(
            parsed.get("old_part").and_then(|v| v.as_str()),
            Some("Currently broadcasting...Post-Harmonization Era.")
        );
        assert_eq!(
            parsed.get("new_part").and_then(|v| v.as_str()),
            Some("Now actively collecting...")
        );
    }

    #[test]
    fn repairs_empty_key_before_old_part() {
        // Mirror case: the bogus value is "old_part" instead of
        // "new_part". Same fix shape, just a different key name.
        // The empty key is here the SECOND key (preceded by
        // `"new_part"`), so the regex's required leading comma is
        // present — matching how the bug would actually appear in
        // the wild.
        let broken = r#"{"new_part":"newer text","":"old_part":"some text"}"#;
        let repaired = fix_known_malformed_patterns(broken);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.get("old_part").and_then(|v| v.as_str()), Some("some text"));
        assert_eq!(parsed.get("new_part").and_then(|v| v.as_str()), Some("newer text"));
    }

    #[test]
    fn leaves_valid_json_unchanged() {
        // Sanity: a well-formed history_summary_replace object
        // should pass through untouched (the function returns the
        // input unchanged so the equality check `repaired != json_str`
        // in `parse_llm_action_response` works correctly).
        let ok = r#"{"old_part":"a","new_part":"b"}"#;
        let repaired = fix_known_malformed_patterns(ok);
        assert_eq!(repaired, ok);
    }

    #[test]
    fn does_not_touch_unrelated_empty_keys() {
        // The regex only matches `,"":"old_part":"` or `,"":"new_part":"`.
        // An empty key with some other value must NOT be modified
        // (the caller would then try the strict parse and surface
        // the error normally, which is the right behavior for a
        // bug we don't recognize).
        let unrelated = r#"{"":"unexpected_value","x":"y"}"#;
        let repaired = fix_known_malformed_patterns(unrelated);
        assert_eq!(repaired, unrelated);
    }
}

#[cfg(test)]
mod parse_llm_action_response_repair_warning_tests {
    //! Tests for the (LlmActionResponse, Option<String>) repair-warning
    //! return shape added so the tolerant regex-fixup is observable
    //! to operators. See `parse_llm_action_response` and the
    //! World-Clock empty-key bug (per Arcurus 2026-06-05 #openworld).
    use super::parse_llm_action_response;

    #[test]
    fn strict_parse_ok_returns_none_repair_warning() {
        // A well-formed LLM response must round-trip cleanly
        // and signal `None` for the repair warning.
        let ok = r#"{
            "action": "study_ancient_tome",
            "outcome": "Learns a new spell.",
            "effects": {"knowledge": 1},
            "narrative": "The scholar pores over the brittle pages.",
            "history_summary": "Studied ancient magic."
        }"#;
        let (parsed, warning) = parse_llm_action_response(ok)
            .expect("well-formed response must parse");
        assert_eq!(parsed.action, "study_ancient_tome");
        assert!(warning.is_none(), "strict-parse-ok path must not emit a repair warning; got {:?}", warning);
    }

    #[test]
    fn broken_history_summary_replace_triggers_repair_warning() {
        // The exact World-Clock 2026-06-05 15:42 malformation:
        //   {"old_part":"...","":"new_part":"..."}
        // Strict serde rejects it; the regex fixes it; the
        // function must return Some(warning) so the caller can
        // surface it in the response payload.
        let broken = r#"{
            "action": "broadcast_chronicle_update",
            "outcome": "The World Clock's narration shifts.",
            "effects": {"influence": 0},
            "narrative": "The clock's tone changes.",
            "history_summary": "Now collecting era markers.",
            "history_summary_replace": [{"old_part":"Currently broadcasting...Post-Harmonization Era.","":"new_part":"Now actively collecting..."}]
        }"#;
        let (parsed, warning) = parse_llm_action_response(broken)
            .expect("repaired response must parse");
        assert_eq!(parsed.action, "broadcast_chronicle_update");
        let w = warning.expect("repair path must emit a warning");
        // The warning must be descriptive enough for an operator
        // grepping the JSONL log to recognize what happened.
        assert!(w.contains("parse_llm_action_response"), "warning should identify source fn: {}", w);
        assert!(w.contains("empty-key"), "warning should name the bug class: {}", w);
    }

    #[test]
    fn unparseable_garbage_returns_err_with_no_warning() {
        // Truly broken input (not the known pattern) must still
        // return Err — the regex is conservative and only fixes
        // shapes we've actually seen. No `Ok` is possible here,
        // so we never need to inspect the warning field.
        let garbage = r#"this is not json at all"#;
        let result = parse_llm_action_response(garbage);
        assert!(result.is_err(), "unparseable input must error");
    }
}
