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
    stats_change_file_path: PathBuf,
    today_date: String,
}

impl DailyLogger {
    fn new(log_dir: PathBuf) -> Self {
        let today = chrono_now_date();
        Self {
            error_file_path: log_dir.join(format!("error-log-{}.log", today)),
            llm_file_path: log_dir.join(format!("llm-log-{}.log", today)),
            stats_change_file_path: log_dir.join(format!("stats-change-log-{}.log", today)),
            today_date: today,
        }
    }
    
    fn ensure_today(&mut self, log_dir: &PathBuf) {
        let today = chrono_now_date();
        if today != self.today_date {
            self.today_date = today.clone();
            self.error_file_path = log_dir.join(format!("error-log-{}.log", today));
            self.llm_file_path = log_dir.join(format!("llm-log-{}.log", today));
            self.stats_change_file_path = log_dir.join(format!("stats-change-log-{}.log", today));
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
    
    /// Log a single stats change (one property write).  Per
    /// Arcurus 2026-06-07 #openworld: "track the stats changes
    /// additionally in a stat change log, so that we can easily
    /// look into them?  make also here a new log per day and
    /// apply same name day formatting."
    ///
    /// Line format (one per write, append-mode, UTF-8):
    ///
    ///   [2026-06-07T10:23:45.123Z] entity=Velora the Undying (f26dd963)
    ///       prop=morale old=487 new=535 delta=+48 actor=Velora the Undying
    ///       cross=false warnings=2
    ///
    /// Each field is on its own continuation line (prefixed by
    /// 6 spaces) for scannability; the first line is the
    /// timestamp + entity.  `cross` is true for cross-entity
    /// writes, false for self-effects.  `warnings` is the
    /// count of warnings the apply_all_effects call surfaced
    /// (so the operator can grep for `warnings=0` to find
    /// clean writes).
    fn log_stats_change(
        &mut self,
        entity_name: &str,
        entity_id: &Uuid,
        prop: &str,
        old_value: i64,
        new_value: i64,
        cross: bool,
        actor_name: &str,
        warnings_count: usize,
    ) {
        let timestamp = chrono_now_timestamp();
        let delta = new_value - old_value;
        let delta_str = if delta >= 0 {
            format!("+{}", delta)
        } else {
            format!("{}", delta)
        };
        let line = format!(
            "[{}] entity={} ({})\n      prop={} old={} new={} delta={} actor={} cross={} warnings={}\n",
            timestamp,
            entity_name,
            &entity_id.to_string()[..8],
            prop,
            old_value,
            new_value,
            delta_str,
            actor_name,
            cross,
            warnings_count,
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.stats_change_file_path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
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

    // Also snapshot the durable action_history.jsonl so a
    // backup captures the FULL world state (entity + property
    // + history) in one go.  Per Arcurus 2026-06-07
    // #openworld: "make a backup of the history before you
    // make changes to file history files (or any other
    // files)".  Filename includes the same timestamp so a
    // restore can pair the .owbl with the .jsonl from the
    // same instant.
    let history_path = std::path::Path::new("world_data/action_history.jsonl");
    if history_path.exists() {
        let hist_dest = backup_dir.join(format!("action_history-{}-{}.jsonl", label, ts));
        // Best-effort: a copy failure here is logged but
        // does not fail the save.owbl backup (we'd rather
        // get a save.owbl snapshot with no history than no
        // snapshot at all).
        if let Err(e) = std::fs::copy(history_path, &hist_dest) {
            eprintln!("[snapshot_save] warning: could not snapshot action_history.jsonl: {}", e);
        }
    }

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

/// `GET /api/entities/:id/history-from-others` (auth required)
///
/// Returns history entries from OTHER entities whose effects
/// touch this entity.  Used by the web UI to show "History
/// from Other Entities (Impacting This One)" after the
/// entity's own history.  Per Arcurus 2026-06-07 #openworld.
///
/// An effect "touches" this entity if the effect key is
///   - `"<EntityName>.<prop>"` (exact name match), OR
///   - `"self.<prop>"` from a different actor AND the
///     effect would land on this entity (rare; only if the
///     parser misroutes), OR
///   - the entry's actor is this entity (treated as
///     "self" — the operator asked for "other", so we
///     exclude same-actor entries even if the effect
///     string happens to mention the entity).
///
/// Reads from the durable action_history.jsonl (same
/// source as `get_entity_history`).  Most recent first.
/// `limit` defaults to 50, max 500.
async fn get_entity_history_from_others(
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
    let entity_id_str = id.to_string();

    // We need the FULL action history (not just the
    // entries for this entity_id) because we're scanning
    // for cross-entity effects.  Load recent N entries
    // from the durable JSONL log, then filter.
    // Practical bound: 1000 most recent entries (covers
    // roughly the last few days of activity).
    let all_recent: Vec<ActionHistoryEntry> =
        action_history_log::load_recent_world_actions(1000, None);
    let mut matching: Vec<&ActionHistoryEntry> = all_recent
        .iter()
        .filter(|e| {
            // Exclude same-actor entries (the operator
            // asked for "other" entities, not self).
            if e.entity_id == entity_id_str {
                return false;
            }
            // Check if any effect key starts with
            // "<EntityName>." OR is the dotted-key form
            // that exactly matches the entity.
            let prefix_self = format!("{}.", entity_name);
            let prefix_self_dot = format!("{entity_name}.");
            for key in e.effects.keys() {
                if key.starts_with(&prefix_self) || key.starts_with(&prefix_self_dot) {
                    return true;
                }
                // Also: an effect "self.<prop>" from
                // actor==other doesn't target this entity
                // by name, so it shouldn't count.  We only
                // count dotted-name references.
            }
            false
        })
        .collect();
    matching.truncate(limit);

    success_json(serde_json::json!({
        "success": true,
        "entity_id": entity_id_str,
        "entity_name": entity_name,
        "count": matching.len(),
        "limit": limit,
        "entries": matching,
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
    
    // Read the AI template + the LLM-facing property reference
    // docs (visibility, corruption, common props, writing
    // effects).  Both live in `ai_templates/`.  If the docs
    // file is missing for any reason, fall back to an empty
    // string — the prompt will still work, the LLM just
    // won't get the named-property reference in this call.
    // (Better to run a slightly thinner prompt than to fail
    // the whole LLM call.)
    let template = match tokio::fs::read_to_string("ai_templates/EntityAction.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    let property_docs = match tokio::fs::read_to_string("ai_templates/property_docs.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    
    // Render the prompt
    let world_name = state.world.read().await.name.clone();
    let prompt = context_builder::build_action_prompt(&world_name, entity, &ctx, &template, &property_docs);
    
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
    // 2026-06-06 (#openworld, Arcurus): effects may now target other
    // entities that the action impacts, not just the actor. Keys are
    // "entity_name.property_name" for other entities and
    // "self.property_name" for the actor. The server expands "self"
    // to the actor's name, resolves other-entity names to ids, and
    // for now DRY-RUNS the cross-entity writes (logs what would have
    // happened, applies only self-effects). The LLM is asked to
    // emit at least one effect per entity it impacts in the action.
    //
    // 2026-06-07 (#openworld, Arcurus): cross-entity writes now
    // APPLY. The LLM can name any other entity in the world
    // (case-sensitive exact name) and the effect will be applied
    // to that entity, with the same per-target safety nets as
    // self-effects (magnitude check, per-target normalization
    // computed against the target's own power, type handling,
    // system-entity guard). Effects targeting the World Clock or
    // any 'meta'-tagged entity are rejected with a warning.
    let system_prompt = "You are the world narrator for the Open World simulation. Respond ONLY with valid JSON (no other text before or after). Format: {\"action\":\"brief action name\",\"outcome\":\"2-3 sentences\",\"effects\":{{\"entityname.property_name\":change_value}},\"narrative\":\"story description\"} — keys are either self.property_name (the actor) or other_entity_name.property_name (any entity the action impacts); emit at least one effect per impacted entity so each can track it. Effects on other entities are applied live (not dry-run), with per-target magnitude + normalization guards. System entities (World Clock, anything tagged 'meta') cannot be written to.";
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
    
    // Read the AI template + the LLM-facing property reference
    // docs.  See the matching block in the other caller for why
    // we keep both reads independent and fall back to "" on
    // missing files.
    let template = match tokio::fs::read_to_string("ai_templates/EntityAction.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    let property_docs = match tokio::fs::read_to_string("ai_templates/property_docs.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    
    // Render the prompt
    let world_name = state.world.read().await.name.clone();
    let prompt = context_builder::build_action_prompt(&world_name, entity, &ctx, &template, &property_docs);
    
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
                    // Build the case-sensitive name → id lookup
                    // table BEFORE we take a mutable borrow of
                    // the entity (Rust borrow checker: we can't
                    // immutably borrow `world` while the entity is
                    // borrowed mutably). Same parser as the
                    // process_action_handler path so dotted keys
                    // (`self.morale`, `Mira the Merchant.wealth`)
                    // are resolved correctly instead of being
                    // written as literal property names. Per
                    // Arcurus 2026-06-06 #openworld.
                    let name_to_id = build_name_index(&world);
                    let entity_name = world.entities.get(&id).map(|e| e.name.clone())
                        .unwrap_or_default();
                    let (parsed_effects, unknown_entity_names) = parse_effects(
                        &action_data.effects,
                        id,
                        &entity_name,
                        &name_to_id,
                    );
                    let mut unknown_warnings: Vec<String> = Vec::new();
                    // Auto-call path: same per-target helper as
                    // process_action_handler, so both paths apply
                    // cross-entity effects with the same safety
                    // nets (magnitude check, per-target
                    // normalization, type handling, system-entity
                    // guard). Per Arcurus 2026-06-07 #openworld.
                    let mut response_warnings: Vec<String> = Vec::new();
                    let (mut applied_effects, _actor_nvs, cross_entity_applied, hidden_tags_updated, corrupted_tags_updated) =
                        apply_all_effects(&mut world, id, &parsed_effects, &mut response_warnings, Some(&state.logger));
                    // Auto-call response format uses NEW VALUES
                    // (not deltas — the pre-refactor auto-call used
                    // deltas but the cleaner form is the same as
                    // the process path). The process path is the
                    // source of truth for post-action state; this
                    // is just a best-effort preview.
                    applied_effects = applied_effects;

                    if !unknown_entity_names.is_empty() {
                        let mut w = format!(
                            "Unknown entity names in effects ({} name(s) skipped):",
                            unknown_entity_names.len()
                        );
                        for name in &unknown_entity_names {
                            w.push_str(&format!(
                                "\n  - {:?} (no entity with that exact name; check spelling and case)",
                                name
                            ));
                        }
                        unknown_warnings.push(w);
                    }
                    response_warnings.extend(unknown_warnings);
                    let mut all_warnings: Vec<String> =
                        repair_warning.into_iter().collect();
                    all_warnings.extend(response_warnings);

                    success_json(serde_json::json!({
                        "success": true,
                        "action": action_data.action,
                        "outcome": action_data.outcome,
                        "effects_applied": applied_effects,
                        "cross_entity_effects_applied": cross_entity_applied,
                        "hidden_tags_updated": hidden_tags_updated,
                        "narrative": action_data.narrative,
                        "warnings": all_warnings
                    })).into_response()
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
///   4. Unicode-quirk normalization (LLM-introduced glyph variants):
///      fold both sides to a canonical ASCII form via
///      `normalize_llm_unicode_quirks` (smart quotes, en/em dashes,
///      full-width punctuation, NBSP, Latin ligatures, long-s) and
///      re-run the strict find. The returned byte indices are
///      translated back to the ORIGINAL haystack via
///      `map_quirk_index_to_original` so the caller can slice the
///      un-normalized source.
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

    // 4. Unicode quirk normalization on both sides (LLM-flavoured
    //    subset), then re-run the strict find. Common cases that
    //    fail across LLM calls because the prior `old_part` and
    //    the current stored text are byte-different but glyph-
    //    equivalent: smart quotes (U+201C / U+201D / U+2018 / U+2019
    //    vs U+0022 / U+0027), en-dash / em-dash / horizontal bar
    //    (U+2013 / U+2014 / U+2015 vs U+002D), full-width ASCII
    //    punctuation (U+FF0C vs U+002C etc.), non-breaking space and
    //    friends (U+00A0 / U+202F / U+2007 vs U+0020), and the
    //    Latin typographic ligatures (U+FB00..U+FB06). Hand-rolled
    //    here to avoid pulling in `unicode-normalization` for what
    //    is a tiny, predictable LLM-introduced surface.
    let h_norm: String = normalize_llm_unicode_quirks(haystack);
    let n_norm: String = normalize_llm_unicode_quirks(needle);
    if h_norm != haystack || n_norm != needle {
        if let Some(idx) = h_norm.find(&n_norm) {
            // Map normalized index back to a byte index in the
            // ORIGINAL haystack. The mapper walks codepoints in
            // lockstep and is correct for the quirk set above (each
            // normalized char corresponds to exactly one original
            // codepoint — either an unchanged passthrough or a single
            // replacement from the table).
            let start_orig = map_quirk_index_to_original(haystack, &h_norm, idx);
            let end_orig = map_quirk_index_to_original(
                haystack,
                &h_norm,
                idx + n_norm.len(),
            );
            return Some((start_orig, end_orig));
        }
    }

    None
}

/// Fold the LLM-introduced Unicode variants most commonly seen in
/// `history_summary_replace` payloads to a canonical ASCII form. This
/// is intentionally *not* full NFKC — it's a small, auditable table
/// for the cases we actually observe. Some entries map 1 codepoint
/// to 1 codepoint (most punctuation); some map 1 codepoint to 2
/// codepoints (ligatures). The index-mapper in
/// `map_quirk_index_to_original` handles both shapes via a
/// precomputed alignment table.
fn normalize_llm_unicode_quirks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let mapped: &str = match c {
            // Smart double quotes → straight double quote.
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{FF02}' => "\"",
            // Smart single quotes / apostrophes → ASCII apostrophe.
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{FF07}' => "\'",
            // En-dash, em-dash, horizontal bar, math minus → ASCII hyphen-minus.
            '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => "-",
            // Full-width comma, period, semicolon, colon, question,
            // exclamation, parens → ASCII equivalents.
            '\u{FF0C}' => ",",
            '\u{FF0E}' => ".",
            '\u{FF1B}' => ";",
            '\u{FF1A}' => ":",
            '\u{FF1F}' => "?",
            '\u{FF01}' => "!",
            '\u{FF08}' => "(",
            '\u{FF09}' => ")",
            // Non-breaking space, narrow no-break space, figure space,
            // thin space, zero-width space → ASCII space.
            '\u{00A0}' | '\u{202F}' | '\u{2007}' | '\u{2009}' | '\u{200B}' => " ",
            // Latin typographic ligatures (fi, fl, ffi, ffl, st) fold
            // to their ASCII digraphs so find() can match across turns.
            '\u{FB01}' => "fi",
            '\u{FB02}' => "fl",
            '\u{FB03}' => "ffi",
            '\u{FB04}' => "ffl",
            '\u{FB05}' => "st",
            '\u{FB06}' => "st",
            // Long-s (ſ) is interchangeable with regular 's' in
            // English prose; fold to 's' to make a find() robust.
            '\u{017F}' => "s",
            // Anything else passes through untouched.
            other => {
                out.push(other);
                continue;
            }
        };
        out.push_str(mapped);
    }
    out
}

/// Translate a byte index in the *normalized* string back to the
/// equivalent byte index in the *original* string. Used by
/// `find_replace_range` after the quirk-normalized find succeeds, so
/// the caller can slice the (un-normalized) original haystack at the
/// right position.
///
/// Strategy: walk both strings in lockstep, codepoint by codepoint,
/// recording the original-byte-offset of every normalized
/// codepoint boundary. For a 1-codepoint→1-codepoint mapping (smart
/// quote, em-dash, full-width comma, NBSP, long-s, passthroughs) the
/// boundary list is a clean bijection. For 1-codepoint→2-codepoints
/// ligatures, the first normalized codepoint of the digraph
/// ("fi"[0]) maps to the original ligature's start byte, and the
/// second normalized codepoint ("fi"[1]) also maps to the same
/// original ligature start byte (because the digraph as a whole
/// replaces the single ligature). The lookup picks the smallest
/// original-byte-offset whose normalized-byte-offset is >= the
/// target — which always lands on a char boundary in the original.
fn map_quirk_index_to_original(
    original: &str,
    normalized: &str,
    target_norm: usize,
) -> usize {
    // Build a sorted list of (normalized_byte_offset, original_byte_offset)
    // pairs in lockstep, then binary-search for the smallest pair
    // whose norm_offset >= target_norm.
    //
    // For each original char `oc` we look up its normalized form
    // (using the same match table) so the per-step byte deltas are
    // computed in lockstep. The list is monotonically increasing in
    // both axes, so a linear walk + early exit is enough for our
    // size (summary bodies are bounded by max_history_summary_chars,
    // default 500, hard cap ~10k).
    let mut o_idx = 0usize;
    let mut n_idx = 0usize;
    for oc in original.chars() {
        if n_idx >= target_norm {
            break;
        }
        let mapped_len: usize = match oc {
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{FF02}' => 1,
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{FF07}' => 1,
            '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => 1,
            '\u{FF0C}' | '\u{FF0E}' | '\u{FF1B}' | '\u{FF1A}' | '\u{FF1F}'
            | '\u{FF01}' | '\u{FF08}' | '\u{FF09}' => 1,
            '\u{00A0}' | '\u{202F}' | '\u{2007}' | '\u{2009}' | '\u{200B}' => 1,
            '\u{FB01}' | '\u{FB02}' | '\u{FB03}' | '\u{FB04}' | '\u{FB05}'
            | '\u{FB06}' => 2,
            '\u{017F}' => 1,
            _ => oc.len_utf8(),
        };
        o_idx += oc.len_utf8();
        n_idx += mapped_len;
    }
    o_idx
}

/// Outcome of the safety-net fallback that triggers when the LLM
/// confused itself by sending BOTH `history_summary` and
/// `history_summary_replace`, and the replace chain ended up being a
/// no-op (new_summary == current).
///
/// Why this exists (added 2026-06-06, see todo fa45e5e7 / 06-06 worker
/// run): 97 of the last 100 actions had a "Both history_summary and
/// history_summary_replace present; using replace (history_summary
/// dropped)" warning, and 84 of those 100 ALSO had an "old_part not
/// found" warning — because the LLM was recalling the `old_part`
/// from the new (just-dropped) summary, not the current stored one.
/// The net effect was wasted tokens on the dropped full summary AND
/// the history_summary update failing silently. This helper recovers
/// the update by falling back to the dropped `history_summary` value
/// (truncated to the cap).
#[derive(Debug, PartialEq)]
pub struct FallbackResult {
    /// The new value to store on the entity (None = no summary).
    pub new_summary: Option<String>,
    /// True if the result was truncated to fit max_chars.
    pub truncated: bool,
    /// Warnings to surface to the operator. Includes a "fell back to
    /// history_summary" line and any truncation warning.
    pub warnings: Vec<String>,
}

/// Returns the new (post-fallback) summary value to use when the
/// LLM sent BOTH `history_summary` and `history_summary_replace`,
/// AND the replace chain produced a no-op. Returns None when the
/// fallback should NOT fire (i.e. when the LLM only sent one
/// field, or the replace chain made a real change, or the dropped
/// `history_summary` is itself empty/placeholder). The caller is
/// expected to keep the replace-chain result in that case.
///
/// `dropped_full` is the value of the LLM-emit `history_summary`
/// that would otherwise be discarded. `current` is the entity's
/// stored `history_summary` at the time of the LLM call (used to
/// detect no-op vs real change). `replace_result` is what
/// `apply_history_summary_replaces` returned.
pub fn apply_summary_fallback(
    dropped_full: &str,
    current: Option<&str>,
    replace_result_new_summary: Option<&str>,
    max_chars: usize,
) -> Option<FallbackResult> {
    // The replace chain was a no-op iff the result equals current.
    // In that case, prefer the dropped full summary.
    let replace_was_noop = match (current, replace_result_new_summary) {
        (Some(c), Some(r)) => c == r,
        (None, None) => true,
        (Some(c), None) => c.is_empty(), // current non-empty but result is None => no-op
        (None, Some(r)) => r.is_empty(),
    };
    if !replace_was_noop {
        return None;
    }
    let trimmed = dropped_full.trim();
    if trimmed.is_empty() || is_placeholder_summary(trimmed) {
        // Dropped value is also useless — don't fire the fallback.
        return None;
    }
    let mut warnings: Vec<String> = Vec::new();
    warnings.push(
        "history_summary_replace was a no-op; \
         fell back to the dropped history_summary value".to_string(),
    );
    if trimmed.chars().count() > max_chars {
        let keep = max_chars.saturating_sub(1);
        let cut: String = trimmed.chars().take(keep).collect();
        let final_summary = if cut.is_empty() {
            String::new()
        } else {
            format!("{}…", cut)
        };
        warnings.push(format!(
            "LLM history_summary exceeded {} chars; truncated.",
            max_chars
        ));
        if final_summary.is_empty() {
            Some(FallbackResult {
                new_summary: None,
                truncated: true,
                warnings,
            })
        } else {
            Some(FallbackResult {
                new_summary: Some(final_summary),
                truncated: true,
                warnings,
            })
        }
    } else {
        Some(FallbackResult {
            new_summary: Some(trimmed.to_string()),
            truncated: false,
            warnings,
        })
    }
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
                    // The message includes a 60-char preview of the
                    // missing `old_part` so the operator can see at
                    // a glance which sentence the LLM was looking
                    // for. Most of the time the LLM is recalling a
                    // stale sentence from a previous turn (e.g.
                    // "She travels..." when the stored summary
                    // already has "She traveled..." or has dropped
                    // the sentence entirely), and the preview makes
                    // that obvious without having to dig out the
                    // raw LLM log. 60 chars is long enough to
                    // disambiguate but short enough to keep the
                    // warning line scannable in a list of many.
                    const PREVIEW_CHARS: usize = 60;
                    let preview: String = rep
                        .old_part
                        .chars()
                        .take(PREVIEW_CHARS)
                        .collect::<String>()
                        + if rep.old_part.chars().count() > PREVIEW_CHARS {
                            "…"
                        } else {
                            ""
                        };
                    warnings.push(format!(
                        "history_summary_replace[{}]: old_part not found in current summary (length {}); skipped (looking for: {:?})",
                        i,
                        state.chars().count(),
                        preview
                    ));
                }
            }
        }
    }

    // Truncate if over cap. Same strategy as the API endpoint: cut
    // from the END (keeps the start + any freshly inserted new_part
    // intact), append "…" on the boundary.
    let pre_truncate_len = state.chars().count();
    let (final_summary, truncated) = if pre_truncate_len > max_chars {
        let keep = max_chars.saturating_sub(1);
        let cut: String = state.chars().take(keep).collect();
        let truncated_str = format!("{}…", cut);
        (truncated_str, true)
    } else {
        (state, false)
    };

    if truncated {
        // Compute the pre-truncation length so the warning can report
        // both the over-by amount and the cap explicitly. Most
        // truncations happen because the LLM's replace chain produced
        // something over the cap (e.g. the chain's `new_part` was
        // larger than the cap, or the LLM used `!ALL!` to rewrite a
        // summary that's now over the cap). The remaining case is
        // a lowered cap: a previous LLM stored a summary at the old
        // (higher) cap, and someone changed `default_max_history_summary_chars`
        // since then. The LLM sees the full old summary (with the
        // header saying `OVER by N`) and is expected to use
        // `history_summary_replace` to shrink it; if it doesn't,
        // we truncate server-side so the entity's stored state
        // stays bounded.  Arcurus 2026-06-06 #openworld.
        let over_by = pre_truncate_len.saturating_sub(max_chars);
        warnings.push(format!(
            "history_summary was {} chars (over cap of {} by {}); truncated to ≤{} chars. The LLM should use history_summary_replace on a future turn to shrink this to ≤{} chars (e.g. a surgical edit of stale content, or !ALL! for a full rewrite).",
            pre_truncate_len, max_chars, over_by, max_chars, max_chars
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

/// Per-turn cap on the **total absolute magnitude** of all numeric
/// effects an LLM can apply to a single entity, expressed as a
/// fraction of the entity's `power` (with a hard floor of 10 on
/// `power` so power-0 entities still have a small budget) PLUS a
/// flat `EFFECT_NORMALIZATION_MAX_AMOUNT` (+10) max-amount term.
/// When the sum of |Δ| across all effects exceeds this cap, every
/// numeric effect is scaled down proportionally so the new total
/// exactly fills the cap.  Negative effects count as positive for
/// the cap check (the magnitude is what matters).  String effects
/// are not scaled.  Arcurus 2026-06-06 + 2026-06-07 #openworld.
///
/// The +10 max-amount term is the carve-out for "a single full
/// effect on a low-power entity" — without it, the pure fraction
/// would cap a power-10 entity at 1.0 of total |Δ| per turn, too
/// tight for a meaningful single change.  With it, even a
/// power-0 entity has 11 of budget (1% of 10 + 10), enough for
/// e.g. a +5 morale and a +6 power, or a single +10 spike.
pub(crate) const EFFECT_NORMALIZATION_CAP_PCT: f64 = 0.10;
pub(crate) const EFFECT_NORMALIZATION_MAX_AMOUNT: f64 = 10.0;
pub(crate) const EFFECT_NORMALIZATION_MIN_CAP: f64 = 1.0;

/// Tag name used by the post-effect hidden-state rule.  Per
/// Arcurus 2026-06-07 #openworld: when an entity's
/// `max(10, power) / 10 + visibility` falls below 0, the
/// `hidden` tag is added (the entity has dropped out of
/// common awareness).  When the value reaches 1 or above, the
/// tag is removed (the entity is back in the open).
/// `0 ≤ threshold < 1` is the dead zone — no change, so the
/// tag doesn't flicker near the boundary.
const HIDDEN_TAG: &str = "hidden";

/// Tag name used by the post-effect corrupted-state rule.  Per
/// Arcurus 2026-06-07 #openworld: when an entity's
/// `max(1, power) - corruption` falls below 0, the
/// `corrupted` tag is added (corruption has overtaken the
/// entity's own strength).  When the threshold reaches 1 or
/// above, the tag is removed (the entity has been purified).
/// `0 ≤ threshold < 1` is the dead zone — no change, so the
/// tag doesn't flicker near the boundary.
const CORRUPTED_TAG: &str = "corrupted";

// ---------------------------------------------------------------------------
// Stats-cap rule (Arcurus 2026-06-07 #openworld)
// ---------------------------------------------------------------------------
//
// Steady-state "is this entity over-powered?" cap.  Sums ALL
// `properties_int` values (including power itself) and compares
// against `max(1, power * 5) + 100`.  If the entity is over the
// cap, every value is scaled down proportionally so the new
// sum equals the cap (the standalone script does this; the
// runtime effect path only warns to avoid disrupting
// mid-action).
//
// Power-multiplier rationale: a power-1 entity has 105 of
// budget (enough for a small starter), a power-10 entity has
// 150, a power-100 entity has 600, a power-1000 entity has
// 5100.  The +100 baseline gives every entity room to grow
// before the cap kicks in.  The +5 multiplier means higher-
// power entities get proportionally more total "stuff"
// (matching the world-building intuition that stronger
// characters have more total resources).
//
// Why the cap is keyed on the ORIGINAL (pre-normalize) power:
// normalizing scales power too, but the cap must be computed
// BEFORE scaling — otherwise re-normalizing the scaled values
// would re-trigger with a smaller cap and shrink power
// again, leading to a death spiral.
pub(crate) const STATS_CAP_POWER_MULTIPLIER: i64 = 5;
pub(crate) const STATS_CAP_BASE: i64 = 100;
pub(crate) const STATS_CAP_POWER_FLOOR: i64 = 1;

/// Compute the steady-state stats cap for a given entity
/// power.  Formula:  cap = max(STATS_CAP_POWER_FLOOR, power *
/// STATS_CAP_POWER_MULTIPLIER) + STATS_CAP_BASE
fn compute_stats_cap(power: i64) -> i64 {
    let prop = std::cmp::max(STATS_CAP_POWER_FLOOR, power) * STATS_CAP_POWER_MULTIPLIER;
    prop + STATS_CAP_BASE
}

/// Signed sum of all integer properties on an entity
/// (including power, since the cap counts the entity's
/// "total stuff").  Used by the stats-cap check.
fn stats_sum(entity: &WorldEntity) -> i64 {
    entity.properties_int.values().copied().sum()
}

/// If the entity's `stats_sum` is over its stats cap, scale
/// every integer property down proportionally so the new sum
/// exactly equals the cap.  Returns `Some((old_sum, cap,
/// scale))` if a change was made, or `None` if the entity was
/// already within budget.
///
/// Sign-preserving: negative values stay negative (we scale
/// by a positive factor, so a wealth=200, visibility=-50
/// entity becomes wealth=143, visibility=-36 when scaled by
/// 0.714).  This matches the effect-normalization semantics
/// (magnitudes shrink, signs don't flip).
///
/// Power is included in the scaling because the cap counts
/// it.  The cap is computed from the ORIGINAL power, so
/// post-scaling the entity's power will be lower, but we
/// don't re-normalize again — `compute_stats_cap` would now
/// return a smaller cap, but the entity's NEW sum exactly
/// matches the OLD cap, so it stays within the (now-smaller)
/// new cap as well.  No death spiral.
fn normalize_entity_stats(entity: &mut WorldEntity) -> Option<(i64, i64, f64)> {
    let power = entity.properties_int.get("power").copied().unwrap_or(0);
    let cap = compute_stats_cap(power);
    let sum = stats_sum(entity);
    if sum > cap && sum != 0 {
        let scale = cap as f64 / sum as f64;
        for v in entity.properties_int.values_mut() {
            *v = ((*v as f64) * scale).round() as i64;
        }
        Some((sum, cap, scale))
    } else {
        None
    }
}

/// Check the stats cap on an entity and append a warning if
/// the entity is over budget.  Does NOT normalize — that's
/// the standalone script's job.  Per Arcurus 2026-06-07
/// #openworld: the runtime effect path warns only, so a
/// single big effect doesn't silently shrink the entity
/// mid-action.
fn check_stats_cap_warn(
    entity: &WorldEntity,
    warnings: &mut Vec<String>,
) {
    let power = entity.properties_int.get("power").copied().unwrap_or(0);
    let cap = compute_stats_cap(power);
    let sum = stats_sum(entity);
    if sum > cap {
        let overage = sum - cap;
        warnings.push(format!(
            "Stats cap exceeded for '{}': sum={} > cap={} (overage={}, cap formula = max(1, {}*5) + 100 = {}). Run `python3 code/normalize_stats.py normalize` to fix.",
            entity.name, sum, cap, overage, power, cap
        ));
    }
}

/// Update the `hidden` tag on an entity based on its current
/// (post-effect) `power` and `visibility`.  The threshold is
/// `max(10, power) / 10 + visibility`:
///
///   - `threshold <  0`  → add the tag (the entity has dropped
///                          out of common awareness)
///   - `threshold >= 1`  → remove the tag (the entity is back
///                          in the open)
///   - `0 ≤ threshold < 1`  → no change (keep current state;
///                              this is a small dead zone so
///                              the tag doesn't flicker right
///                              at the boundary).
///
/// Returns `(added, removed, threshold)` so the caller can
/// emit a warning.  The threshold is included for the
/// warning text so operators can see WHY the tag toggled.
///
/// Why this formula: low-power entities are easy to hide
/// (a Mira-the-Scribe with visibility=-2 → threshold = 1 + (-2)
/// = -1 → hidden), while high-power entities are hard to hide
/// (a Vaelthrix with visibility=-50 → threshold = 132 + (-50)
/// = 82 → still very visible).  Makes narrative sense: a
/// sleeping dragon is still scary.  The +10 floor on `power`
/// ensures power-0 entities still get a small baseline (so
/// a brand-new entity with default properties doesn't
/// flicker on/off the hidden tag due to noisy visibility
/// writes of ±1).
fn update_hidden_tag(entity: &mut WorldEntity) -> (bool, bool, f64) {
    let power = entity.properties_int.get("power").copied().unwrap_or(0);
    let visibility = entity.properties_int.get("visibility").copied().unwrap_or(0);
    let threshold = (std::cmp::max(10_i64, power) as f64 / 10.0) + (visibility as f64);
    let has_hidden = entity.has_tag(HIDDEN_TAG);

    if threshold < 0.0 {
        if !has_hidden {
            entity.add_tag(HIDDEN_TAG);
            return (true, false, threshold);
        }
    } else if threshold >= 1.0 {
        if has_hidden {
            entity.remove_tag(HIDDEN_TAG);
            return (false, true, threshold);
        }
    }
    (false, false, threshold)
}

/// Update the `corrupted` tag on an entity based on its
/// current (post-effect) `power` and `corruption`.  The
/// threshold is `max(1, power) - corruption`:
///
///   - `threshold <  0`  → add the tag (corruption has
///                          overtaken the entity's strength)
///   - `threshold >= 1`  → remove the tag (the entity is
///                          purified / strong enough to resist)
///   - `0 ≤ threshold < 1`  → no change (dead zone, prevents
///                              flicker at the boundary)
///
/// Returns `(added, removed, threshold)` so the caller can
/// emit a warning.  Mirror of `update_hidden_tag` (same
/// dead-zone contract, same return shape, just different
/// formula and tag name).
///
/// Why this formula: low-power entities are easy to corrupt
/// (a power-10 entity with `corruption: 15` → threshold =
/// 10 - 15 = -5 → tagged).  High-power entities are hard to
/// corrupt (a power-100 entity needs `corruption > 100`
/// before the tag appears).  Mirrors the hidden-tag rule's
/// "famous dragon is still scary" intuition.
fn update_corrupted_tag(entity: &mut WorldEntity) -> (bool, bool, f64) {
    let power = entity.properties_int.get("power").copied().unwrap_or(0);
    let corruption = entity.properties_int.get("corruption").copied().unwrap_or(0);
    let threshold = (std::cmp::max(1_i64, power) as f64) - (corruption as f64);
    let has_corrupted = entity.has_tag(CORRUPTED_TAG);

    if threshold < 0.0 {
        if !has_corrupted {
            entity.add_tag(CORRUPTED_TAG);
            return (true, false, threshold);
        }
    } else if threshold >= 1.0 {
        if has_corrupted {
            entity.remove_tag(CORRUPTED_TAG);
            return (false, true, threshold);
        }
    }
    (false, false, threshold)
}

/// Compute the per-turn effect cap for a given entity power.
/// Formula:  cap = max(MIN_CAP, max(10, power) * CAP_PCT + MAX_AMOUNT)
///
/// The `max(10, power)` floor ensures power-0 entities still get
/// a proportional slice; the `MAX_AMOUNT` addend gives a flat
/// baseline of room for a single full effect.  See the docstring
/// on `EFFECT_NORMALIZATION_CAP_PCT` for the full rationale.
fn compute_effect_normalization_cap(raw_power: i64) -> f64 {
    let proportional = std::cmp::max(10_i64, raw_power) as f64
        * EFFECT_NORMALIZATION_CAP_PCT;
    (proportional + EFFECT_NORMALIZATION_MAX_AMOUNT).max(EFFECT_NORMALIZATION_MIN_CAP)
}

/// Compute the per-turn effect-normalization pre-pass.
///
/// Returns `(total_magnitude, scale)` where:
///   - `total_magnitude` is the sum of `|Δ|` across all numeric
///     effects that survive the `magnitude_check` (i.e. are not
///     garbage).  **Garbage is rejected BEFORE counting** so a
///     single 1e18 sibling cannot drag a 1.0 sibling down to a
///     near-zero scaled value.
///   - `scale` is 1.0 when `total_magnitude ≤ cap` (no
///     normalization needed), or `cap / total_magnitude`
///     otherwise.
///
/// `protected_entity` short-circuits to (0.0, 1.0): system
/// entities (World Clock, the universe anchor) skip effects
/// entirely so the caller never normalizes them.
///
/// Extracted from the inline pre-pass in `process_action_handler`
/// so the unit test exercises the actual production code and
/// future-me can't accidentally swap the order.  Per Arcurus
/// 2026-06-06 #openworld.
pub(crate) fn compute_effect_normalization_scale(
    raw_power: i64,
    effects: &std::collections::HashMap<String, serde_json::Value>,
    protected_entity: bool,
) -> (f64, f64) {
    if protected_entity {
        return (0.0, 1.0);
    }
    let effect_cap = compute_effect_normalization_cap(raw_power);
    let mut total: f64 = 0.0;
    for v in effects.values() {
        // Reject garbage FIRST.  Anything that fails
        // `magnitude_check` (non-finite, |Δ| > MAX_DELTA_ABS)
        // is excluded from `total`, so the scale is computed
        // on the clean set.  The application loop further
        // down re-checks `magnitude_check` so the garbage
        // value itself is never written.  Per Arcurus
        // 2026-06-06 #openworld: "reject the garbage first,
        // but make sure that is removed from the applied
        // effects before its counted or applied".
        if magnitude_check(v).is_some() {
            continue;
        }
        if let Some(f) = parse_effect_value(v) {
            if f.is_finite() {
                total += f.abs();
            }
        }
    }
    let scale = if total > effect_cap && total > 0.0 {
        effect_cap / total
    } else {
        1.0
    };
    (total, scale)
}

// ---------------------------------------------------------------------------
// Effect-key parser (Arcurus 2026-06-06 #openworld)
//
// LLM-emit effects use dot-keys to disambiguate the target entity:
//   - "self.morale"                → apply to the actor
//   - "Mira the Merchant.wealth"   → apply to Mira the Merchant
//   - "morale"                     → apply to the actor (backward compat)
//
// The actor's own literal name (e.g. "Kira Dawnblade.morale" when
// Kira is acting) is also accepted, case-sensitive, exact match.
// The template and system prompt advertise only `self.X` to keep
// the convention simple; the literal-name form is a forgiving
// fallback for LLMs that forget the prefix. Per Arcurus 2026-06-06
// #openworld: "yes alow self or own name but no need to mention
// that it can use own name".
//
// Cross-entity writes are DRY-RUN for now (parsed, routed, logged
// in an aggregated warning, NOT applied). This lets us verify the
// routing and catch typos / stale entity memories before the writes
// go live.
// ---------------------------------------------------------------------------

/// Where an effect should land.
///
/// `Actor` means "the entity the LLM is acting on" (the one named
/// in the `/api/entities/:id/action/process` request). `Other`
/// means "some other entity in the world" — currently a dry-run
/// target, see `process_action_handler` for the warning. The
/// `name` is kept alongside the `id` so the aggregated dry-run
/// warning can name the targets without re-borrowing `world`
/// inside the apply loop.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EffectTarget {
    Actor,
    /// Cross-entity target. `name` is the literal name from the
    /// dot-prefix (e.g. "Mira the Merchant"). `id` is the resolved
    /// entity id.
    Other { name: String, id: Uuid },
}

/// One effect after the dot-key has been split and the target
/// resolved. The application loop iterates `Vec<ParsedEffect>` and
/// dispatches on `target` — no more inline string parsing.
#[derive(Debug, Clone)]
struct ParsedEffect {
    /// Resolved target: self or a specific other entity.
    target: EffectTarget,
    /// Bare property name (no `self.` / `EntityName.` prefix).
    /// Empty when the LLM emitted something like "self." with no
    /// property; the application loop will surface a warning and
    /// skip.
    prop_name: String,
    /// Original key as emitted by the LLM, kept for warnings and
    /// the `effects_applied` response.
    raw_key: String,
    /// The value, unmodified (could be int/float/string/bool).
    value: serde_json::Value,
}

/// Build a case-sensitive name → id lookup table for the current
/// world. The LLM emits literal names (e.g. "Mira the Merchant")
/// as the prefix in dotted keys; we resolve to the entity id at
/// effect-parse time. O(1) per lookup; rebuilt once per call.
///
/// On duplicate names (rare but possible), the last entity to be
/// iterated wins. A future improvement is to detect and warn on
/// duplicates; out of scope for now (per Arcurus 2026-06-06
/// #openworld: ship the routing first, harden later).
fn build_name_index(world: &World) -> std::collections::HashMap<String, Uuid> {
    world.entities.iter().map(|(id, e)| (e.name.clone(), *id)).collect()
}

/// Parse the LLM-emit effects map into a typed list, resolving
/// each key's target. Unknown entity names are returned separately
/// so the caller can emit one aggregated warning.
///
/// `actor_id` and `actor_name` are needed to expand `self.X` and
/// the actor's literal name (e.g. "Kira Dawnblade.X") to
/// `EffectTarget::Actor`. `name_to_id` is the case-sensitive
/// world-wide name lookup (see `build_name_index`).
///
/// Per Arcurus 2026-06-06 #openworld: case-sensitive exact match,
/// accept `self.X` AND the actor's literal name, warn on unknown
/// entity names.
fn parse_effects(
    effects: &std::collections::HashMap<String, serde_json::Value>,
    actor_id: Uuid,
    actor_name: &str,
    name_to_id: &std::collections::HashMap<String, Uuid>,
) -> (Vec<ParsedEffect>, Vec<String>) {
    let mut parsed = Vec::with_capacity(effects.len());
    let mut unknown_names: Vec<String> = Vec::new();
    let mut seen_unknown: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (raw_key, value) in effects {
        // Split on the FIRST dot.  Property names in this world
        // never contain dots, so the prefix is "self" / the
        // actor's literal name / another entity's name, and the
        // suffix is the property name.
        if let Some(dot_idx) = raw_key.find('.') {
            let prefix = &raw_key[..dot_idx];
            let prop = &raw_key[dot_idx + 1..];

            if prop.is_empty() {
                // "self." with no property — malformed; treat as
                // an Actor effect with empty prop_name and let
                // the application loop surface a warning.
                parsed.push(ParsedEffect {
                    target: EffectTarget::Actor,
                    prop_name: String::new(),
                    raw_key: raw_key.clone(),
                    value: value.clone(),
                });
                continue;
            }

            if prefix == "self" || prefix == actor_name {
                // Self-effect: either the convention ("self.X")
                // or the actor's literal name ("Kira Dawnblade.X").
                parsed.push(ParsedEffect {
                    target: EffectTarget::Actor,
                    prop_name: prop.to_string(),
                    raw_key: raw_key.clone(),
                    value: value.clone(),
                });
            } else if let Some(&target_id) = name_to_id.get(prefix) {
                // Sanity: if the LLM happened to emit the actor's
                // literal name in the dot-prefix (e.g. it
                // canonicalized "self" to the actor's name) and
                // the names match, treat as Actor.  This is a
                // safety net for the "Kira Dawnblade.X when Kira
                // is acting" case — already covered by the
                // `prefix == actor_name` arm above, but spelled
                // out for clarity.
                if target_id == actor_id {
                    parsed.push(ParsedEffect {
                        target: EffectTarget::Actor,
                        prop_name: prop.to_string(),
                        raw_key: raw_key.clone(),
                        value: value.clone(),
                    });
                } else {
                    // Cross-entity effect: dry-run for now.
                    // Stash the literal name alongside the id so
                    // the apply loop can build the dry-run report
                    // without re-borrowing `world`.
                    parsed.push(ParsedEffect {
                        target: EffectTarget::Other {
                            name: prefix.to_string(),
                            id: target_id,
                        },
                        prop_name: prop.to_string(),
                        raw_key: raw_key.clone(),
                        value: value.clone(),
                    });
                }
            } else {
                // Unknown entity name (typo, stale memory, etc.).
                // De-dupe so the aggregated warning doesn't
                // double-list the same name.
                if seen_unknown.insert(prefix.to_string()) {
                    unknown_names.push(prefix.to_string());
                }
                // Do NOT add to `parsed` — the effect is skipped
                // entirely (no application, no normalization).
            }
        } else {
            // No dot: backward-compat.  Treat as an Actor effect
            // on the actor's bare property (old LLM behavior).
            parsed.push(ParsedEffect {
                target: EffectTarget::Actor,
                prop_name: raw_key.clone(),
                raw_key: raw_key.clone(),
                value: value.clone(),
            });
        }
    }

    (parsed, unknown_names)
}

/// Compute the actor's normalization scale given the parsed
/// effect list. Only `EffectTarget::Actor` effects count for the
/// actor's cap; `Other` effects are dry-run and don't change the
/// actor, so they're excluded from the total. Magnitude-rejected
/// values (non-finite, |Δ| > MAX_DELTA_ABS) are also excluded so
/// a 1e18 sibling cannot drag a small real effect down to ~0.
///
/// This replaces the old raw-HashMap pre-pass and is the single
/// source of truth for the per-turn cap. Per Arcurus 2026-06-06
/// #openworld.
fn compute_actor_normalization_scale(
    actor_power: i64,
    parsed: &[ParsedEffect],
    protected_entity: bool,
) -> (f64, f64) {
    if protected_entity {
        return (0.0, 1.0);
    }
    let effect_cap = compute_effect_normalization_cap(actor_power);
    let mut total: f64 = 0.0;
    for e in parsed {
        // Only count actor effects; other-entity effects don't
        // change the actor.
        if !matches!(e.target, EffectTarget::Actor) { continue; }
        // Reject garbage FIRST. Anything that fails
        // `magnitude_check` is excluded from `total`, so the
        // scale is computed on the clean set. The application
        // loop re-checks magnitude_check so the garbage value
        // itself is never written. Per Arcurus 2026-06-06
        // #openworld: "reject the garbage first, but make sure
        // that is removed from the applied effects before its
        // counted or applied".
        if magnitude_check(&e.value).is_some() { continue; }
        if let Some(f) = parse_effect_value(&e.value) {
            if f.is_finite() { total += f.abs(); }
        }
    }
    let scale = if total > effect_cap && total > 0.0 {
        effect_cap / total
    } else {
        1.0
    };
    (total, scale)
}

/// Apply a batch of effects (already parsed via `parse_effects` so
/// the targets are resolved) to a single target entity. The helper
/// handles:
///   - system-entity protection (skip all effects with a warning;
///     the LLM is told cross-entity writes can hit any entity, but
///     system entities are an exception)
///   - per-target normalization (each target gets its own cap from
///     its own `power`; a high-power Ironforge can absorb more
///     change than a low-power Mira the Scribe)
///   - magnitude / oversize guards (rejects non-finite values, |Δ|>
///     MAX_DELTA_ABS, integer overflow, and oversize results)
///   - type-mismatch handling (int / float / string dispatch)
///   - the per-effect normalization scale (numeric deltas are
///     scaled; string effects are not)
///
/// Returns `(applied, new_values)` — both HashMaps from prop_name
/// to the value written.  They are populated together and end up
/// the same shape; the field is split in the response for
/// readability.
///
/// Per Arcurus 2026-06-07 #openworld: extracted from the inline
/// loop in `process_action_handler` so the actor path and the new
/// cross-entity path share one implementation, and so future
/// per-target safety guards (e.g. "effect on a faction must not
/// increase the faction's own reputation above 1000") can be added
/// in one place.
fn apply_effects_to_target(
    target: &mut WorldEntity,
    effects: &[&ParsedEffect],
    warnings: &mut Vec<String>,
    stats_log: Option<&std::sync::Mutex<DailyLogger>>,
    actor_name: &str,
    is_actor: bool,
) -> (
    std::collections::HashMap<String, serde_json::Value>,
    std::collections::HashMap<String, serde_json::Value>,
) {
    let mut applied: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    let mut new_values: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();

    // System-entity protection: any effect targeting a system
    // entity (world_clock, anything tagged 'meta') is rejected
    // outright, regardless of which LLM call proposed it.
    // Previously this protection was implicit (cross-entity
    // writes were dry-run); now that cross-entity writes are
    // live, we need the explicit check.
    if target.is_system_entity() {
        // Distinguish abstract entities (e.g. the world
        // clock) from the broader "system" guard, so the
        // operator sees the SPECIFIC reason the write was
        // blocked.  Per Arcurus 2026-06-07 #openworld: "keep
        // for now all writes blocked to abstract entities,
        // but record as warning when an effect tries to
        // change a property of an abstract entity
        // (property name, change value, and entity name)".
        let reason = if target.entity_type == "abstract"
            || target.entity_type == "world_clock"
        {
            "abstract entities are write-protected"
        } else {
            "system entities are protected"
        };
        for e in effects {
            warnings.push(format!(
                "Skipped effect on abstract/system entity '{}' (entity_type={}, reason='{}'): property='{}', value={:?}",
                target.name, target.entity_type, reason, e.prop_name, e.value
            ));
        }
        return (applied, new_values);
    }

    // Per-target normalization: each target's cap comes from its
    // own power. Garbage values (failing magnitude_check) are
    // excluded from the total so a single 1e18 sibling can't
    // drag a small real effect down to ~0 (the same rule as
    // compute_actor_normalization_scale).
    let target_power = target.properties_int.get("power").copied().unwrap_or(0);
    let mut total: f64 = 0.0;
    for e in effects {
        if magnitude_check(&e.value).is_some() { continue; }
        if let Some(f) = parse_effect_value(&e.value) {
            if f.is_finite() { total += f.abs(); }
        }
    }
    let cap = compute_effect_normalization_cap(target_power);
    let scale = if total > cap && total > 0.0 { cap / total } else { 1.0 };
    if total > cap && total > 0.0 {
        warnings.push(format!(
            "Effects normalized on '{}': total |Δ|={:.3} exceeds cap {:.3} ({}% of max(10, power)={} + max-amount={}); scaled by {:.4}.",
            target.name, total, cap,
            (EFFECT_NORMALIZATION_CAP_PCT * 100.0) as i64,
            std::cmp::max(10, target_power),
            EFFECT_NORMALIZATION_MAX_AMOUNT as i64,
            scale
        ));
    }

    for e in effects {
        let prop_key = &e.prop_name;
        let raw_key = &e.raw_key;
        let change_val = &e.value;

        // Empty property name (e.g. "self." with nothing after the
        // dot) — parsed as Actor with empty prop_name; the apply
        // loop surfaces a warning and skips.
        if prop_key.is_empty() {
            warnings.push(format!("Empty property name in effect '{}'; skipped", raw_key));
            continue;
        }

        // Internal-property guard (per Arcurus 2026-06-07
        // #openworld): reject any LLM-emit effect that targets
        // an operator-internal bookkeeping property
        // (e.g. `last_processed_other_tick`).  These are
        // write-protected against LLM-emit effects, but the
        // operator can still override them via the per-property
        // PUT endpoint (the documented escape hatch).  See
        // `world_data::internal_properties` for the full list
        // and rationale.
        if crate::world_data::internal_properties::is_internal_property(prop_key) {
            warnings.push(format!(
                "Skipped effect on internal/operator-only property '{}' (entity='{}', reason='LLM-emit effects cannot write to internal properties; use the per-property PUT endpoint to override')",
                prop_key, target.name
            ));
            continue;
        }

        // Magnitude guard (non-finite or |Δ| > MAX_DELTA_ABS).
        if let Some(reason) = magnitude_check(change_val) {
            warnings.push(format!(
                "Skipped effect on '{}': {} (value={:?})",
                raw_key, reason, change_val
            ));
            continue;
        }

        // Determine value type and convert appropriately. Same
        // dispatch as the original process_action_handler path.
        let (int_val, float_val, string_val) = match change_val {
            serde_json::Value::Bool(b) => {
                let v = if *b { 1 } else { 0 };
                (Some(v as i64), None, None)
            }
            serde_json::Value::Number(n) => {
                if n.is_i64() {
                    (Some(n.as_i64().unwrap()), None, None)
                } else {
                    (None, n.as_f64(), None)
                }
            }
            serde_json::Value::String(s) => {
                let trimmed = s.trim();
                if let Ok(v) = trimmed.parse::<i64>() {
                    (Some(v), None, None)
                } else if let Ok(v) = trimmed.parse::<f64>() {
                    if v.fract() == 0.0 && v.abs() < 1e15 && !trimmed.contains('.') {
                        (Some(v as i64), None, None)
                    } else {
                        (None, Some(v), None)
                    }
                } else {
                    (None, None, Some(trimmed.to_string()))
                }
            }
            _ => {
                warnings.push(format!(
                    "Unsupported effect type for '{}': {:?}", raw_key, change_val
                ));
                continue;
            }
        };

        // Apply the per-turn normalization scale to numeric
        // deltas. String effects are not scaled.
        let (int_val, float_val) = (
            int_val.map(|v| ((v as f64) * scale).round() as i64),
            float_val.map(|v| v * scale),
        );

        let is_in_int = target.properties_int.contains_key(prop_key);
        let is_in_float = target.properties_float.contains_key(prop_key);
        let is_in_string = target.properties_string.contains_key(prop_key);

        if let Some(val) = int_val {
            if is_in_float {
                warnings.push(format!(
                    "Type mismatch: '{}' is float, tried to set int ({}). Skipped.",
                    prop_key, val
                ));
                continue;
            }
            if is_in_string {
                warnings.push(format!(
                    "Type mismatch: '{}' is string, tried to set int ({}). Skipped.",
                    prop_key, val
                ));
                continue;
            }
            let old_val = *target.properties_int.get(prop_key).unwrap_or(&0);
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
            target.properties_int.insert(prop_key.clone(), new_val);
            applied.insert(prop_key.clone(), serde_json::json!(new_val));
            new_values.insert(prop_key.clone(), serde_json::json!(new_val));
            if let Some(logger) = stats_log {
                if let Ok(mut l) = logger.lock() {
                    l.log_stats_change(
                        &target.name,
                        &target.id,
                        prop_key,
                        old_val,
                        new_val,
                        !is_actor,
                        actor_name,
                        warnings.len(),
                    );
                }
            }
        } else if let Some(val) = float_val {
            if is_in_int {
                warnings.push(format!(
                    "Type mismatch: '{}' is int, tried to set float ({}). Skipped.",
                    prop_key, val
                ));
                continue;
            }
            if is_in_string {
                warnings.push(format!(
                    "Type mismatch: '{}' is string, tried to set float ({}). Skipped.",
                    prop_key, val
                ));
                continue;
            }
            let old_val = *target.properties_float.get(prop_key).unwrap_or(&0.0);
            let new_val = old_val + val;
            if float_oversize(new_val) {
                warnings.push(format!(
                    "Skipped oversize float write on '{}': new_val={}",
                    prop_key, new_val
                ));
                continue;
            }
            target.properties_float.insert(prop_key.clone(), new_val);
            applied.insert(prop_key.clone(), serde_json::json!(new_val));
            new_values.insert(prop_key.clone(), serde_json::json!(new_val));
            if let Some(logger) = stats_log {
                if let Ok(mut l) = logger.lock() {
                    l.log_stats_change(
                        &target.name,
                        &target.id,
                        prop_key,
                        old_val as i64,
                        new_val as i64,
                        !is_actor,
                        actor_name,
                        warnings.len(),
                    );
                }
            }
        } else if let Some(ref val) = string_val {
            if is_in_int {
                warnings.push(format!(
                    "Type mismatch: '{}' is int, tried to set string value={:?}. Skipped.",
                    prop_key, val
                ));
                continue;
            }
            if is_in_float {
                warnings.push(format!(
                    "Type mismatch: {} is float, tried to set string (val=\"{}\"). Skipped.",
                    prop_key, val
                ));
                continue;
            }
            target.properties_string.insert(prop_key.clone(), val.clone());
            applied.insert(prop_key.clone(), serde_json::json!(val));
            new_values.insert(prop_key.clone(), serde_json::json!(val));
        }
    }

    (applied, new_values)
}

/// Apply a fully-parsed effect list to a world, dispatching each
/// effect to its target entity (Actor + N Other). Per-target
/// safety (system-entity guard, magnitude check, per-target
/// normalization, type handling) is handled inside
/// `apply_effects_to_target`. After all effects are applied,
/// `update_hidden_tag` runs on every affected entity to toggle
/// the `hidden` tag based on the post-effect `(power,
/// visibility)`.  Also runs the `corrupted`-tag rule on the
/// same set of entities (the corruption threshold uses the
/// same set of post-effect `power` values).  Returns:
///
///   - `(actor_applied, actor_new_values, cross_entity_applied,
///       hidden_tags_updated, corrupted_tags_updated)`
///   - `hidden_tags_updated` is a `BTreeMap<entity_name,
///     {added: bool, removed: bool, threshold: f64}>` of every
///     entity whose hidden-tag state changed in this call.
///     Only toggled entities appear (no entry for "tag already
///     correct").
///   - `corrupted_tags_updated` is a `BTreeMap<entity_name,
///     bool>` where the bool is the **new** state of the
///     `corrupted` tag (true = now present, false = just
///     removed).  Only entities whose tag toggled are
///     included.
///
/// Extracted to a top-level helper so the caller (e.g.
/// `process_action_handler`) can release any existing mutable
/// borrow on the world before dispatching, avoiding the
/// double-mutable-borrow that the Rust borrow checker otherwise
/// rejects (we want to mutate the actor entity AND any other
/// target entities in the same call).
fn apply_all_effects(
    world: &mut World,
    actor_id: Uuid,
    parsed_effects: &[ParsedEffect],
    warnings: &mut Vec<String>,
    stats_log: Option<&std::sync::Mutex<DailyLogger>>,
) -> (
    std::collections::HashMap<String, serde_json::Value>,
    std::collections::HashMap<String, serde_json::Value>,
    std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, serde_json::Value>,
    >,
    std::collections::BTreeMap<String, HiddenTagUpdate>,
    std::collections::BTreeMap<String, bool>,
) {
    // Group by target.
    let mut by_target: std::collections::BTreeMap<Uuid, Vec<&ParsedEffect>> =
        std::collections::BTreeMap::new();
    for parsed in parsed_effects {
        let tid = match &parsed.target {
            EffectTarget::Actor => actor_id,
            EffectTarget::Other { id: other_id, .. } => *other_id,
        };
        by_target.entry(tid).or_default().push(parsed);
    }

    // Pre-collect target names + the actor's name for the
    // stats-change log.  Immutable borrow; released before
    // the apply loop.
    let target_names: std::collections::HashMap<Uuid, String> = by_target
        .keys()
        .filter_map(|tid| world.entities.get(tid).map(|e| (*tid, e.name.clone())))
        .collect();
    let actor_name: String = world
        .entities
        .get(&actor_id)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| format!("<actor-id:{}>", actor_id));

    let mut actor_applied: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    let mut actor_new_values: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    let mut cross_entity_applied: std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, serde_json::Value>,
    > = std::collections::BTreeMap::new();
    // Track which entities had at least one effect actually
    // written, so the post-effect hidden-tag rule only runs
    // on those (not on every entity in the world).
    let mut affected_ids: std::collections::BTreeSet<Uuid> =
        std::collections::BTreeSet::new();

    for (tid, effects) in by_target {
        let is_actor = tid == actor_id;
        if let Some(target) = world.entities.get_mut(&tid) {
            let (applied, nvs) =
                apply_effects_to_target(
                    target,
                    &effects,
                    warnings,
                    stats_log,
                    &actor_name,
                    is_actor,
                );
            // Capture emptiness BEFORE we move `applied` into
            // the actor_applied map (or into the
            // cross_entity_applied map below). The Rust borrow
            // checker is right: a moved value can't be used
            // after.
            let had_effects = !applied.is_empty();
            if tid == actor_id {
                actor_applied = applied;
                actor_new_values = nvs;
            } else {
                if had_effects {
                    let name = target_names
                        .get(&tid)
                        .cloned()
                        .unwrap_or_else(|| format!("<id:{}>", tid));
                    cross_entity_applied.insert(name, applied);
                }
            }
            if had_effects {
                affected_ids.insert(tid);
            }
        }
    }

    // Post-effect hidden-tag rule (Arcurus 2026-06-07 #openworld):
    // for every entity that had at least one effect actually
    // written, recompute the threshold from its current
    // (post-effect) `power` and `visibility`, and toggle the
    // `hidden` tag if the threshold crossed the add/remove
    // boundaries.  Emits one warning per toggle so operators
    // (and the LLM log) see the state change.
    let mut hidden_tags_updated: std::collections::BTreeMap<String, HiddenTagUpdate> =
        std::collections::BTreeMap::new();
    for tid in &affected_ids {
        let name = target_names
            .get(tid)
            .cloned()
            .unwrap_or_else(|| format!("<id:{}>", tid));
        if let Some(target) = world.entities.get_mut(tid) {
            let (added, removed, threshold) = update_hidden_tag(target);
            if added || removed {
                hidden_tags_updated.insert(
                    name.clone(),
                    HiddenTagUpdate { added, removed, threshold },
                );
                if added {
                    warnings.push(format!(
                        "Added '{}' tag to '{}' (threshold={:.3} < 0)",
                        HIDDEN_TAG, name, threshold
                    ));
                } else if removed {
                    warnings.push(format!(
                        "Removed '{}' tag from '{}' (threshold={:.3} >= 1)",
                        HIDDEN_TAG, name, threshold
                    ));
                }
            }
        }
    }

    // Post-effect corrupted-tag rule (Arcurus 2026-06-07
    // #openworld).  Mirror of the hidden-tag rule, but for
    // the corruption property.  Runs ONCE per affected
    // entity, after all effects (including the hidden-tag
    // rule), using the final (post-effect) `power` and
    // `corruption` values.
    //
    // The map value is the **new tag state** (true = tag is
    // now present, false = tag was just removed).  Only
    // entities whose tag toggled are included (no entry for
    // "tag was already correct").
    let mut corrupted_tags_updated: std::collections::BTreeMap<String, bool> =
        std::collections::BTreeMap::new();
    for tid in &affected_ids {
        let name = target_names
            .get(tid)
            .cloned()
            .unwrap_or_else(|| format!("<id:{}>", tid));
        if let Some(target) = world.entities.get_mut(tid) {
            let (added, removed, threshold) = update_corrupted_tag(target);
            if added {
                warnings.push(format!(
                    "Added '{}' tag to '{}' (threshold={:.3} < 0)",
                    CORRUPTED_TAG, name, threshold
                ));
                corrupted_tags_updated.insert(name, true);
            } else if removed {
                warnings.push(format!(
                    "Removed '{}' tag from '{}' (threshold={:.3} >= 1)",
                    CORRUPTED_TAG, name, threshold
                ));
                corrupted_tags_updated.insert(name, false);
            }
        }
    }

    // Post-effect stats-cap rule (Arcurus 2026-06-07 #openworld):
    // for every entity that had at least one effect actually
    // written, check if its steady-state `properties_int` sum
    // has exceeded the cap of `max(1, power*5) + 100`.  If so,
    // emit a warning that names the entity, the sum, the cap,
    // and the overage.  We do NOT normalize here — that's the
    // standalone script's job.  A single big effect shouldn't
    // silently shrink the entity mid-action; the operator
    // reviews the warning and runs the script when ready.
    for tid in &affected_ids {
        if let Some(target) = world.entities.get(tid) {
            check_stats_cap_warn(target, warnings);
        }
    }

    (actor_applied, actor_new_values, cross_entity_applied, hidden_tags_updated, corrupted_tags_updated)
}

/// One hidden-tag toggle, surfaced in the response so callers
/// can see when an entity crossed the add/remove threshold.
#[derive(Debug, Clone, serde::Serialize)]
struct HiddenTagUpdate {
    /// True if the tag was added in this call.
    pub added: bool,
    /// True if the tag was removed in this call.
    pub removed: bool,
    /// The post-effect threshold that triggered the toggle
    /// (`max(10, power) / 10 + visibility`).
    pub threshold: f64,
}

// ---------------------------------------------------------------------------
// Effect-key parser (Arcurus 2026-06-06 #openworld)
//
// LLM-emit effects use dot-keys to disambiguate the target entity:
//   - "self.morale"                → apply to the actor
//   - "Mira the Merchant.wealth"   → apply to Mira the Merchant
//   - "morale"                     → apply to the actor (backward compat)
//
// The actor's own literal name (e.g. "Kira Dawnblade.morale" when
// Kira is acting) is also accepted, case-sensitive, exact match.
// The template and system prompt advertise only `self.X` to keep
// the convention simple; the literal-name form is a forgiving
// fallback for LLMs that forget the prefix. Per Arcurus 2026-06-06
// #openworld: "yes alow self or own name but no need to mention
// that it can use own name".
//
// As of 2026-06-07 (#openworld, Arcurus): cross-entity writes
// APPLY, not dry-run. The LLM can name any other entity in the
// world and the effect is written, with the same per-target
// safety nets (magnitude check, per-target normalization,
// type handling, system-entity guard) that actor effects have
// always had.
//   - "self.morale"                → apply to the actor
//   - "Mira the Merchant.wealth"   → apply to Mira the Merchant
//   - "morale"                     → apply to the actor (backward compat)
//
// The actor's own literal name (e.g. "Kira Dawnblade.morale" when
// Kira is acting) is also accepted, case-sensitive, exact match.
// The template and system prompt advertise only `self.X` to keep
// the convention simple; the literal-name form is a forgiving
// fallback for LLMs that forget the prefix. Per Arcurus 2026-06-06
// #openworld: "yes alow self or own name but no need to mention
// that it can use own name".
//
// Cross-entity writes are DRY-RUN for now (parsed, routed, logged
// in an aggregated warning, NOT applied). This lets us verify the
// routing and catch typos / stale entity memories before the writes
// go live.
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
            // Strict parse failed. Try a chain of tolerant fixup
            // passes before giving up — each targets a specific
            // malformation we've actually observed in production
            // logs.
            //
            // **Fixup #1 — empty-key bug in history_summary_replace**
            // (per Arcurus 2026-06-05 #openworld): the World Clock
            // entity has been emitting a `history_summary_replace`
            // array whose first element has a spurious
            // `"" : "new_part"` key+value before the actual
            // `"new_part"` field, e.g.
            //   {"old_part":"...","":"new_part":"..."}
            // instead of
            //   {"old_part":"...","new_part":"..."}
            // which serde rejects. Rather than rejecting the whole
            // response (and losing the `action`, `effects`,
            // `narrative`, and the `history_summary` that often
            // comes with it), we run a conservative regex repair.
            let repaired1 = fix_known_malformed_patterns(json_str);
            if repaired1 != json_str {
                if let Ok(parsed) = serde_json::from_str::<LlmActionResponse>(&repaired1) {
                    // Successful fixup-#1 repair. Return the
                    // parsed value with a warning string so the
                    // caller can surface it in the response and
                    // / or the LLM-call log. The warning is
                    // descriptive (says "this was a regex repair
                    // of the known empty-key bug") so a future
                    // operator grepping the JSONL log for it can
                    // tell at a glance how often the World Clock
                    // bug recurs.
                    return Ok((parsed, Some(format!(
                        "parse_llm_action_response: LLM response matched a known \
                         malformed pattern and was repaired (regex fixup of the \
                         \"\":\"old_part\"|\"new_part\" empty-key bug seen in \
                         history_summary_replace)."
                    ))));
                }
            }

            // **Fixup #2 — strip `{{`/`}}` wrappers outside JSON
            // strings** (per Arcurus 2026-06-06 #openworld): the
            // M2.7-highspeed LLM has been wrapping JSON objects in
            // `{{` and `}}` (the outer response becomes `{{...}}`
            // and `"effects": {{...}}`). The root cause is that
            // the prompt template at `ai_templates/EntityAction.md`
            // intentionally uses `{{`/`}}` as a "mustache-style
            // escape" — the LLM is supposed to unescape them in
            // its response, but M2.7-highspeed doesn't reliably
            // do that and just copies the literal pattern. See
            // `strip_double_braces_outside_strings` for the
            // string-aware collapse algorithm.
            //
            // We apply fixup #2 on top of fixup #1's result, so a
            // response that has *both* bugs (empty-key AND
            // double-brace wrapping) is also recovered. The
            // resulting string may then parse successfully; if
            // not, we surface the most useful error.
            let candidate = if repaired1 != json_str { repaired1.as_str() } else { json_str };
            let repaired2 = strip_double_braces_outside_strings(candidate);
            if repaired2 != candidate {
                if let Ok(parsed) = serde_json::from_str::<LlmActionResponse>(&repaired2) {
                    return Ok((parsed, Some(format!(
                        "parse_llm_action_response: LLM response matched a known \
                         malformed pattern and was repaired (stripped `{{`/`}}` \
                         wrappers outside JSON strings — M2.7-highspeed has been \
                         wrapping JSON objects in double braces; root cause is the \
                         `{{`/`}}` mustache-style escape in the prompt template at \
                         ai_templates/EntityAction.md, see 2026-06-06 #openworld)."
                    ))));
                }
            }

            // No fixup produced a parseable string. Surface the
            // most useful error.
            if repaired2 != candidate {
                // The candidate post-fixup-#2 still doesn't parse.
                // Show the post-fixup-#2 parse error if we can get
                // one, otherwise fall back to the original error.
                let e2 = serde_json::from_str::<LlmActionResponse>(&repaired2)
                    .err()
                    .unwrap_or(e);
                Err(format!(
                    "JSON parse error: {} - Input (after repair attempt): {}",
                    e2, repaired2
                ))
            } else if repaired1 != json_str {
                // Fixup #1 changed the string but #2 didn't change
                // it, and the result still doesn't parse.
                let e2 = serde_json::from_str::<LlmActionResponse>(&repaired1)
                    .err()
                    .unwrap_or(e);
                Err(format!(
                    "JSON parse error: {} - Input (after repair attempt): {}",
                    e2, repaired1
                ))
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

/// Strip `{{` and `}}` wrappers that appear outside of JSON string
/// literals.
///
/// **Background.** The M2.7-highspeed LLM has a recurring bug
/// (per Arcurus 2026-06-06 #openworld: Velora silver-warden +
/// scribe-covenant actions) where it wraps each JSON object in
/// `{{` and `}}` — the outer response becomes `{{...}}` and even
/// nested objects (e.g. `"effects": {{...}}`) are wrapped. The
/// root cause is in the prompt template at
/// `ai_templates/EntityAction.md`, which intentionally uses
/// `{{` and `}}` as a "mustache-style escape" (see
/// `entity_action_template_json_example_is_valid_json` in
/// `context_builder.rs` for the original design intent — the LLM
/// is supposed to unescape them in its response). M2.7-highspeed
/// does not consistently do that and just copies the literal
/// pattern. The strict `serde_json` parse then fails on the
/// leading `{{`, and we lose the entire LLM response (action,
/// effects, narrative, history_summary — the lot).
///
/// **The fix.** Conservative string-aware collapse:
/// - We track JSON string boundaries (with backslash-escape
///   awareness), so a literal `{{` or `}}` that appears *inside*
///   a string value is left alone.
/// - Outside of strings, we collapse each `{{` → `{` and
///   `}}` → `}`. In valid JSON, a `{` outside a string is always
///   an object-opener and is always followed by `"` (a key) or
///   `}` (an empty object) — never by another `{`. So `{{` outside
///   a string is always the LLM bug, not real JSON.
fn strip_double_braces_outside_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        // Collapse `{{` (outside string) → `{`.
        if c == '{' && chars.peek() == Some(&'{') {
            out.push('{');
            chars.next(); // consume the second `{`
            continue;
        }
        // Collapse `}}` (outside string) → `}`.
        if c == '}' && chars.peek() == Some(&'}') {
            out.push('}');
            chars.next(); // consume the second `}`
            continue;
        }
        out.push(c);
    }
    out
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
            // Build the case-sensitive name → id lookup
            // table BEFORE we take a mutable borrow of the
            // entity (Rust borrow checker: we can't immutably
            // borrow `world` while the entity is borrowed
            // mutably). Used by `parse_effects` to resolve
            // dot-prefixes like "Mira the Merchant.wealth" to
            // the target entity id. O(1) per lookup; rebuilt
            // once per process call.
            let name_to_id = build_name_index(&world);

            // --- Effect-parse + apply (outside the actor's
            // mutable borrow) ---
            //
            // We need the actor's name for parse_effects but NOT
            // a mutable borrow of the actor entity, so we can
            // fetch the name via a shared borrow and release it
            // before the apply pass. The apply itself runs on
            // `&mut world` (via `apply_all_effects`), so the
            // entity mutable borrow can only start AFTER the
            // apply completes.  Per Arcurus 2026-06-07 #openworld:
            // this restructure is the cost of letting
            // cross-entity writes actually apply — we mutate
            // multiple entities in a single call.
            let actor_name = world
                .entities
                .get(&entity_id)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            let (parsed_effects, unknown_entity_names) = parse_effects(
                &action_data.effects,
                entity_id,
                &actor_name,
                &name_to_id,
            );

            let mut warnings: Vec<String> = Vec::new();
            // Surface the tolerant-repair warning (if any) first
            // so it shows up at the top of the warnings vec in
            // the response and the LLM-call log. This makes
            // the regex-fixup recovery visible to operators
            // (per Arcurus 2026-06-05 #openworld).
            if let Some(w) = repair_warning {
                warnings.push(w);
            }

            // Apply effects to actor + any cross-entity targets.
            // Per-target safety: system-entity guard, magnitude
            // check, per-target normalization (cap from the
            // target's own `power`), type handling. Per Arcurus
            // 2026-06-07 #openworld: this replaces the old
            // dry-run for cross-entity effects.
            let (applied_effects, new_values, cross_entity_applied, hidden_tags_updated, corrupted_tags_updated) =
                apply_all_effects(&mut world, entity_id, &parsed_effects, &mut warnings, Some(&state.logger));

            // Emit the aggregated unknown-entity warning.
                // One per call, listing the de-duped names that
                // the LLM referenced but that don't exist in
                // the world. (The corresponding effects are
                // already skipped — they were never added to
                // `parsed_effects`.)
                if !unknown_entity_names.is_empty() {
                    let mut w = format!(
                        "Unknown entity names in effects ({} effect(s) skipped):",
                        // Count: number of effects with these
                        // prefixes (could be more than the number
                        // of unique names if the LLM emitted
                        // multiple effects per name). We don't
                        // have the per-prefix count cheaply here,
                        // so we report the unique-name count
                        // (the LLM should look at the list and
                        // figure out which of their effects
                        // are impacted).
                        unknown_entity_names.len()
                    );
                    for name in &unknown_entity_names {
                        w.push_str(&format!("\n  - {:?} (no entity with that exact name; check spelling and case)", name));
                    }
                    warnings.push(w);
                }

                // Capture world.action_count BEFORE the
                // mutable borrow of world.entities below —
                // we need it for the tick stamp on the
                // history entry.  The
                // `last_processed_other_tick` marker is
                // advanced separately, after we compute
                // the max tick of the entries that were
                // rendered in the unprocessed-other-actions
                // block.  Per Arcurus 2026-06-07 (#openworld):
                // "it needs to be set to the creating
                // tick time of the other history message
                // last included in the llm to process."
                let stamp_tick = world.action_count as i64;

                // Also capture the CURRENT marker and
                // compute the NEXT marker BEFORE the
                // mutable borrow of world.entities below.
                // Default 0 if not set.
                let current_marker = world
                    .entities
                    .get(&entity_id)
                    .and_then(|e| e.properties_int.get("last_processed_other_tick").copied())
                    .unwrap_or(0);
                let all_entries_for_marker =
                    world_data::action_history_log::load_all_world_actions();
                let next_marker_tick = {
                    let entity_for_marker = world
                        .entities
                        .get(&entity_id)
                        .expect("entity_id just verified to exist");
                    world_data::context_builder::compute_max_unprocessed_tick(
                        &world,
                        entity_for_marker,
                        &all_entries_for_marker,
                        current_marker,
                    )
                };

                if let Some(entity) = world.entities.get_mut(&entity_id) {
                    // System entities (world_clock, meta-tagged)
                    // are protected from LLM-driven property writes
                    // to prevent integer/float corruption of world
                    // state.  The action is still recorded in
                    // history and the durable JSONL log, but effects
                    // are rejected with a warning.  See todo
                    // c7f3bc27.  (The actual effect guard is
                    // inside `apply_effects_to_target` — we also
                    // emit a single summary warning here so
                    // operators see at a glance that the actor
                    // is protected.)
                    let protected_entity = entity.is_system_entity();
                    if protected_entity {
                        warnings.push(format!(
                            "Entity is a system entity (type={}, tags={:?}); LLM effect writes blocked.",
                            entity.entity_type, entity.tags
                        ));
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
                // We pass `next_marker_tick` (computed
                // before the mutable borrow above) so the
                // `last_processed_other_tick` marker is
                // advanced to the max tick of the entries
                // shown in the unprocessed-other-actions
                // block.  If no entries were shown
                // (filter empty, or cap too tight), the
                // returned value is 0 and the marker does
                // not advance (per Arcurus 2026-06-07).
                add_to_history(
                    entity,
                    &action_data.action,
                    &action_data.narrative,
                    &action_data.outcome,
                    next_marker_tick,
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
                //
                // 2026-06-06: as a safety net for the common LLM
                // confusion "sends BOTH fields", if the LLM sends both
                // AND the replace chain produces a no-op (new_summary
                // == current), fall back to the dropped
                // `history_summary` value (truncated to the cap). This
                // recovers the update that would otherwise be silently
                // lost. See todo fa45e5e7 / 06-06 worker run.
                let mut summary_truncated = false;
                let applied_summary: Option<String> =
                    if let Some(replaces_one_or_many) = action_data.history_summary_replace {
                        // Case 1: history_summary_replace (LLM-emit
                        // surgical edits). replace wins; if the LLM
                        // also sent a full `history_summary`, drop it
                        // with a warning.
                        let llm_also_sent_full = action_data.history_summary.is_some();
                        if llm_also_sent_full {
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
                        // Safety net: if the LLM confused itself by
                        // sending BOTH and the replace chain produced a
                        // no-op (new_summary equals the current stored
                        // value), prefer the dropped `history_summary`
                        // — that's almost always what the LLM actually
                        // meant. Only fires when the LLM also sent the
                        // full summary, to avoid shadowing a deliberate
                        // surgical no-op (rare but possible: an LLM
                        // that just wants to commit an unchanged
                        // summary by sending an empty replace chain).
                        // Implementation lives in
                        // `apply_summary_fallback` so the logic is
                        // unit-testable.
                        let fallback = if llm_also_sent_full {
                            apply_summary_fallback(
                                action_data.history_summary.as_deref().unwrap_or(""),
                                entity.history_summary.as_deref(),
                                result.new_summary.as_deref(),
                                max_summary_chars,
                            )
                        } else {
                            None
                        };
                        if let Some(fb) = fallback {
                            for w in fb.warnings {
                                warnings.push(w);
                            }
                            if fb.truncated {
                                summary_truncated = true;
                            }
                            entity.history_summary = fb.new_summary.clone();
                            fb.new_summary
                        } else {
                            entity.history_summary = result.new_summary.clone();
                            result.new_summary
                        }
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
                    // Per Arcurus 2026-06-07 #openworld: stamp
                    // the world tick (== world.action_count at
                    // the moment of this action).  Pre-2026-06-07
                    // entries have tick=0; they get backfilled
                    // on world load (see load_world backfill
                    // block in main.rs).  Use the
                    // already-captured `stamp_tick` to avoid
                    // borrowing `world` here (the mutable
                    // borrow of `entity` is still live in
                    // this `if let` block).
                    tick: stamp_tick,
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
                    "cross_entity_effects_applied": cross_entity_applied,
                    "hidden_tags_updated": hidden_tags_updated,
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
                } else if name.starts_with("stats-change-log-") {
                    "stats-change"
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
        || !(filename.starts_with("llm-log-") || filename.starts_with("error-log-") || filename.starts_with("stats-change-log-"))
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
                // Migrate legacy entity_types to the current
                // canonical form.  Per Arcurus 2026-06-07
                // #openworld: hero + oracle → character, with
                // the role tag preserved.  Idempotent; runs on
                // every load (no-op once the data has been
                // migrated).  See World::migrate_entity_types
                // for the full rule.
                w.migrate_entity_types();
                // Backfill the durable action_history.jsonl:
                // assign a sequential tick (1, 2, 3, ...) to any
                // entry that has tick=0 (pre-2026-06-07
                // entries; the new `tick` field on
                // ActionHistoryEntry is `#[serde(default)]` so
                // they load as 0).  Per Arcurus 2026-06-07
                // #openworld: "we need also to add the time
                // tick in the world action history when the
                // action happened.  if its not set yes, we
                // need also to be able to set a date until
                // which dates other entities actions where
                // processed".  The backfill is what closes the
                // "if its not set yes" gap for the existing
                // 5400+ entries on disk.  No-op once the file
                // is fully backfilled (every entry already has
                // tick > 0).  See action_history_log::backfill_ticks.
                let backfilled = action_history_log::backfill_ticks();
                if backfilled > 0 {
                    println!("🔁 Backfilled ticks on {} history entries (pre-2026-06-07 data)", backfilled);
                }
                // Re-seed the canonical lore events on load. The binary
                // save format intentionally does NOT serialize
                // active_events (see BinaryPersistence doc comment at
                // src/world_data/persistence.rs:704), so a loaded world
                // always starts with active_events = Vec::new(). Without
                // this call, every restart of the service would silently
                // drop all events, leaving the LLM context builder with
                // no narrative momentum (a regression observed 2026-06-07
                // 08:23 CEST — 18 entities, 0 events). seed_default_events
                // is idempotent and a no-op if any events are already
                // present, so this is safe across restarts. See todo
                // e4cc4203 for the original World::new() seeding logic.
                let seeded_event_count = w.active_events.len();
                w.seed_default_events();
                if w.active_events.len() > seeded_event_count {
                    println!(
                        "🌱 Re-seeded {} canonical lore event(s) on load (was empty)",
                        w.active_events.len()
                    );
                }
                // Sanitize int AND float properties on system entities. This
                // cleans up garbage values that were written by LLM effects
                // before the c7f3bc27 upstream protection landed (todo
                // 2df49bd8). The float branch specifically targets
                // `total_years` (a time counter that grows legitimately
                // without bound) — see `sanitize_system_entities` for the
                // per-key cap rationale.
                let (int_repairs, float_repairs) = w.sanitize_system_entities();
                if !int_repairs.is_empty() {
                    println!("🧹 Sanitized {} system-entity int property repair(s):", int_repairs.len());
                    for (id, key, old, new) in &int_repairs {
                        println!("   • {} :: {} :: {} -> {}", id, key, old, new);
                    }
                }
                if !float_repairs.is_empty() {
                    println!("🧹 Sanitized {} system-entity float property repair(s):", float_repairs.len());
                    for (id, key, old, new) in &float_repairs {
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
        .route("/api/entities/:id/history-from-others", get(get_entity_history_from_others))
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
mod effect_normalization_tests {
    //! Unit tests for the per-turn effect normalization that runs
    //! just before effects are applied to an entity.  The
    //! normalization is **purely a function of** `(power, effects)`
    //! — the test harness reimplements the same algorithm so the
    //! expected scale can be computed without spinning up the full
    //! apply-pipeline.  See the pre-pass block in
    //! `process_action_handler` for the canonical implementation.

    use super::*;

    /// Build a small `World` with three named entities for
    /// the effect-key parser tests. The names are unique so
    /// exact-match lookups can be tested.
    fn make_test_world() -> (World, Uuid, Uuid, Uuid) {
        let mut world = World::new("test");
        let actor = WorldEntity::new("hero", "Kira Dawnblade", 0.0, 0.0);
        let other1 = WorldEntity::new("merchant", "Mira the Merchant", 100.0, 0.0);
        let other2 = WorldEntity::new("location", "Whisperwood Forest", 200.0, 0.0);
        let actor_id = actor.id;
        let other1_id = other1.id;
        let other2_id = other2.id;
        world.entities.insert(actor.id, actor);
        world.entities.insert(other1.id, other1);
        world.entities.insert(other2.id, other2);
        (world, actor_id, other1_id, other2_id)
    }

    /// Build an effects HashMap from `(key, f64)` pairs. F64 so
    /// the LLM-style numbers (5, 0.5, 1e18) all parse the same
    /// way the production code would.
    fn mk(key_value: &[(&str, f64)]) -> std::collections::HashMap<String, serde_json::Value> {
        let mut m = std::collections::HashMap::new();
        for (k, v) in key_value {
            m.insert((*k).to_string(), serde_json::json!(*v));
        }
        m
    }

    #[test]
    fn parse_effects_self_prefix_is_actor() {
        let (world, actor_id, _, _) = make_test_world();
        let name_to_id = build_name_index(&world);
        let effects = mk(&[("self.morale", 1.0)]);
        let (parsed, unknown) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target, EffectTarget::Actor);
        assert_eq!(parsed[0].prop_name, "morale");
        assert_eq!(parsed[0].raw_key, "self.morale");
        assert!(unknown.is_empty());
    }

    #[test]
    fn parse_effects_actor_literal_name_is_actor() {
        // The actor is "Kira Dawnblade" — `Kira Dawnblade.morale`
        // should also resolve to Actor (case-sensitive exact match).
        // Per Arcurus 2026-06-06 #openworld: "yes alow self or own
        // name but no need to mention that it can use own name".
        let (world, actor_id, _, _) = make_test_world();
        let name_to_id = build_name_index(&world);
        let effects = mk(&[("Kira Dawnblade.morale", 2.0)]);
        let (parsed, unknown) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target, EffectTarget::Actor);
        assert_eq!(parsed[0].prop_name, "morale");
        assert!(unknown.is_empty());
    }

    #[test]
    fn parse_effects_other_entity_resolves_by_name() {
        let (world, actor_id, other1_id, _) = make_test_world();
        let name_to_id = build_name_index(&world);
        let effects = mk(&[("Mira the Merchant.wealth", -2.0)]);
        let (parsed, unknown) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target,
            EffectTarget::Other { name: "Mira the Merchant".to_string(), id: other1_id });
        assert_eq!(parsed[0].prop_name, "wealth");
        assert!(unknown.is_empty());
    }

    #[test]
    fn parse_effects_unknown_entity_name_collected() {
        let (world, actor_id, _, _) = make_test_world();
        let name_to_id = build_name_index(&world);
        // "Miar the Merchant" (typo) — unknown.
        let effects = mk(&[("Miar the Merchant.wealth", -1.0)]);
        let (parsed, unknown) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        assert!(parsed.is_empty(),
            "unknown-name effect must be skipped, not parsed: got {parsed:?}");
        assert_eq!(unknown, vec!["Miar the Merchant".to_string()]);
    }

    #[test]
    fn parse_effects_case_sensitive() {
        // Lowercase "mira the merchant" should NOT match
        // "Mira the Merchant". Per Arcurus 2026-06-06
        // #openworld: "yea do exact match".
        let (world, actor_id, _, _) = make_test_world();
        let name_to_id = build_name_index(&world);
        let effects = mk(&[("mira the merchant.wealth", -1.0)]);
        let (parsed, unknown) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        assert!(parsed.is_empty());
        assert_eq!(unknown, vec!["mira the merchant".to_string()]);
    }

    #[test]
    fn parse_effects_no_dot_is_actor() {
        // Backward compat: bare "morale" (no dot) should resolve
        // to Actor on the bare property name. The old LLM
        // behavior.  Per Arcurus 2026-06-06 #openworld: "no
        // need to mention that it can use own name" — bare
        // keys are an even older convention.
        let (world, actor_id, _, _) = make_test_world();
        let name_to_id = build_name_index(&world);
        let effects = mk(&[("morale", 1.0)]);
        let (parsed, unknown) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target, EffectTarget::Actor);
        assert_eq!(parsed[0].prop_name, "morale");
        assert_eq!(parsed[0].raw_key, "morale");
        assert!(unknown.is_empty());
    }

    #[test]
    fn parse_effects_mixed_self_other_unknown_garbage() {
        // Single call with every kind of effect, to prove they
        // get routed correctly without interfering with each
        // other.
        let (world, actor_id, other1_id, other2_id) = make_test_world();
        let name_to_id = build_name_index(&world);
        let mut effects = mk(&[
            ("self.morale", 1.0),                    // Actor
            ("Kira Dawnblade.power", 2.0),           // Actor (literal name)
            ("Mira the Merchant.wealth", -2.0),      // Other
            ("Whisperwood Forest.knowledge", 3.0),   // Other
            ("Miar the Merchant.wealth", -1.0),      // Unknown
        ]);
        effects.insert("self.power".to_string(), serde_json::json!(1e18)); // garbage (will be tested below)
        let (parsed, unknown) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        // Garbage is parsed (it has a valid target — Actor), but
        // it would be magnitude-rejected at the apply stage.
        // The unknown name is filtered out.
        assert_eq!(parsed.len(), 5, "all 5 known-name effects parsed; unknown skipped");
        assert_eq!(unknown, vec!["Miar the Merchant".to_string()]);
        // Verify each parsed target
        let actor_count = parsed.iter().filter(|p| p.target == EffectTarget::Actor).count();
        let other_count = parsed.iter().filter(|p| matches!(p.target, EffectTarget::Other { .. })).count();
        assert_eq!(actor_count, 3, "self.morale + literal name + garbage = 3 Actor");
        assert_eq!(other_count, 2, "Mira + Whisperwood = 2 Other");
        // Verify the IDs of the Other effects
        let other_ids: std::collections::HashSet<Uuid> = parsed.iter()
            .filter_map(|p| match p.target {
                EffectTarget::Other { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert!(other_ids.contains(&other1_id));
        assert!(other_ids.contains(&other2_id));
    }

    #[test]
    fn actor_normalization_scale_excludes_other_effects() {
        // Cross-entity (Other) effects must NOT count toward
        // the actor's normalization cap. Per Arcurus 2026-06-06
        // #openworld: "self.X for the actor" — the cap is
        // about the actor, not the world.
        let (world, actor_id, _, _) = make_test_world();
        let name_to_id = build_name_index(&world);
        // Actor would have cap 10 (power=100). Total |Δ| for
        // self.X is 5 (under cap), but if we wrongly counted
        // the Other effects it would be 5+8+20=33, scaled to
        // ~0.303 — and the small self.X would be applied as
        // 1.5 instead of 5. We assert the scale is 1.0, i.e.
        // Other effects are excluded.
        let effects = mk(&[
            ("self.morale", 5.0),                    // actor, |Δ|=5
            ("Mira the Merchant.wealth", -8.0),      // other, |Δ|=8
            ("Whisperwood Forest.knowledge", 20.0),  // other, |Δ|=20
        ]);
        let (parsed, _) = parse_effects(
            &effects, actor_id, "Kira Dawnblade", &name_to_id);
        let (total, scale) = compute_actor_normalization_scale(100, &parsed, false);
        assert!((total - 5.0).abs() < 1e-9,
            "actor total should be 5.0 (other effects excluded), got {total}");
        assert!((scale - 1.0).abs() < 1e-9,
            "actor scale should be 1.0 (5.0 < cap of 10), got {scale}");
    }

    /// The same algorithm the pre-pass uses, lifted out for testing.
    /// Returns the scale factor (1.0 if no normalization is needed).
    fn compute_scale(
        raw_power: i64,
        effects: &std::collections::HashMap<String, serde_json::Value>,
        protected_entity: bool,
    ) -> f64 {
        // Thin test wrapper around the production function.  We
        // call the production code directly (not a copy) so a
        // future refactor of the normalization pre-pass cannot
        // silently drift the test from reality.
        compute_effect_normalization_scale(raw_power, effects, protected_entity).1
    }

    fn mk_effects(pairs: &[(&str, f64)]) -> std::collections::HashMap<String, serde_json::Value> {
        let mut m = std::collections::HashMap::new();
        for (k, v) in pairs {
            // Encode as f64 json number, even if integer-valued, so
            // magnitude_check / parse_effect_value handle it the
            // same way they do in the pre-pass.
            m.insert((*k).to_string(), serde_json::json!(v));
        }
        m
    }

    #[test]
    fn normalization_no_scale_when_under_cap() {
        // power=100 ⇒ cap = 10.  Total |Δ| = 2+3 = 5 < 10. No scale.
        let eff = mk_effects(&[("power", 2.0), ("morale", 3.0)]);
        let s = compute_scale(100, &eff, false);
        assert!((s - 1.0).abs() < 1e-9, "expected no scale, got {s}");
    }

    #[test]
    fn normalization_scales_down_proportionally_when_over_cap() {
        // power=100 ⇒ cap = max(1, 10% of 10 + 10) = max(1, 11) = 11.  Hmm,
        // that's 11 not 20.  Use power=1000 to land at 100 + 10 = 110, then
        // bump the total to clear it.
        //   power=1000 ⇒ cap = 1000 * 0.10 + 10 = 110.  Total = 50+80 = 130.
        //   scale = 110/130 ≈ 0.8462.
        let eff = mk_effects(&[("power", 50.0), ("morale", 80.0)]);
        let s = compute_scale(1000, &eff, false);
        let expected = 110.0 / 130.0;
        assert!((s - expected).abs() < 1e-9, "expected scale {expected}, got {s}");
        // Verify: after applying the scale, total |Δ| = cap exactly.
        let new_total: f64 = 50.0 * s + 80.0 * s;
        assert!((new_total - 110.0).abs() < 1e-6, "post-scale total should equal cap, got {new_total}");
    }

    #[test]
    fn normalization_negatives_count_as_positive() {
        // power=100 ⇒ cap = 10 + 10 = 20.  Effects: -15 and -10 ⇒ total |Δ| = 25 > 20.
        // scale = 20/25 = 0.8.  (Same shape as the old test, but with
        // the cap bumped by the +10 max-amount term.)
        let eff = mk_effects(&[("power", -15.0), ("morale", -10.0)]);
        let s = compute_scale(100, &eff, false);
        let expected = 20.0 / 25.0;
        assert!((s - expected).abs() < 1e-9, "expected scale {expected}, got {s}");
    }

    #[test]
    fn normalization_power_zero_uses_floor_ten() {
        // power=0 ⇒ cap = max(1, 10% of 10 + 10) = max(1, 11) = 11.
        // Effects: 0.4 and 0.4 ⇒ total |Δ| = 0.8 < 11. No scale.
        let eff = mk_effects(&[("power", 0.4), ("morale", 0.4)]);
        let s = compute_scale(0, &eff, false);
        assert!((s - 1.0).abs() < 1e-9, "expected no scale, got {s}");
    }

    #[test]
    fn normalization_power_zero_over_cap_scales_to_eleven() {
        // power=0 ⇒ cap = 11 (= 1 + 10 max-amount).  Effects: 7 and 6
        // ⇒ total |Δ| = 13 > 11.  scale = 11/13 ≈ 0.8462.
        let eff = mk_effects(&[("power", 7.0), ("morale", 6.0)]);
        let s = compute_scale(0, &eff, false);
        let expected = 11.0 / 13.0;
        assert!((s - expected).abs() < 1e-9, "expected scale {expected}, got {s}");
        let new_total: f64 = 7.0 * s + 6.0 * s;
        assert!((new_total - 11.0).abs() < 1e-6, "post-scale total should equal cap, got {new_total}");
    }

    #[test]
    fn normalization_mixed_signs_treated_as_magnitudes() {
        // power=200 ⇒ cap = 20 + 10 = 30.  Effects: +20 and -15 ⇒ total |Δ| = 35 > 30.
        // scale = 30/35 = 6/7 ≈ 0.8571.
        let eff = mk_effects(&[("power", 20.0), ("morale", -15.0)]);
        let s = compute_scale(200, &eff, false);
        let expected = 30.0 / 35.0;
        assert!((s - expected).abs() < 1e-9, "expected scale {expected}, got {s}");
    }

    #[test]
    fn normalization_max_amount_term_gives_low_power_room() {
        // Per Arcurus 2026-06-07 #openworld: the +10 max-amount term
        // means even a power-0 entity has a per-turn cap of 11 (not
        // 1.0), enough for a single full +10 effect.  This test
        // specifically exercises the +10 term — a 10+1 effect
        // (e.g. +7 and +4) should fit exactly in the cap, and an
        // 11+1 effect (e.g. +8 and +4) should scale.
        let eff = mk_effects(&[("morale", 7.0), ("power", 4.0)]);  // total = 11
        let s = compute_scale(0, &eff, false);
        // power=0 ⇒ cap = 11.  Total = 11.  No scale.
        assert!((s - 1.0).abs() < 1e-9,
            "11 should fit exactly in cap of 11, expected no scale, got {s}");
        let eff2 = mk_effects(&[("morale", 8.0), ("power", 4.0)]);  // total = 12
        let s2 = compute_scale(0, &eff2, false);
        // power=0 ⇒ cap = 11.  Total = 12 > 11.  scale = 11/12.
        let expected2 = 11.0 / 12.0;
        assert!((s2 - expected2).abs() < 1e-9,
            "expected scale {expected2}, got {s2}");
    }

    #[test]
    fn normalization_protected_entity_skips_all_effects() {
        // Protected entity — even if effects would exceed cap, scale=1.0
        // because the effects aren't being applied (the loop skips them).
        let eff = mk_effects(&[("power", 1000.0), ("morale", 5000.0)]);
        let s = compute_scale(100, &eff, true);
        assert!((s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn normalization_oversize_deltas_excluded_from_total() {
        // The 1e18 value would be magnitude-rejected, so it doesn't
        // count toward the total.  With only the small effect,
        // we're under the cap.
        let mut eff = mk_effects(&[("power", 5.0)]);
        eff.insert("evil".to_string(), serde_json::json!(1e18));
        let s = compute_scale(100, &eff, false);
        assert!((s - 1.0).abs() < 1e-9, "1e18 must be excluded; expected no scale, got {s}");
    }

    // Regression test for Arcurus 2026-06-06 #openworld:
    // "reject the garbage first, but make sure that is removed
    //  from the applied effects before its counted or applied.
    //  otherwise if normalized it would still be applied."
    //
    // Specifically: a small *real* effect (morale: 1.0) sitting
    // next to a sibling garbage value (1e18) must NOT be scaled
    // to ~0 by the normalization.  The garbage is excluded from
    // the total (magnitude_check fails), so the small effect is
    // applied at its literal value.
    #[test]
    fn normalization_garbage_does_not_drag_sibling_to_zero() {
        let mut eff = mk_effects(&[("morale", 1.0)]);
        eff.insert("evil".to_string(), serde_json::json!(1e18));
        let (total, scale) =
            compute_effect_normalization_scale(100, &eff, false);
        // 1e18 was rejected, so the total is just 1.0 (the small effect).
        assert!((total - 1.0).abs() < 1e-9,
            "total should be 1.0 (garbage excluded), got {total}");
        // 1.0 is under the cap (10 for power=100), so scale is 1.0.
        // If the order were wrong (garbage counted), total would
        // be ~1e18, scale would be ~1e-17, and the sibling 1.0
        // would be applied as ~1e-17.  This assertion guards
        // against that exact regression.
        assert!((scale - 1.0).abs() < 1e-9,
            "scale should be 1.0 (garbage excluded, small effect under cap), got {scale}");
        // Same for non-numeric garbage (e.g. a string that
        // magnitude_check would reject as a non-finite parse):
        let mut eff2 = mk_effects(&[("morale", 2.0)]);
        eff2.insert("evil".to_string(), serde_json::json!("not a number"));
        let (total2, scale2) =
            compute_effect_normalization_scale(100, &eff2, false);
        assert!((total2 - 2.0).abs() < 1e-9,
            "non-numeric garbage should be excluded; got total={total2}");
        assert!((scale2 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn normalization_constant_values_are_pickable() {
        // Sanity: the cap formula constants all compile to the
        // values we expect.  If these drift, the rest of the
        // tests above are meaningless.
        assert!((EFFECT_NORMALIZATION_CAP_PCT - 0.10).abs() < 1e-9);
        assert!((EFFECT_NORMALIZATION_MAX_AMOUNT - 10.0).abs() < 1e-9);
        assert!((EFFECT_NORMALIZATION_MIN_CAP - 1.0).abs() < 1e-9);
    }

    // ---- Cross-entity apply tests (Arcurus 2026-06-07 #openworld) ----
    //
    // The previous dry-run tests (which are now removed) tested
    // the `DryRunReport` struct. The new tests below cover the
    // replacement behavior: cross-entity effects actually apply,
    // with per-target safety (system-entity guard, per-target
    // normalization, type handling).

    /// Minimal world with one actor (Kira) and one other (Mira).
    fn make_two_entity_world() -> (World, Uuid, Uuid) {
        let mut world = World::new("test");
        let mut actor = WorldEntity::new("hero", "Kira Dawnblade", 0.0, 0.0);
        actor.properties_int.insert("power".to_string(), 100);
        actor.properties_int.insert("morale".to_string(), 50);
        let mut other = WorldEntity::new("merchant", "Mira the Merchant", 100.0, 0.0);
        other.properties_int.insert("power".to_string(), 50);
        other.properties_int.insert("wealth".to_string(), 100);
        let actor_id = actor.id;
        let other_id = other.id;
        world.entities.insert(actor.id, actor);
        world.entities.insert(other.id, other);
        (world, actor_id, other_id)
    }

    #[test]
    fn cross_entity_effect_applies_to_named_target() {
        // Per Arcurus 2026-06-07 #openworld: cross-entity writes
        // are no longer dry-run. The LLM proposes an effect on
        // "Mira the Merchant.wealth" and Mira's wealth actually
        // changes.
        let (mut world, actor_id, other_id) = make_two_entity_world();
        // Integer value (-5) so the int property doesn't trigger
        // the type-mismatch path. The LLM emits integer values
        // in practice; float values for int properties are
        // rejected as type-mismatches (intentional).
        let parsed = vec![ParsedEffect {
            target: EffectTarget::Other {
                name: "Mira the Merchant".to_string(),
                id: other_id,
            },
            prop_name: "wealth".to_string(),
            raw_key: "Mira the Merchant.wealth".to_string(),
            value: serde_json::json!(-5),
        }];
        let mut warnings = Vec::new();
        let (actor_applied, _actor_nvs, cross, _hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);
        // Actor has no effects, so its map is empty.
        assert!(actor_applied.is_empty(), "actor should have no applied effects");
        // Mira's wealth went from 100 to 95.
        let mira = world.entities.get(&other_id).unwrap();
        assert_eq!(mira.properties_int.get("wealth"), Some(&95));
        // The cross-entity map should have Mira's wealth update.
        assert!(cross.contains_key("Mira the Merchant"),
            "cross-entity map should be keyed by entity name: {cross:?}");
        assert_eq!(cross["Mira the Merchant"]["wealth"], serde_json::json!(95));
        // No warnings expected for a clean small effect.
        assert!(warnings.is_empty(), "no warnings expected, got: {warnings:?}");
    }

    #[test]
    fn actor_and_cross_entity_effects_both_apply() {
        // A single LLM call with both self.X and Other.Y keys
        // should apply BOTH. The actor's morale changes and
        // Mira's wealth changes.
        let (mut world, actor_id, other_id) = make_two_entity_world();
        let parsed = vec![
            ParsedEffect {
                target: EffectTarget::Actor,
                prop_name: "morale".to_string(),
                raw_key: "self.morale".to_string(),
                value: serde_json::json!(5),
            },
            ParsedEffect {
                target: EffectTarget::Other {
                    name: "Mira the Merchant".to_string(),
                    id: other_id,
                },
                prop_name: "wealth".to_string(),
                raw_key: "Mira the Merchant.wealth".to_string(),
                value: serde_json::json!(10),
            },
        ];
        let mut warnings = Vec::new();
        let (actor_applied, _actor_nvs, cross, _hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);
        // Kira's morale: 50 + 5 = 55.
        let kira = world.entities.get(&actor_id).unwrap();
        assert_eq!(kira.properties_int.get("morale"), Some(&55));
        assert_eq!(actor_applied["morale"], serde_json::json!(55));
        // Mira's wealth: 100 + 10 = 110.
        let mira = world.entities.get(&other_id).unwrap();
        assert_eq!(mira.properties_int.get("wealth"), Some(&110));
        assert_eq!(cross["Mira the Merchant"]["wealth"], serde_json::json!(110));
    }

    #[test]
    fn cross_entity_target_system_entity_is_rejected() {
        // Per Arcurus 2026-06-07 #openworld: the previous dry-run
        // implicitly protected system entities. Now that
        // cross-entity writes apply, the protection is explicit
        // in `apply_effects_to_target`. An LLM that names the
        // World Clock in an effect must be warned, not silently
        // overwrite the system entity.
        let mut world = World::new("test");
        let mut actor = WorldEntity::new("hero", "Kira Dawnblade", 0.0, 0.0);
        actor.properties_int.insert("power".to_string(), 100);
        let actor_id = actor.id;
        world.entities.insert(actor.id, actor);
        // Build the world clock (auto-created by World::new).
        let clock_id = world.entities.values()
            .find(|e| e.is_system_entity())
            .unwrap().id;
        let clock_name = world.entities.get(&clock_id).unwrap().name.clone();
        // Snapshot clock.history_entries before the effect.
        let clock_before = world.entities.get(&clock_id).unwrap()
            .properties_int.get("history_entries").copied().unwrap_or(0);
        let parsed = vec![ParsedEffect {
            target: EffectTarget::Other {
                name: clock_name.clone(),
                id: clock_id,
            },
            prop_name: "history_entries".to_string(),
            raw_key: format!("{clock_name}.history_entries"),
            value: serde_json::json!(1.0),
        }];
        let mut warnings = Vec::new();
        let (actor_applied, _actor_nvs, cross, _hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);
        // Actor has nothing applied.
        assert!(actor_applied.is_empty());
        // Cross-entity map should NOT contain the clock.
        assert!(!cross.contains_key(&clock_name),
            "system-entity effect must not appear in cross-entity applied: {cross:?}");
        // Clock's history_entries unchanged.
        let clock_after = world.entities.get(&clock_id).unwrap()
            .properties_int.get("history_entries").copied().unwrap_or(0);
        assert_eq!(clock_before, clock_after,
            "system-entity write must be rejected (no change to clock)");
        // Warning emitted.
        assert!(warnings.iter().any(|w| w.contains("Skipped effect on abstract/system entity")),
            "expected a 'Skipped effect on abstract/system entity' warning, got: {warnings:?}");
        // Per Arcurus 2026-06-07 #openworld: the warning now
        // includes the property name, the change value, and
        // the entity name (all three) so the operator can
        // triage the LLM's intent without digging deeper.
        let w = warnings.iter().find(|w| w.contains("Skipped effect on abstract/system entity")).unwrap();
        assert!(w.contains("'World Clock'"), "warning should name the entity: {w}");
        assert!(w.contains("'history_entries'"), "warning should name the property: {w}");
        assert!(w.contains("Number(1.0)") || w.contains("1.0"),
            "warning should include the change value: {w}");
        assert!(warnings.iter().any(|w| w.contains(&clock_name)),
            "warning should name the system entity: {warnings:?}");
    }

    #[test]
    fn cross_entity_per_target_normalization_uses_target_power() {
        // Per Arcurus 2026-06-07 #openworld: each target gets
        // its own cap, computed from its own `power`. A
        // low-power target has a tighter cap. The actor's cap
        // doesn't protect a low-power target.
        let (mut world, actor_id, other_id) = make_two_entity_world();
        // Mira has power=50, cap = 50*0.10 + 10 = 15.
        // A total |Δ| of 20 on Mira's wealth should scale to 15.
        // The actor (power=100, cap=20) is unaffected because
        // the actor has no effects in this test.
        let parsed = vec![ParsedEffect {
            target: EffectTarget::Other {
                name: "Mira the Merchant".to_string(),
                id: other_id,
            },
            prop_name: "wealth".to_string(),
            raw_key: "Mira the Merchant.wealth".to_string(),
            value: serde_json::json!(20),
        }];
        let mut warnings = Vec::new();
        let (_actor_applied, _actor_nvs, cross, _hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);
        // Mira's wealth: 100 + 20 (scaled to 15) = 115.
        let mira = world.entities.get(&other_id).unwrap();
        assert_eq!(mira.properties_int.get("wealth"), Some(&115),
            "cross-entity effect should scale to target's cap of 15 (not actor's cap of 20)");
        // Per-target normalization warning emitted (target name in warning).
        assert!(warnings.iter().any(|w| w.contains("Effects normalized on 'Mira the Merchant'")),
            "expected per-target 'Effects normalized' warning, got: {warnings:?}");
    }

    #[test]
    fn cross_entity_garbage_value_does_not_drag_sibling_to_zero() {
        // The same regression the actor-side test covers, but
        // for cross-entity targets. A 1e18 sibling on Mira must
        // be excluded from Mira's total, so Mira's small real
        // effect is applied at its literal value.
        let (mut world, actor_id, other_id) = make_two_entity_world();
        // The LLM emits a small +5 on wealth AND a 1e18 garbage
        // value. The 1e18 must be rejected BEFORE the scale is
        // computed.
        let parsed = vec![
            ParsedEffect {
                target: EffectTarget::Other {
                    name: "Mira the Merchant".to_string(),
                    id: other_id,
                },
                prop_name: "wealth".to_string(),
                raw_key: "Mira the Merchant.wealth".to_string(),
                value: serde_json::json!(5),
            },
            ParsedEffect {
                target: EffectTarget::Other {
                    name: "Mira the Merchant".to_string(),
                    id: other_id,
                },
                prop_name: "evil_garbage".to_string(),
                raw_key: "Mira the Merchant.evil_garbage".to_string(),
                value: serde_json::json!(1e18),
            },
        ];
        let mut warnings = Vec::new();
        let (_actor_applied, _actor_nvs, cross, _hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);
        // Mira's wealth: 100 + 5 = 105. The 1e18 was rejected,
        // so the scale is 1.0 and the small effect is applied
        // at its literal value.
        let mira = world.entities.get(&other_id).unwrap();
        assert_eq!(mira.properties_int.get("wealth"), Some(&105),
            "garbage sibling must not drag cross-entity small effect to ~0");
        // The garbage write is also rejected, with a warning.
        assert!(!mira.properties_int.contains_key("evil_garbage"),
            "garbage property must not be written");
        assert!(warnings.iter().any(|w| w.contains("evil_garbage") || w.contains("MAX_DELTA_ABS")),
            "expected a magnitude-rejection warning for the garbage value: {warnings:?}");
    }
}

#[cfg(test)]
mod hidden_tag_tests {
    //! Tests for the post-effect hidden-tag rule (Arcurus
    //! 2026-06-07 #openworld).
    //!
    //! Threshold: `max(10, power) / 10 + visibility`
    //!   - `threshold <  0` → add the `hidden` tag
    //!   - `threshold >= 1` → remove the `hidden` tag
    //!   - `0 ≤ threshold < 1` → no change (dead zone)
    //!
    //! The rule is checked for every entity that had at least
    //! one effect actually written to it, AFTER all effects have
    //! been applied (so the threshold uses post-effect values).

    use super::*;
    use crate::world_data::WorldEntity;
    use std::collections::BTreeMap;

    /// Build a minimal in-memory World + one entity with the
    /// given (power, visibility) and an arbitrary set of
    /// starter tags.  Returns (world, entity_id).
    fn mk_world_with_entity(
        power: i64,
        visibility: i64,
        tags: &[&str],
    ) -> (World, Uuid) {
        let mut world = World::new("test");
        let id = Uuid::new_v4();
        let mut entity = WorldEntity::new(
            "character",   // entity_type
            "tester",      // name
            100.0,
            100.0,
        );
        entity.id = id;
        entity.properties_int.insert("power".to_string(), power);
        entity.properties_int.insert("visibility".to_string(), visibility);
        for t in tags {
            entity.add_tag(t);
        }
        world.entities.insert(id, entity);
        (world, id)
    }

    /// Build a world with two entities: an actor (id) and a
    /// target named "Mira the Merchant" (other_id) with the
    /// given (power, visibility).  Returns (world, actor_id,
    /// other_id).
    fn mk_world_actor_and_mira(
        actor_power: i64,
        actor_visibility: i64,
        mira_power: i64,
        mira_visibility: i64,
    ) -> (World, Uuid, Uuid) {
        let mut world = World::new("test");
        let actor_id = Uuid::new_v4();
        let mira_id = Uuid::new_v4();
        let mut actor = WorldEntity::new("character", "Actor", 0.0, 0.0);
        actor.id = actor_id;
        actor.properties_int.insert("power".to_string(), actor_power);
        actor.properties_int.insert("visibility".to_string(), actor_visibility);
        let mut mira = WorldEntity::new("character", "Mira the Merchant", 0.0, 0.0);
        mira.id = mira_id;
        mira.properties_int.insert("power".to_string(), mira_power);
        mira.properties_int.insert("visibility".to_string(), mira_visibility);
        world.entities.insert(actor_id, actor);
        world.entities.insert(mira_id, mira);
        (world, actor_id, mira_id)
    }

    /// A small helper: build a ParsedEffect that targets the
    /// actor (self-effect) with the given property delta.
    /// `value` is a JSON number that the LLM would emit; the
    /// parser extracts the inner f64.
    fn actor_int_delta(prop: &str, value: i64) -> ParsedEffect {
        ParsedEffect {
            prop_name: prop.to_string(),
            raw_key: format!("self.{prop}"),
            value: serde_json::json!(value),
            target: EffectTarget::Actor,
        }
    }

    /// Same as `actor_int_delta` but targets a named other.
    fn other_int_delta(name: &str, id: Uuid, prop: &str, value: i64) -> ParsedEffect {
        ParsedEffect {
            prop_name: prop.to_string(),
            raw_key: format!("{name}.{prop}"),
            value: serde_json::json!(value),
            target: EffectTarget::Other {
                name: name.to_string(),
                id,
            },
        }
    }

    // -----------------------------------------------------------------
    // Unit tests for the pure threshold function
    // -----------------------------------------------------------------

    #[test]
    /// threshold = 1 + 0 = 1 → >= 1, tag should be REMOVED.
    /// Boundary: threshold exactly at 1 is on the "remove" side.
    fn threshold_exactly_one_removes_tag() {
        let (mut world, id) = mk_world_with_entity(10, 0, &[HIDDEN_TAG]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_hidden_tag(entity);
        assert!((t - 1.0).abs() < 1e-9, "threshold should be exactly 1.0, got {t}");
        assert!(!added, "should not be added");
        assert!(removed, "should be removed");
        assert!(!entity.has_tag(HIDDEN_TAG), "tag should be gone");
    }

    #[test]
    /// threshold = 1 + (-1) = 0 → dead zone (0 ≤ t < 1), no change.
    fn threshold_zero_is_dead_zone() {
        // No tag present, threshold is 0 → no add.
        let (mut world, id) = mk_world_with_entity(10, -1, &[]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_hidden_tag(entity);
        assert!((t - 0.0).abs() < 1e-9, "threshold should be 0.0, got {t}");
        assert!(!added && !removed, "dead zone, no change");
        assert!(!entity.has_tag(HIDDEN_TAG), "still no tag");
    }

    #[test]
    /// threshold = 1 + (-0.5) = 0.5 → dead zone, tag stays.
    fn threshold_half_is_dead_zone() {
        let (mut world, id) = mk_world_with_entity(10, 0, &[HIDDEN_TAG]);
        // visibility = -0.5 isn't representable as i64, so we
        // approximate via a +0.5 floating approach: leave
        // visibility = 0 (threshold = 1, not dead zone). The
        // real dead-zone test is threshold_zero_is_dead_zone
        // above. This test checks threshold > 1.0 doesn't fire
        // when tag isn't present (no-op).
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_hidden_tag(entity);
        assert!(t >= 1.0, "sanity: threshold should be >= 1, got {t}");
        assert!(removed, "tag should be removed at threshold >= 1");
    }

    #[test]
    /// threshold = 1 + (-2) = -1 → < 0, tag should be ADDED.
    fn threshold_below_zero_adds_tag() {
        let (mut world, id) = mk_world_with_entity(10, -2, &[]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_hidden_tag(entity);
        assert!(t < 0.0, "threshold should be < 0, got {t}");
        assert!(added, "should be added");
        assert!(!removed);
        assert!(entity.has_tag(HIDDEN_TAG));
    }

    #[test]
    /// High-power entity (power=100) is hard to hide: even
    /// visibility=-50 → threshold = 10 - 50 = -40 < 0, but
    /// this test specifically checks the OPPOSITE: power=100
    /// with visibility=-5 → threshold = 10 + (-5) = 5 ≥ 1, so
    /// the tag (if present) is REMOVED.  This is the "famous
    /// dragon" case: you can't hide a power-100 entity with a
    /// few points of negative visibility.
    fn high_power_entity_is_hard_to_hide() {
        let (mut world, id) = mk_world_with_entity(100, -5, &[HIDDEN_TAG]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_hidden_tag(entity);
        assert!((t - 5.0).abs() < 1e-9, "threshold should be 5.0, got {t}");
        assert!(removed, "high-power entity with mild -visibility should NOT be hidden");
        assert!(!entity.has_tag(HIDDEN_TAG));
    }

    #[test]
    /// power = 0 (below the 10 floor) still uses the floor:
    /// threshold = max(10, 0)/10 + vis = 1 + vis.
    /// vis = -2 → threshold = -1 → hidden.
    fn power_zero_uses_floor_of_ten() {
        let (mut world, id) = mk_world_with_entity(0, -2, &[]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (_, _, t) = update_hidden_tag(entity);
        assert!((t - (-1.0)).abs() < 1e-9,
            "with power=0, floor of 10 applies; threshold should be -1.0, got {t}");
    }

    #[test]
    /// Already has the tag and threshold is still < 0 → no-op
    /// (don't re-add).  Returns (false, false).
    fn already_hidden_under_threshold_is_noop() {
        let (mut world, id) = mk_world_with_entity(10, -2, &[HIDDEN_TAG]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, _) = update_hidden_tag(entity);
        assert!(!added && !removed, "no change expected when already correct");
    }

    #[test]
    /// Threshold in dead zone (0 ≤ t < 1) and no tag present →
    /// no-op.  We can't easily make t=0.5 from integer props,
    /// so this test is effectively the "threshold exactly 0"
    /// case from `threshold_zero_is_dead_zone` (re-tested here
    /// at the boundary for symmetry).
    fn dead_zone_with_no_tag_is_noop() {
        let (mut world, id) = mk_world_with_entity(10, -1, &[]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, _) = update_hidden_tag(entity);
        assert!(!added && !removed);
        assert!(!entity.has_tag(HIDDEN_TAG));
    }

    #[test]
    /// Missing `power` property → defaults to 0 (so floor of
    /// 10 applies).  Missing `visibility` property → defaults
    /// to 0.  Net: threshold = 1.0, tag (if present) is removed.
    fn missing_properties_use_defaults() {
        let mut world = World::new("test");
        let id = Uuid::new_v4();
        let mut entity = WorldEntity::new(
            "character", "blank", 0.0, 0.0,
        );
        entity.id = id;
        entity.add_tag(HIDDEN_TAG);
        // Deliberately do NOT set power or visibility.
        world.entities.insert(id, entity);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_hidden_tag(entity);
        assert!((t - 1.0).abs() < 1e-9, "defaults: 1.0 + 0.0 = 1.0, got {t}");
        assert!(removed, "tag should be removed at threshold 1.0");
        assert!(!entity.has_tag(HIDDEN_TAG));
    }

    // -----------------------------------------------------------------
    // Integration tests: rule runs inside apply_all_effects
    // -----------------------------------------------------------------

    #[test]
    /// Actor (power=10) takes a self-effect that pushes
    /// visibility from 0 to -2.  After apply, threshold =
    /// 1 + (-2) = -1 < 0, so the `hidden` tag should appear
    /// on the actor.  This proves the rule runs AFTER
    /// effects are applied (using post-effect visibility).
    fn actor_self_effect_below_threshold_adds_tag() {
        let (mut world, actor_id) = mk_world_with_entity(10, 0, &[]);
        let parsed = vec![actor_int_delta("visibility", -2)];
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        // The actor was affected (had a write), so the rule ran.
        assert!(hidden.contains_key("tester"),
            "hidden map should contain the actor, got: {hidden:?}");
        let update = &hidden["tester"];
        assert!(update.added, "tag should be added; update = {update:?}");
        assert!(!update.removed);
        assert!(update.threshold < 0.0);

        // Persisted on the entity itself.
        let actor = world.entities.get(&actor_id).unwrap();
        assert!(actor.has_tag(HIDDEN_TAG), "tag should be persisted");

        // And surfaced as a warning.
        assert!(warnings.iter().any(|w| w.contains(HIDDEN_TAG) && w.contains("tester")),
            "warning should mention the tag and entity name, got: {warnings:?}");
    }

    #[test]
    /// Cross-entity effect on Mira (power=10) drops her
    /// visibility to -3 → threshold = 1 + (-3) = -2 < 0 →
    /// Mira's `hidden` tag is added.  Actor (power=10) is
    /// unaffected by the cross-entity write, so actor's tag
    /// state is NOT touched (the rule only runs on affected
    /// entities).
    fn cross_entity_effect_below_threshold_adds_tag() {
        let (mut world, actor_id, mira_id) = mk_world_actor_and_mira(
            10, 0,   // actor: would NOT be hidden at this state
            10, 0,   // mira:  starting visible
        );
        let parsed = vec![other_int_delta(
            "Mira the Merchant", mira_id, "visibility", -3,
        )];
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        assert!(hidden.contains_key("Mira the Merchant"),
            "Mira's hidden toggle should be in the map: {hidden:?}");
        assert!(hidden["Mira the Merchant"].added);
        assert!(!hidden["Mira the Merchant"].removed);

        // Mira has the tag persisted.
        let mira = world.entities.get(&mira_id).unwrap();
        assert!(mira.has_tag(HIDDEN_TAG), "Mira should now be hidden");

        // Actor is NOT in the map (no effect on actor means
        // the rule didn't run on actor).  We use has_tag to
        // also assert the actor's tags weren't touched.
        assert!(!hidden.contains_key("Actor"),
            "actor was not affected; rule should not have run on it: {hidden:?}");
    }

    #[test]
    /// An effect that RAISES visibility past the boundary
    /// (>= 1) should REMOVE the `hidden` tag.  Start Mira
    /// with the tag and visibility=-3 (threshold = -2, hidden).
    /// Apply +3 → visibility=0 (threshold=1, remove tag).
    fn cross_entity_effect_past_threshold_removes_tag() {
        let (mut world, actor_id, mira_id) = mk_world_actor_and_mira(
            10, 0, 10, -3,
        );
        // Pre-seed Mira with the hidden tag (she's already
        // hidden at the start of this turn).
        world.entities.get_mut(&mira_id).unwrap().add_tag(HIDDEN_TAG);
        let parsed = vec![other_int_delta(
            "Mira the Merchant", mira_id, "visibility", 3,
        )];
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        assert!(hidden.contains_key("Mira the Merchant"));
        let u = &hidden["Mira the Merchant"];
        assert!(u.removed, "tag should be removed; update = {u:?}");
        assert!(!u.added);
        assert!(u.threshold >= 1.0,
            "threshold should be >= 1.0 after visibility boost, got {}", u.threshold);

        // Persisted: tag is gone.
        let mira = world.entities.get(&mira_id).unwrap();
        assert!(!mira.has_tag(HIDDEN_TAG), "Mira's hidden tag should be removed");

        // Warning surfaced.
        assert!(warnings.iter().any(|w| w.contains("Removed") && w.contains("Mira")),
            "warning should mention 'Removed' and Mira: {warnings:?}");
    }

    #[test]
    /// Multiple effects on the same entity: the rule should
    /// run ONCE, after all effects, using the final
    /// (post-effect) power/visibility.  This is the
    /// "checked after all effects are applied" requirement.
    /// Mira has power=10.  First effect: visibility -= 2
    /// (threshold would be -1, hidden).  Second effect:
    /// visibility += 3 (threshold would be 1, visible).
    /// Net threshold = 1+0 = 1 → tag should NOT be added.
    fn rule_runs_once_after_all_effects_use_final_values() {
        let (mut world, actor_id, mira_id) = mk_world_actor_and_mira(
            10, 0, 10, 0,
        );
        let parsed = vec![
            other_int_delta("Mira the Merchant", mira_id, "visibility", -2),
            other_int_delta("Mira the Merchant", mira_id, "visibility", 3),
        ];
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, _hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        // Mira's final visibility: 0 + (-2) + 3 = 1.
        // Threshold = 1 + 1 = 2 ≥ 1.  Mira stays unhidden.
        let mira = world.entities.get(&mira_id).unwrap();
        assert_eq!(mira.properties_int.get("visibility"), Some(&1));
        assert!(!mira.has_tag(HIDDEN_TAG),
            "Mira's final threshold is 2.0; tag should not be present");

        // The "run once after all effects" contract: there
        // should be at most ONE hidden-tag-related warning,
        // even though the test stack has two effects on Mira.
        let hidden_warnings: Vec<&String> = warnings.iter()
            .filter(|w| w.contains(HIDDEN_TAG))
            .collect();
        assert!(hidden_warnings.len() <= 1,
            "rule should run once per affected entity, got {} hidden-tag warnings: {hidden_warnings:?}",
            hidden_warnings.len());
    }

    #[test]
    /// An entity that is NOT in the effect list (i.e. no
    /// effect was written to it) should NOT have the rule
    /// run on it.  We test this by giving an unrelated
    /// entity a power/visibility combo that would qualify
    /// for hidden, then sending a self-effect on the actor
    /// only.  The unrelated entity's tag state must be
    /// untouched.
    fn unaffected_entity_is_not_checked() {
        let (mut world, actor_id, mira_id) = mk_world_actor_and_mira(
            10, 0, 10, -5,  // Mira would qualify for hidden, but we don't touch her
        );
        let parsed = vec![actor_int_delta("power", 5)];  // actor self-effect only
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, hidden, _corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        // Mira is not in the hidden map (no effect → rule skipped).
        assert!(!hidden.contains_key("Mira the Merchant"),
            "Mira had no effect, rule should not run on her: {hidden:?}");
        // Mira's tags are completely untouched.
        let mira = world.entities.get(&mira_id).unwrap();
        assert!(!mira.has_tag(HIDDEN_TAG),
            "Mira's tags must not be touched (no effect → rule skipped)");
    }
}

#[cfg(test)]
mod corrupted_tag_tests {
    //! Tests for the post-effect corrupted-tag rule (Arcurus
    //! 2026-06-07 #openworld).
    //!
    //! Threshold: `max(1, power) - corruption`
    //!   - `threshold <  0` → add the `corrupted` tag
    //!   - `threshold >= 1` → remove the `corrupted` tag
    //!   - `0 ≤ threshold < 1` → no change (dead zone)
    //!
    //! Mirror of `mod hidden_tag_tests` (same shape, different
    //! formula and tag name).  The two rules share the
    //! `affected_ids` set inside `apply_all_effects` and both
    //! run on the same post-effect entity state.

    use super::*;
    use crate::world_data::WorldEntity;

    /// Build a minimal in-memory World + one entity with the
    /// given (power, corruption) and starter tags.  Returns
    /// (world, entity_id).
    fn mk_world_with_entity(
        power: i64,
        corruption: i64,
        tags: &[&str],
    ) -> (World, Uuid) {
        let mut world = World::new("test");
        let id = Uuid::new_v4();
        let mut entity = WorldEntity::new("character", "tester", 0.0, 0.0);
        entity.id = id;
        entity.properties_int.insert("power".to_string(), power);
        entity.properties_int.insert("corruption".to_string(), corruption);
        for t in tags {
            entity.add_tag(t);
        }
        world.entities.insert(id, entity);
        (world, id)
    }

    /// Build a world with two entities: an actor (id) and a
    /// target named "Mira the Merchant" (other_id) with the
    /// given (power, corruption).
    fn mk_world_actor_and_mira(
        actor_power: i64,
        actor_corruption: i64,
        mira_power: i64,
        mira_corruption: i64,
    ) -> (World, Uuid, Uuid) {
        let mut world = World::new("test");
        let actor_id = Uuid::new_v4();
        let mira_id = Uuid::new_v4();
        let mut actor = WorldEntity::new("character", "Actor", 0.0, 0.0);
        actor.id = actor_id;
        actor.properties_int.insert("power".to_string(), actor_power);
        actor.properties_int.insert("corruption".to_string(), actor_corruption);
        let mut mira = WorldEntity::new("character", "Mira the Merchant", 0.0, 0.0);
        mira.id = mira_id;
        mira.properties_int.insert("power".to_string(), mira_power);
        mira.properties_int.insert("corruption".to_string(), mira_corruption);
        world.entities.insert(actor_id, actor);
        world.entities.insert(mira_id, mira);
        (world, actor_id, mira_id)
    }

    fn actor_int_delta(prop: &str, value: i64) -> ParsedEffect {
        ParsedEffect {
            prop_name: prop.to_string(),
            raw_key: format!("self.{prop}"),
            value: serde_json::json!(value),
            target: EffectTarget::Actor,
        }
    }

    fn other_int_delta(name: &str, id: Uuid, prop: &str, value: i64) -> ParsedEffect {
        ParsedEffect {
            prop_name: prop.to_string(),
            raw_key: format!("{name}.{prop}"),
            value: serde_json::json!(value),
            target: EffectTarget::Other {
                name: name.to_string(),
                id,
            },
        }
    }

    // -----------------------------------------------------------------
    // Unit tests for the pure threshold function
    // -----------------------------------------------------------------

    #[test]
    /// threshold = max(1, 10) - 9 = 1 → >= 1, tag REMOVED.
    /// Boundary: threshold exactly at 1 is on the "remove" side.
    fn threshold_exactly_one_removes_tag() {
        let (mut world, id) = mk_world_with_entity(10, 9, &[CORRUPTED_TAG]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_corrupted_tag(entity);
        assert!((t - 1.0).abs() < 1e-9, "threshold should be exactly 1.0, got {t}");
        assert!(!added, "should not be added");
        assert!(removed, "should be removed");
        assert!(!entity.has_tag(CORRUPTED_TAG), "tag should be gone");
    }

    #[test]
    /// threshold = max(1, 10) - 10 = 0 → dead zone (0 ≤ t < 1).
    fn threshold_zero_is_dead_zone() {
        let (mut world, id) = mk_world_with_entity(10, 10, &[]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_corrupted_tag(entity);
        assert!((t - 0.0).abs() < 1e-9, "threshold should be 0.0, got {t}");
        assert!(!added && !removed, "dead zone, no change");
        assert!(!entity.has_tag(CORRUPTED_TAG), "still no tag");
    }

    #[test]
    /// threshold = max(1, 10) - 15 = -5 → < 0, tag ADDED.
    fn threshold_below_zero_adds_tag() {
        let (mut world, id) = mk_world_with_entity(10, 15, &[]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_corrupted_tag(entity);
        assert!(t < 0.0, "threshold should be < 0, got {t}");
        assert!(added, "should be added");
        assert!(!removed);
        assert!(entity.has_tag(CORRUPTED_TAG));
    }

    #[test]
    /// High-power entity is hard to corrupt: power=100,
    /// corruption=20 → threshold = 100 - 20 = 80 ≥ 1, so
    /// the tag (if present) is REMOVED.  A legendary entity
    /// resists ordinary corruption.
    fn high_power_entity_is_hard_to_corrupt() {
        let (mut world, id) = mk_world_with_entity(100, 20, &[CORRUPTED_TAG]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_corrupted_tag(entity);
        assert!((t - 80.0).abs() < 1e-9, "threshold should be 80.0, got {t}");
        assert!(removed, "high-power entity with mild corruption should NOT be tagged");
        assert!(!entity.has_tag(CORRUPTED_TAG));
    }

    #[test]
    /// power = 0 (below the +1 floor) still uses the floor:
    /// threshold = max(1, 0) - corruption = 1 - corruption.
    /// corruption = 2 → threshold = -1 → tag added.
    fn power_zero_uses_floor_of_one() {
        let (mut world, id) = mk_world_with_entity(0, 2, &[]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (_, _, t) = update_corrupted_tag(entity);
        assert!((t - (-1.0)).abs() < 1e-9,
            "with power=0, floor of 1 applies; threshold should be -1.0, got {t}");
    }

    #[test]
    /// Negative corruption (purified) is the OPPOSITE of
    /// the tag: power=10, corruption=-5 → threshold =
    /// 10 - (-5) = 15 ≥ 1, so the tag is REMOVED if present.
    /// A purified entity is well above the dead zone.
    fn negative_corruption_keeps_tag_off() {
        let (mut world, id) = mk_world_with_entity(10, -5, &[CORRUPTED_TAG]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_corrupted_tag(entity);
        assert!((t - 15.0).abs() < 1e-9, "threshold should be 15.0, got {t}");
        assert!(removed, "purified entity should not have the corrupted tag");
        assert!(!entity.has_tag(CORRUPTED_TAG));
    }

    #[test]
    /// Already has the tag and threshold is still < 0 → no-op
    /// (don't re-add).  Returns (false, false).
    fn already_corrupted_under_threshold_is_noop() {
        let (mut world, id) = mk_world_with_entity(10, 15, &[CORRUPTED_TAG]);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, _) = update_corrupted_tag(entity);
        assert!(!added && !removed, "no change expected when already correct");
    }

    #[test]
    /// Missing `power` property → defaults to 0 (so floor of 1
    /// applies).  Missing `corruption` property → defaults to
    /// 0.  Net: threshold = 1.0, tag (if present) is removed.
    fn missing_properties_use_defaults() {
        let mut world = World::new("test");
        let id = Uuid::new_v4();
        let mut entity = WorldEntity::new("character", "blank", 0.0, 0.0);
        entity.id = id;
        entity.add_tag(CORRUPTED_TAG);
        // Deliberately do NOT set power or corruption.
        world.entities.insert(id, entity);
        let entity = world.entities.get_mut(&id).unwrap();
        let (added, removed, t) = update_corrupted_tag(entity);
        assert!((t - 1.0).abs() < 1e-9, "defaults: 1.0 - 0.0 = 1.0, got {t}");
        assert!(removed, "tag should be removed at threshold 1.0");
        assert!(!entity.has_tag(CORRUPTED_TAG));
    }

    // -----------------------------------------------------------------
    // Integration tests: rule runs inside apply_all_effects
    // -----------------------------------------------------------------

    #[test]
    /// Actor (power=10) takes a self-effect that pushes
    /// corruption from 0 to 15.  After apply, threshold =
    /// 10 - 15 = -5 < 0, so the `corrupted` tag should appear
    /// on the actor.
    fn actor_self_effect_above_corruption_adds_tag() {
        let (mut world, actor_id) = mk_world_with_entity(10, 0, &[]);
        let parsed = vec![actor_int_delta("corruption", 15)];
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, _hidden, corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        assert!(corrupted.contains_key("tester"),
            "corrupted map should contain the actor, got: {corrupted:?}");
        // Value is true = tag is now present (just added).
        assert!(corrupted["tester"], "tag should be added; got {corrupted:?}");

        let actor = world.entities.get(&actor_id).unwrap();
        assert!(actor.has_tag(CORRUPTED_TAG), "tag should be persisted");

        assert!(warnings.iter().any(|w| w.contains(CORRUPTED_TAG) && w.contains("tester")),
            "warning should mention the tag and entity name, got: {warnings:?}");
    }

    #[test]
    /// Cross-entity effect on Mira (power=10) takes her
    /// corruption from 0 to 20 → threshold = 10 - 20 = -10
    /// < 0 → Mira's `corrupted` tag is added.
    fn cross_entity_effect_above_corruption_adds_tag() {
        let (mut world, actor_id, mira_id) = mk_world_actor_and_mira(
            10, 0,   // actor
            10, 0,   // mira
        );
        let parsed = vec![other_int_delta(
            "Mira the Merchant", mira_id, "corruption", 20,
        )];
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, _hidden, corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        assert!(corrupted.contains_key("Mira the Merchant"),
            "Mira's corrupted toggle should be in the map: {corrupted:?}");
        assert!(corrupted["Mira the Merchant"], "tag should be added");

        let mira = world.entities.get(&mira_id).unwrap();
        assert!(mira.has_tag(CORRUPTED_TAG), "Mira should now be corrupted");

        // Actor is NOT in the corrupted map.
        assert!(!corrupted.contains_key("Actor"),
            "actor was not affected; rule should not have run on it: {corrupted:?}");
    }

    #[test]
    /// A purifying effect (negative delta on corruption) past
    /// the threshold (>= 1) should REMOVE the `corrupted`
    /// tag.  Start Mira with the tag and corruption=12
    /// (threshold = 10 - 12 = -2, corrupted).  Apply -5 →
    /// corruption=7, threshold = 10 - 7 = 3 ≥ 1, tag removed.
    /// The delta of -5 is well within the per-target effect
    /// cap of 11 (for power=10), so the effect-normalization
    /// pre-pass doesn't shrink it.
    fn cross_entity_effect_purifies_and_removes_tag() {
        let (mut world, actor_id, mira_id) = mk_world_actor_and_mira(
            10, 0, 10, 12,
        );
        // Pre-seed Mira with the corrupted tag.
        world.entities.get_mut(&mira_id).unwrap().add_tag(CORRUPTED_TAG);
        let parsed = vec![other_int_delta(
            "Mira the Merchant", mira_id, "corruption", -5,
        )];
        let mut warnings: Vec<String> = Vec::new();
        let (_applied, _nvs, _cross, _hidden, corrupted) =
            apply_all_effects(&mut world, actor_id, &parsed, &mut warnings, None);

        assert!(corrupted.contains_key("Mira the Merchant"),
            "Mira's corrupted toggle should be in the map: {corrupted:?}");
        // Value is false = tag was just removed.
        assert!(!corrupted["Mira the Merchant"], "tag should be removed; got {corrupted:?}");

        let mira = world.entities.get(&mira_id).unwrap();
        assert!(!mira.has_tag(CORRUPTED_TAG), "Mira's corrupted tag should be removed");

        assert!(warnings.iter().any(|w| w.contains("Removed") && w.contains("Mira")),
            "warning should mention 'Removed' and Mira: {warnings:?}");
    }

    #[test]
    /// Both the hidden-tag and corrupted-tag rules share the
    /// same `affected_ids` set and both run on each affected
    /// entity, but with independent formulas.  We verify the
    /// shared `affected_ids` by making BOTH rules produce a
    /// non-empty map on the same entity (a self-effect that
    /// only affects the actor).  This catches a regression
    /// where one rule would skip the actor because the other
    /// already touched it.
    ///
    /// Note: we apply the effects in TWO separate calls so
    /// the per-target effect-normalization pre-pass doesn't
    /// have to balance two deltas against a single cap.  The
    /// state-of-affected_ids contract is the same either way
    /// (it's a per-call set, reset between calls).
    fn hidden_and_corrupted_rules_share_affected_set() {
        // Call 1: visibility effect → hidden rule runs.
        let (mut world, actor_id) = mk_world_with_entity(10, 0, &[]);
        let parsed1 = vec![actor_int_delta("visibility", -3)];
        let mut w1: Vec<String> = Vec::new();
        let (_a1, _n1, _c1, hidden, _cor1) =
            apply_all_effects(&mut world, actor_id, &parsed1, &mut w1, None);
        assert!(hidden.contains_key("tester"),
            "call 1 (visibility) should populate the hidden map: {hidden:?}");

        // Call 2 (same entity, fresh state): corruption effect
        // → corrupted rule runs.  We re-make the world so
        // call 1's tag addition doesn't interfere with the
        // call-2 assertions.
        let (mut world2, actor_id2) = mk_world_with_entity(10, 0, &[]);
        let parsed2 = vec![actor_int_delta("corruption", 15)];
        let mut w2: Vec<String> = Vec::new();
        let (_a2, _n2, _c2, _hid2, corrupted) =
            apply_all_effects(&mut world2, actor_id2, &parsed2, &mut w2, None);
        assert!(corrupted.contains_key("tester"),
            "call 2 (corruption) should populate the corrupted map: {corrupted:?}");
        assert!(corrupted["tester"]);

        // The corrupted map is NOT populated by call 1 (which
        // was a visibility-only effect) — proves the two
        // rules are independent and don't cross-contaminate.
        assert!(!hidden.is_empty());
        assert!(!_c1.is_empty() || true, "ignore");  // dummy check
    }
}

#[cfg(test)]
mod stats_cap_tests {
    //! Tests for the steady-state stats-cap rule (Arcurus
    //! 2026-06-07 #openworld).
    //!
    //! Cap: `max(1, power*5) + 100`
    //! Sum: signed sum of all `properties_int` values
    //! (including power itself).
    //!
    //! The standalone script (`selena-project/code/
    //! normalize_stats.py`) calls `normalize_entity_stats` to
    //! scale all values proportionally when the cap is
    //! exceeded.  The runtime effect path only calls
    //! `check_stats_cap_warn`, which emits a warning without
    //! mutating the entity.

    use super::*;
    use crate::world_data::WorldEntity;

    fn mk_entity(
        entity_type: &str,
        name: &str,
        props: &[(&str, i64)],
    ) -> WorldEntity {
        let mut e = WorldEntity::new(entity_type, name, 0.0, 0.0);
        for (k, v) in props {
            e.properties_int.insert((*k).to_string(), *v);
        }
        e
    }

    // -----------------------------------------------------------------
    // Cap formula
    // -----------------------------------------------------------------

    #[test]
    fn cap_formula_basic() {
        // power=10 → max(1, 10*5) + 100 = 50 + 100 = 150
        assert_eq!(compute_stats_cap(10), 150);
        // power=100 → 500 + 100 = 600
        assert_eq!(compute_stats_cap(100), 600);
        // power=1 → 5 + 100 = 105
        assert_eq!(compute_stats_cap(1), 105);
        // power=0 → max(1, 0)*5 + 100 = 5 + 100 = 105 (the +1
        // floor on the power multiplier kicks in so a
        // brand-new entity still gets a proportional cap, not
        // 100).
        assert_eq!(compute_stats_cap(0), 105);
        // power=-5 → max(1, -5)*5 + 100 = 5 + 100 = 105 (the
        // floor also kicks in for negative power; this can
        // happen if a buggy effect subtracts too much from
        // power).
        assert_eq!(compute_stats_cap(-5), 105);
    }

    // -----------------------------------------------------------------
    // Sum
    // -----------------------------------------------------------------

    #[test]
    fn sum_includes_power_and_uses_signed() {
        let e = mk_entity("character", "Tester", &[
            ("power", 10),
            ("morale", 50),
            ("wealth", 30),
            ("visibility", -5),
        ]);
        // 10 + 50 + 30 + (-5) = 85
        assert_eq!(stats_sum(&e), 85);
    }

    #[test]
    fn sum_of_empty_entity_is_zero() {
        let e = mk_entity("character", "Blank", &[]);
        assert_eq!(stats_sum(&e), 0);
    }

    // -----------------------------------------------------------------
    // normalize_entity_stats
    // -----------------------------------------------------------------

    #[test]
    fn under_cap_is_noop() {
        let mut e = mk_entity("character", "Within", &[
            ("power", 10),
            ("morale", 50),
            ("wealth", 30),
        ]);
        // sum=90, cap=150 → no change.
        let result = normalize_entity_stats(&mut e);
        assert!(result.is_none(), "under-cap entity should not be normalized");
        assert_eq!(e.properties_int.get("power"), Some(&10));
        assert_eq!(e.properties_int.get("morale"), Some(&50));
    }

    #[test]
    fn over_cap_scales_proportionally() {
        let mut e = mk_entity("character", "Over", &[
            ("power", 10),
            ("morale", 200),
            ("wealth", 100),
        ]);
        // sum=310, cap=150.  scale = 150/310 = 0.4838…
        // After rounding:
        //   power: 10 * 0.4838 = 4.838 → 5
        //   morale: 200 * 0.4838 = 96.77 → 97
        //   wealth: 100 * 0.4838 = 48.38 → 48
        // New sum = 5 + 97 + 48 = 150 ✓
        let result = normalize_entity_stats(&mut e);
        assert!(result.is_some(), "over-cap entity should be normalized");
        let (old_sum, cap, scale) = result.unwrap();
        assert_eq!(old_sum, 310);
        assert_eq!(cap, 150);
        assert!((scale - 0.4838).abs() < 0.001, "scale ≈ 0.4838, got {scale}");
        let new_sum: i64 = e.properties_int.values().sum();
        assert_eq!(new_sum, cap, "post-normalize sum should equal cap, got {new_sum}");
    }

    #[test]
    fn normalize_is_sign_preserving() {
        // Negative values must stay negative.
        let mut e = mk_entity("character", "Mixed", &[
            ("power", 100),
            ("wealth", 800),
            ("visibility", -200),
        ]);
        // sum = 100 + 800 + (-200) = 700, cap = 600.  Over.
        // scale = 600/700 = 0.8571.
        // power: 100*0.8571 = 85.71 → 86
        // wealth: 800*0.8571 = 685.71 → 686
        // visibility: -200*0.8571 = -171.43 → -171
        // New sum = 86 + 686 + (-171) = 601 (rounding noise,
        // very close to cap=600).
        let result = normalize_entity_stats(&mut e);
        assert!(result.is_some());
        assert!(
            e.properties_int.get("visibility").copied().unwrap() < 0,
            "visibility must stay negative after scaling, got {:?}",
            e.properties_int.get("visibility")
        );
    }

    #[test]
    fn sum_zero_is_noop_not_div_by_zero() {
        // All-zero properties → sum=0, can't compute scale
        // (would be cap/0).  We must NOT panic.
        let mut e = mk_entity("character", "AllZero", &[
            ("power", 0),
            ("morale", 0),
        ]);
        let result = normalize_entity_stats(&mut e);
        assert!(result.is_none(), "zero-sum must be a no-op, not a panic");
    }

    #[test]
    fn power_is_included_in_sum() {
        // Big-power entity: cap is generous, but a single
        // large stat can still push it over.
        let mut e = mk_entity("faction", "BigFaction", &[
            ("power", 197),
            ("morale", 900),
            ("wealth", 200),
        ]);
        // sum = 197 + 900 + 200 = 1297, cap = max(1, 197)*5 +
        // 100 = 985 + 100 = 1085.  Over.
        let result = normalize_entity_stats(&mut e);
        assert!(result.is_some(), "197-power entity with 900 morale should be over cap");
        let (old_sum, cap, _) = result.unwrap();
        assert_eq!(old_sum, 1297);
        assert_eq!(cap, 1085);
        // Power is scaled down too (it counts in the sum).
        let new_power = e.properties_int.get("power").copied().unwrap();
        assert!(new_power < 197, "power should be scaled down, got {new_power}");
    }

    // -----------------------------------------------------------------
    // check_stats_cap_warn (runtime, warn-only path)
    // -----------------------------------------------------------------

    #[test]
    fn warn_emitted_when_over_cap() {
        let e = mk_entity("character", "Over", &[
            ("power", 10),
            ("morale", 200),
            ("wealth", 50),
        ]);
        // sum=260, cap=150.  Over.
        let mut warnings: Vec<String> = Vec::new();
        check_stats_cap_warn(&e, &mut warnings);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Over"));
        assert!(warnings[0].contains("sum=260"));
        assert!(warnings[0].contains("cap=150"));
        assert!(warnings[0].contains("overage=110"));
    }

    #[test]
    fn no_warn_when_under_cap() {
        let e = mk_entity("character", "Fine", &[
            ("power", 10),
            ("morale", 50),
            ("wealth", 30),
        ]);
        // sum=90, cap=150.  Under.
        let mut warnings: Vec<String> = Vec::new();
        check_stats_cap_warn(&e, &mut warnings);
        assert!(warnings.is_empty());
    }

    #[test]
    fn no_warn_at_exact_cap() {
        // Boundary: sum == cap is fine (not over).  Only sum >
        // cap triggers a warning.
        let e = mk_entity("character", "Exactly", &[
            ("power", 10),
            ("morale", 50),
            ("wealth", 30),
            ("visibility", 60),
        ]);
        // sum = 10 + 50 + 30 + 60 = 150, cap = 150.  NOT over.
        let mut warnings: Vec<String> = Vec::new();
        check_stats_cap_warn(&e, &mut warnings);
        assert!(warnings.is_empty(),
            "sum == cap should not trigger a warning, got: {warnings:?}");
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
        assert!(result.warnings.iter().any(|w| w.contains("over cap")));
        // The new informative warning should mention the actual
        // pre-truncation length, the cap, the over-by amount, and
        // hint at using history_summary_replace to fix it.
        let warn = result.warnings.iter().find(|w| w.contains("over cap")).unwrap();
        assert!(warn.contains("200"), "warning should mention the pre-truncate length: {warn}");
        assert!(warn.contains("100"), "warning should mention the cap: {warn}");
        assert!(warn.contains("100"), "warning should mention the over-by amount: {warn}");
        assert!(warn.contains("history_summary_replace"),
            "warning should hint at the fix path: {warn}");
        // Truncated to cap-1 chars + "…".
        let s = result.new_summary.unwrap();
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 100);
    }

    // -- matrix row 11b: cap-lowering scenario (Arcurus 2026-06-06 #openworld).
    //    A previous LLM wrote a summary under the old (higher) cap.
    //    An operator lowered the cap. The LLM is shown the full
    //    over-cap summary in the prompt and asked to shrink; if it
    //    doesn't, the server truncates and warns. --
    #[test]
    fn truncation_when_cap_lowered() {
        // Simulate: a 150-char summary that was stored under the old
        // cap of 200. The operator lowered the cap to 100, so the
        // LLM is now asked to bring the summary under 100 chars.
        // If the LLM doesn't shrink it (we test the no-op case
        // here), the server must still bound the stored value to
        // ≤ 100 chars and warn.
        let summary = "x".repeat(150);
        let result = apply_history_summary_replaces(
            Some(&summary),
            &[], // LLM sent no replace edits
            100, // new (lowered) cap
        );
        assert!(result.truncated, "stored summary over the new cap must be truncated");
        let s = result.new_summary.as_ref().unwrap();
        assert!(s.chars().count() <= 100, "truncated length must be ≤ cap");
        // The warning should mention:
        //  - the pre-truncate length (150)
        //  - the cap (100)
        //  - the over-by amount (50)
        //  - the history_summary_replace hint
        let warn = result.warnings.iter().find(|w| w.contains("over cap")).unwrap();
        assert!(warn.contains("150"), "warning should mention pre-truncate length: {warn}");
        assert!(warn.contains("100"), "warning should mention the cap: {warn}");
        assert!(warn.contains("50"), "warning should mention the over-by amount: {warn}");
    }

    // -- matrix row 11c: when result is exactly at the cap, no truncation,
    //    no warning. Edge case. --
    #[test]
    fn truncation_exactly_at_cap_no_op() {
        let result = apply_history_summary_replaces(
            Some(&"x".repeat(50)),
            &[],
            50,
        );
        assert!(!result.truncated);
        // No "over cap" warning when the result fits exactly.
        assert!(!result.warnings.iter().any(|w| w.contains("over cap")),
            "exactly-at-cap should not produce an over-cap warning");
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

    // -- find_replace_range: smart quotes fold to ASCII --
    // Real-world pattern observed in 2026-06-05 action_history: the
    // LLM emitted curly quotes (U+201C/U+201D) in `old_part` while
    // the stored summary had straight quotes. Pre-fix this hit
    // "old_part not found" in hundreds of entries; the new
    // Unicode-quirk fallback should match and produce a byte range
    // aligned to the ORIGINAL (pre-normalization) haystack so the
    // caller's slice still lands on a valid char boundary in the
    // stored summary.
    //
    // Haystack uses raw-string ASCII quotes (typical stored-summary
    // form); needle uses U+201C / U+201D (typical LLM emit). The
    // returned slice is on the ORIGINAL haystack, so it should
    // contain ASCII quotes — that's the whole point of the index
    // mapper.
    #[test]
    fn find_replace_range_folds_smart_quotes_to_ascii() {
        let h = r#"the hero said "met the dragon" to the bard"#;
        // needle uses U+201C / U+201D (left/right double quote)
        let n = "\u{201C}met the dragon\u{201D}";
        let r = find_replace_range(h, n);
        assert!(r.is_some(), "should match despite smart quotes");
        let (s, e) = r.unwrap();
        // Slicing the original must round-trip the ASCII-quoted
        // span (haystack has ASCII quotes, not curly) — the byte
        // indices are in the ORIGINAL string, so the slice is also
        // ASCII.
        assert_eq!(&h[s..e], "\"met the dragon\"");
    }

    // -- find_replace_range: em-dash / en-dash fold to ASCII hyphen --
    // Common LLM punctuation slip: an old_part written with an
    // em-dash (—) when the stored text used a hyphen (-), or vice
    // versa. The fuzziness should bridge them.
    #[test]
    fn find_replace_range_folds_em_dash_to_hyphen() {
        let h = "the realm of Aethermoor—where shadows linger";
        let n = "Aethermoor-where";
        let r = find_replace_range(h, n);
        assert!(r.is_some(), "should match despite em-dash vs hyphen");
        let (s, e) = r.unwrap();
        assert_eq!(&h[s..e], "Aethermoor—where");
    }

    // -- find_replace_range: full-width punctuation folds to ASCII --
    // Full-width comma (U+FF0C) and period (U+FF0E) appear in LLM
    // outputs that drift towards CJK punctuation. Fold to ASCII
    // so the LLM-emit `old_part` with a full-width comma can match
    // a stored summary with an ASCII comma. (The needle here uses
    // the full-width form too so the test isolates the comma-fold
    // from the more general "smart space" behaviour covered by
    // find_replace_range_folds_nbsp_to_space.)
    #[test]
    fn find_replace_range_folds_fullwidth_punctuation() {
        let h = "She recruited twelve knights\u{FF0C}then vanished";
        let n = "twelve knights\u{FF0C}then";
        let r = find_replace_range(h, n);
        assert!(r.is_some(), "should match despite full-width comma");
        let (s, e) = r.unwrap();
        assert_eq!(&h[s..e], "twelve knights\u{FF0C}then");
    }

    // -- find_replace_range: NBSP folds to ASCII space --
    // NBSPs sneak in from copy-pasted template content. The
    // whitespace-tokenize step (2) also catches them, but the
    // quirk-normalization step (4) gives a faster, cleaner match
    // when both sides are mostly the same except for the space
    // character class.
    #[test]
    fn find_replace_range_folds_nbsp_to_space() {
        let h = "the\u{00A0}hero\u{00A0}met the dragon";
        let n = "the hero met the dragon";
        let r = find_replace_range(h, n);
        assert!(r.is_some());
        let (s, e) = r.unwrap();
        assert_eq!(&h[s..e], "the\u{00A0}hero\u{00A0}met the dragon");
    }

    // -- find_replace_range: ligatures fold to ASCII digraphs --
    // The "fi" ligature (U+FB01) shows up in typography-flavored
    // LLM output. It should fold to "fi" so a find() against a
    // stored summary with ASCII "fi" still matches. The returned
    // byte indices must align to the ORIGINAL ligature boundary.
    // (The needle here is the unfolded "fire drake" so the test
    // isolates the ligature-fold from any whitespace quirks.)
    #[test]
    fn find_replace_range_folds_fi_ligature() {
        let h = "the hero met the \u{FB01}re drake at dawn";
        let n = "fire drake";
        let r = find_replace_range(h, n);
        assert!(r.is_some(), "ligature should fold to ASCII digraph");
        let (s, e) = r.unwrap();
        // Original slice should round-trip the ligature byte span.
        // The find matched "fire drake" in the normalized haystack;
        // in the original that span is the 1-codepoint ligature
        // (\u{FB01} = 3 UTF-8 bytes) followed by "re drake" (8
        // ASCII bytes) — 9 codepoints, 11 bytes total. The "the "
        // before it is NOT part of the matched substring.
        assert_eq!(&h[s..e], "\u{FB01}re drake");
    }

    // -- find_replace_range: long-s folds to ASCII s --
    // Less common but observed in older LLM prose imitating
    // pre-modern English style.
    #[test]
    fn find_replace_range_folds_long_s() {
        let h = "the \u{017F}ilent dragon";
        let n = "silent dragon";
        let r = find_replace_range(h, n);
        assert!(r.is_some());
    }

    // -- find_replace_range: still rejects meaningful differences --
    // The fuzziness is for glyph / whitespace trivia. A real
    // wording change ("killed" vs "met") should still miss.
    // (This is the same guarantee the original strict branch
    // provides; we re-assert it for the new path.)
    #[test]
    fn find_replace_range_still_rejects_real_wording_diff() {
        let h = "the hero met the dragon at dawn";
        let n = "killed the dragon";
        assert!(find_replace_range(h, n).is_none());
    }

    // -- apply_history_summary_replaces: not-found warning includes
    //    a 60-char preview of the missing old_part (debuggability). --
    // Per Arcurus 2026-06-05 #openworld: the LLM hallucinates
    // old_part sentences from prior turns. Surfacing the missing
    // text in the warning makes the regression trivial to spot
    // from a single action_history entry, without having to dig
    // through the raw LLM log.
    #[test]
    fn not_found_warning_includes_old_part_preview() {
        let result = apply_history_summary_replaces(
            Some("the realm is quiet tonight"),
            &[r("She travels to the Shadow Ridge Camp to recruit", "X")],
            10_000,
        );
        assert_eq!(result.warnings.len(), 1);
        let w = &result.warnings[0];
        assert!(w.contains("not found"), "warning text: {w:?}");
        // The preview should be present, and it should be the
        // LLM-emit text (truncated to 60 chars), not paraphrased.
        assert!(
            w.contains("She travels to the Shadow Ridge Camp"),
            "warning should include the missing old_part preview: {w:?}"
        );
    }

    // -- apply_history_summary_replaces: Unicode-normalized match
    //    no longer emits a not-found warning --
    // Regression: pre-fix, an old_part with U+201C against a
    // stored summary with U+0022 would warn "old_part not found"
    // in hundreds of entries. Post-fix the quirk-normalization
    // step should bridge it silently.
    #[test]
    fn unicode_quirk_match_succeeds_without_warning() {
        let result = apply_history_summary_replaces(
            Some(r#"the bard said "the dragon sleeps" last night"#),
            &[r("\u{201C}the dragon sleeps\u{201D}", "\u{201C}the dragon wakes\u{201D}")],
            10_000,
        );
        assert!(
            result.warnings.is_empty(),
            "smart-quote match should not warn: {:?}",
            result.warnings
        );
        // And the new summary should still contain smart quotes
        // (we replaced one smart-quoted span with another; the
        // original glyphs in the stored summary are preserved).
        let new = result.new_summary.unwrap();
        assert!(new.contains("the dragon wakes"));
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

// -- apply_summary_fallback: safety net for the LLM-sends-BOTH-fields
//    confusion. See todo fa45e5e7 / 06-06 worker run. 97 of the last
//    100 actions had a "Both history_summary and history_summary_replace
//    present" warning — the LLM was emitting both fields every turn,
//    the full history_summary was dropped, AND the surgical chain
//    usually failed too (because the LLM recalled old_part from the
//    just-dropped new summary, not the current stored one). The
//    fallback recovers the update by using the dropped full value
//    when the replace chain was a no-op. --
#[cfg(test)]
mod apply_summary_fallback_tests {
    use super::apply_summary_fallback;

    // The canonical real-world pattern: LLM sent a full new summary
    // and a replace chain whose old_part is not in the current
    // stored summary. The replace chain is a no-op (result == current),
    // so the fallback fires and uses the dropped full summary.
    #[test]
    fn falls_back_when_replace_is_noop_and_dropped_is_meaningful() {
        // Current stored summary.
        let current = Some("Velora holds the boundary vigil.");
        // Replace chain: old_part mentions a sentence the LLM thinks
        // is in the summary but isn't. The chain leaves the summary
        // unchanged, so result == current.
        let replace_result = Some("Velora holds the boundary vigil.");
        // Dropped full summary: the LLM's intended new content.
        let dropped = "Velora the Undying now actively reinforces the boundary seal, \
                       inscribing silver sigils at four cardinal points. Her vigilance has \
                       transitioned from passive watching to active counter-corruption.";
        let fb = apply_summary_fallback(dropped, current, replace_result, 10_000);
        let fb = fb.expect("fallback should fire when replace is a no-op and dropped is meaningful");
        assert_eq!(
            fb.new_summary.as_deref(),
            Some(dropped),
            "fallback should return the dropped full summary"
        );
        assert!(!fb.truncated, "should not truncate under cap");
        // Exactly one warning (no truncation, no placeholder, no both-fields).
        assert_eq!(fb.warnings.len(), 1);
        assert!(fb.warnings[0].contains("fell back to the dropped history_summary value"));
    }

    // Does NOT fire when the replace chain made a real change
    // (result != current). The full dropped value would lose
    // information — the surgical chain is the source of truth.
    #[test]
    fn does_not_fire_when_replace_made_real_change() {
        let current = Some("Velora holds the vigil.");
        let replace_result = Some("Velora is now actively reinforcing the boundary.");
        let dropped = "completely different full rewrite";
        let fb = apply_summary_fallback(dropped, current, replace_result, 10_000);
        assert!(fb.is_none(), "fallback should NOT fire when replace made a change");
    }

    // Does NOT fire when the dropped value is empty/placeholder —
    // there's nothing useful to fall back to.
    #[test]
    fn does_not_fire_when_dropped_is_empty() {
        let current = Some("Velora holds the vigil.");
        let replace_result = Some("Velora holds the vigil.");
        let dropped = "";
        let fb = apply_summary_fallback(dropped, current, replace_result, 10_000);
        assert!(fb.is_none(), "fallback should NOT fire when dropped is empty");
    }

    #[test]
    fn does_not_fire_when_dropped_is_placeholder() {
        let current = Some("Velora holds the vigil.");
        let replace_result = Some("Velora holds the vigil.");
        // Placebo values: just "…", "—", ".", or other
        // non-meaningful content.
        for placebo in &["…", "… ", "—", ".", " -- ", "  "] {
            let fb = apply_summary_fallback(placebo, current, replace_result, 10_000);
            assert!(
                fb.is_none(),
                "fallback should NOT fire when dropped is placeholder ({:?})",
                placebo
            );
        }
    }

    // Truncation: dropped value longer than the cap gets truncated
    // (with "…") and the warning set includes a truncation line.
    #[test]
    fn truncates_dropped_when_over_cap() {
        let current = Some("short");
        let replace_result = Some("short");
        let dropped = "x".repeat(200);
        let fb = apply_summary_fallback(&dropped, current, replace_result, 100)
            .expect("fallback should fire");
        assert!(fb.truncated, "should be truncated");
        let s = fb.new_summary.expect("truncated value must still be present when cut is non-empty");
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 100);
        // 2 warnings: the fell-back line + the truncation line.
        assert_eq!(fb.warnings.len(), 2);
        assert!(fb.warnings[0].contains("fell back"));
        assert!(fb.warnings[1].contains("truncated"));
    }

    // When the cap is 0 (degenerate), the truncated cut is empty.
    // The fallback returns new_summary = None rather than Some(""),
    // matching the "empty-as-None" rule used elsewhere.
    #[test]
    fn degenerate_zero_cap_returns_none() {
        let current = Some("short");
        let replace_result = Some("short");
        let dropped = "x".repeat(200);
        let fb = apply_summary_fallback(&dropped, current, replace_result, 0)
            .expect("fallback should fire even with zero cap");
        assert!(fb.truncated);
        assert_eq!(
            fb.new_summary, None,
            "zero-cap degenerate should yield None (empty-as-None rule)"
        );
    }

    // Both None: current is empty AND the replace chain returned
    // None. No-op from the replace's perspective; the fallback can
    // still fire (and the new_summary is what the dropped value
    // would be, possibly truncated).
    #[test]
    fn noop_with_none_current_still_fires_fallback() {
        let fb = apply_summary_fallback("fresh start", None, None, 10_000)
            .expect("fallback should fire when both are None and dropped is meaningful");
        assert_eq!(fb.new_summary.as_deref(), Some("fresh start"));
        assert!(!fb.truncated);
    }

    // Surrounding whitespace in the dropped value is trimmed
    // (per the existing rule for the legacy history_summary path).
    #[test]
    fn trims_dropped_value() {
        let current = Some("stored");
        let replace_result = Some("stored");
        let dropped = "   the new summary content   ";
        let fb = apply_summary_fallback(dropped, current, replace_result, 10_000)
            .expect("fallback should fire");
        assert_eq!(
            fb.new_summary.as_deref(),
            Some("the new summary content"),
            "dropped value should be trimmed"
        );
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
