mod world_data;
use crate::world_data::default_true;

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::io::Write;

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

use world_data::{World, WorldEntity, persistence::BinaryPersistence, entity_history::format_history_for_llm};

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
    
    fn log_llm(&mut self, context: &str, response: &str, time_ms: u64, success: bool, parsing_outcome: &str, extra: &str) {
        let timestamp = chrono_now_timestamp();
        let success_str = if success { "SUCCESS" } else { "FAILED" };
        let lines = format!(
            "[{}] === LLM Call ===\nSuccess: {}\nTime: {} ms\n--- Context ---\n{}\n--- Response ---\n{}\n--- Parsing ---\n{}\n--- Extra ---\n{}\n====================\n\n",
            timestamp, success_str, time_ms, context, response, parsing_outcome, extra
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.llm_file_path)
            .and_then(|mut f| f.write_all(lines.as_bytes()));
    }
}

fn chrono_now_date() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let secs = now.as_secs();
    let days = secs / 86400;
    // Unix epoch was Thursday (1970-01-01). Add days and offset to get YYYY-MM-DD.
    // Using a simple approach: convert to broken-down time
    let days_since_epoch = secs as i64;
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
        if remaining < dim[m] { break; }
        remaining -= dim[m];
        month = m + 1;
    }
    day = remaining as i64 + 1;
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn chrono_now_timestamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let secs = now.as_secs();
    let days_since_epoch = secs as i64;
    let mut year = 1970;
    let mut month = 1;
    let mut day = 1;
    let mut remaining = days_since_epoch;
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
        if remaining < dim[m] { break; }
        remaining -= dim[m];
        month = m + 1;
    }
    day = remaining as i64 + 1;
    let secs_of_day = secs % 86400;
    let hours = secs_of_day / 3600;
    let mins = (secs_of_day % 3600) / 60;
    let secs = secs_of_day % 60;
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", year, month, day, hours, mins, secs)
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

// ============================================================================
// World endpoints
// ============================================================================

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
    
    match world.get_entity(&id) {
        Some(entity) => success_json(EntityResponse {
            success: true,
            data: Some(entity.clone()),
            error: None,
        }).into_response(),
        None => error_json(StatusCode::NOT_FOUND, "Entity not found"),
    }
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
    
    // Get world stats for relative descriptions
    let stats = world.calculate_stats();
    let type_stats = stats.by_type.get(&entity.entity_type);
    
    // Build property context with relative descriptions
    let mut prop_context = String::new();
    for (key, value) in &entity.properties_int {
        let relative = if let Some(ts) = type_stats {
            if let Some(stat) = ts.properties_int.get(key) {
                World::get_relative_value(*value as f64, stat.min, stat.max, stat.avg)
            } else {
                "unknown"
            }
        } else {
            "unknown"
        };
        prop_context.push_str(&format!("  - {}: {} ({})\n", key, value, relative));
    }
    for (key, value) in &entity.properties_float {
        prop_context.push_str(&format!("  - {}: {:.2}\n", key, value));
    }
    
    // Build entity history context
    let entity_history_str = format_history_for_llm(&entity, &world.settings);
    
    // Build nearby entities context
    let nearby_entities = world.get_entities_in_radius(entity.x, entity.y, 150.0);
    let nearby_entities: Vec<_> = nearby_entities.iter().filter(|e| e.id != entity.id).collect();
    let nearby_entities_str = if nearby_entities.is_empty() {
        String::from("No other entities nearby.")
    } else {
        let mut s = String::new();
        for other in &nearby_entities {
            let dist = ((other.x - entity.x).powi(2) + (other.y - entity.y).powi(2)).sqrt();
            s.push_str(&format!("- **{}** ({}) - Distance: {:.1}\n", other.name, other.entity_type, dist));
            if !other.description.is_empty() {
                s.push_str(&format!("  {}\n", other.description));
            }
            // Show a few key properties
            let key_props: Vec<String> = other.properties_int.iter()
                .take(3)
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect();
            if !key_props.is_empty() {
                s.push_str(&format!("  Properties: {}\n", key_props.join(", ")));
            }
        }
        s
    };
    
    // Build power tier context - calculate based on key power properties
    let power_tier_str = {
        // Calculate total power from key properties
        let power_keys = ["power", "strength", "army_size", "wealth", "influence"];
        let mut total_power = 0i64;
        for key in &power_keys {
            if let Some(v) = entity.properties_int.get(*key) {
                total_power += v;
            }
        }
        // Add float properties that represent power
        for (_, v) in &entity.properties_float {
            if *v > 0.0 {
                total_power += *v as i64;
            }
        }
        // Determine tier based on total power
        if total_power >= 1000 {
            format!("Legendary (Power: {}) - Among the most powerful beings in the world", total_power)
        } else if total_power >= 500 {
            format!("Epic (Power: {}) - A formidable force to be reckoned with", total_power)
        } else if total_power >= 200 {
            format!("Rare (Power: {}) - Above average strength and influence", total_power)
        } else if total_power >= 50 {
            format!("Uncommon (Power: {}) - A competent and capable individual", total_power)
        } else {
            format!("Common (Power: {}) - An ordinary entity in the world", total_power)
        }
    };
    
    // Build world events context
    let world_events_str = if world.active_events.is_empty() {
        String::new()
    } else {
        let mut s = String::from("## Active World Events\n\n");
        for event in &world.active_events {
            if event.active {
                s.push_str(&format!("### {}\n{}", event.name, event.description));
                if !event.influence.is_empty() {
                    s.push_str(&format!("\n**How this affects entities:** {}", event.influence));
                }
                s.push_str("\n\n");
            }
        }
        s
    };
    
    // Read the AI template
    let template = match tokio::fs::read_to_string("ai_templates/EntityAction.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    
    // Build the prompt
    let prompt = template
        .replace("{world_name}", &state.world.read().await.name)
        .replace("{entity_name}", &entity.name)
        .replace("{entity_type}", &entity.entity_type)
        .replace("{description}", &entity.description)
        .replace("{tags}", &entity.tags.join(", "))
        .replace("{x}", &format!("{:.1}", entity.x))
        .replace("{y}", &format!("{:.1}", entity.y))
        .replace("{property_context}", &prop_context)
        .replace("{power_tier}", &power_tier_str)
        .replace("{entity_history}", &entity_history_str)
        .replace("{nearby_entities}", &nearby_entities_str)
        .replace("{world_events}", &world_events_str);
    
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
        "property_context": prop_context,
        "world_events": world_events_str,
        "prompt": prompt,
        "llm_configured": llm_configured
    })).into_response()
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
                &format!("LLM call for entity {} ({})", id, entity_name),
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
                            &format!("LLM call for entity {} ({})", id, entity_name),
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
                                &format!("LLM call for entity {} ({})", id, entity_name),
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
                        &format!("LLM call for entity {} ({})", id, entity_name),
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
                    &format!("LLM call for entity {} ({})", id, entity_name),
                    &raw_response,
                    elapsed_ms,
                    true,
                    "Success - response received",
                    &format!("Reasoning: {:?}", reasoning)
                );
            }
            
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
                    &format!("LLM call for entity {} ({})", id, entity_name),
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
    
    // Get world stats for relative descriptions
    let stats = world.calculate_stats();
    let type_stats = stats.by_type.get(&entity.entity_type);
    
    // Build property context with relative descriptions
    let mut prop_context = String::new();
    for (key, value) in &entity.properties_int {
        let relative = if let Some(ts) = type_stats {
            if let Some(stat) = ts.properties_int.get(key) {
                World::get_relative_value(*value as f64, stat.min, stat.max, stat.avg)
            } else {
                "unknown"
            }
        } else {
            "unknown"
        };
        prop_context.push_str(&format!("  - {}: {} ({})\n", key, value, relative));
    }
    for (key, value) in &entity.properties_float {
        prop_context.push_str(&format!("  - {}: {:.2}\n", key, value));
    }
    
    // Build entity history context
    let entity_history_str = format_history_for_llm(&entity, &world.settings);
    
    // Build nearby entities context
    let nearby_entities = world.get_entities_in_radius(entity.x, entity.y, 150.0);
    let nearby_entities: Vec<_> = nearby_entities.iter().filter(|e| e.id != entity.id).collect();
    let nearby_entities_str = if nearby_entities.is_empty() {
        String::from("No other entities nearby.")
    } else {
        let mut s = String::new();
        for other in &nearby_entities {
            let dist = ((other.x - entity.x).powi(2) + (other.y - entity.y).powi(2)).sqrt();
            s.push_str(&format!("- **{}** ({}) - Distance: {:.1}\n", other.name, other.entity_type, dist));
            if !other.description.is_empty() {
                s.push_str(&format!("  {}\n", other.description));
            }
            // Show a few key properties
            let key_props: Vec<String> = other.properties_int.iter()
                .take(3)
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect();
            if !key_props.is_empty() {
                s.push_str(&format!("  Properties: {}\n", key_props.join(", ")));
            }
        }
        s
    };
    
    // Build power tier context - calculate based on key power properties
    let power_tier_str = {
        // Calculate total power from key properties
        let power_keys = ["power", "strength", "army_size", "wealth", "influence"];
        let mut total_power = 0i64;
        for key in &power_keys {
            if let Some(v) = entity.properties_int.get(*key) {
                total_power += v;
            }
        }
        // Add float properties that represent power
        for (_, v) in &entity.properties_float {
            if *v > 0.0 {
                total_power += *v as i64;
            }
        }
        // Determine tier based on total power
        if total_power >= 1000 {
            format!("Legendary (Power: {}) - Among the most powerful beings in the world", total_power)
        } else if total_power >= 500 {
            format!("Epic (Power: {}) - A formidable force to be reckoned with", total_power)
        } else if total_power >= 200 {
            format!("Rare (Power: {}) - Above average strength and influence", total_power)
        } else if total_power >= 50 {
            format!("Uncommon (Power: {}) - A competent and capable individual", total_power)
        } else {
            format!("Common (Power: {}) - An ordinary entity in the world", total_power)
        }
    };
    
    // Build world events context
    let world_events_str = if world.active_events.is_empty() {
        String::new()
    } else {
        let mut s = String::from("## Active World Events\n\n");
        for event in &world.active_events {
            if event.active {
                s.push_str(&format!("### {}\n{}", event.name, event.description));
                if !event.influence.is_empty() {
                    s.push_str(&format!("\n**How this affects entities:** {}", event.influence));
                }
                s.push_str("\n\n");
            }
        }
        s
    };
    
    // Read the AI template
    let template = match tokio::fs::read_to_string("ai_templates/EntityAction.md").await {
        Ok(t) => t,
        Err(_) => "".to_string(),
    };
    
    // Build the prompt
    let prompt = template
        .replace("{world_name}", &state.world.read().await.name)
        .replace("{entity_name}", &entity.name)
        .replace("{entity_type}", &entity.entity_type)
        .replace("{description}", &entity.description)
        .replace("{tags}", &entity.tags.join(", "))
        .replace("{x}", &format!("{:.1}", entity.x))
        .replace("{y}", &format!("{:.1}", entity.y))
        .replace("{property_context}", &prop_context)
        .replace("{power_tier}", &power_tier_str)
        .replace("{entity_history}", &entity_history_str)
        .replace("{nearby_entities}", &nearby_entities_str)
        .replace("{world_events}", &world_events_str);
    
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
                "property_context": prop_context,
                "prompt": prompt,
                "llm_configured": false
            }
        })).into_response();
    }
    
    // Make LLM API call
    let api_url = &state.settings.llm.api_url;
    let model = &state.settings.llm.model;
    let api_key = api_key.unwrap();
    
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
                return success_json(serde_json::json!({
                    "success": false,
                    "error": format!("LLM API error: {} - {}", status, body),
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
                Ok(action_data) => {
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
                        
                        success_json(serde_json::json!({
                            "success": true,
                            "action": action_data.action,
                            "outcome": action_data.outcome,
                            "effects_applied": applied_effects,
                            "narrative": action_data.narrative
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

// Helper struct for parsing LLM response
#[derive(Debug, serde::Deserialize)]
struct LlmActionResponse {
    action: String,
    outcome: String,
    #[serde(default)]
    effects: std::collections::HashMap<String, serde_json::Value>,
    narrative: String,
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

// Parse the JSON response from LLM
fn parse_llm_action_response(raw: &str) -> Result<LlmActionResponse, String> {
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
    
    serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {} - Input: {}", e, json_str))
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
        Ok(action_data) => {
            // Apply effects to entity
            let mut world = state.world.write().await;
            if let Some(entity) = world.entities.get_mut(&entity_id) {
                let mut applied_effects = std::collections::HashMap::new();
                let mut new_values = std::collections::HashMap::new();
                let mut warnings: Vec<String> = Vec::new();
                
                for (prop_key, change_val) in &action_data.effects {
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
                        let new_val = old_val + val;
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
                
                // Log to LLM log
                let parsing_outcome = format!(
                    "Applied {} effects. Warnings: {:?}",
                    applied_effects.len(),
                    warnings
                );
                if let Ok(mut logger) = state.logger.lock() {
                    logger.ensure_today(&PathBuf::from("logs"));
                    logger.log_llm(
                        &format!("Process action for entity {}: {}", entity_id, action_data.action),
                        raw_response,
                        0,
                        true,
                        &parsing_outcome,
                        &format!("Effects: {:?}", action_data.effects)
                    );
                }
                
                let response_json = serde_json::json!({
                    "success": true,
                    "action": action_data.action,
                    "outcome": action_data.outcome,
                    "effects_applied": applied_effects,
                    "new_values": new_values,
                    "narrative": action_data.narrative,
                    "warnings": warnings,
                });
                
                success_json(response_json).into_response()
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
                logger.log_llm(
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
    let mut world = World::new(&req.name);
    
    // Generate sample entities if requested
    if req.generate_sample {
        use world_data::WorldEntity;
        
        // Oak Valley Village
        let mut village = WorldEntity::new("location", "Oak Valley Village", 150.0, 250.0);
        village.description = "A peaceful farming village.".to_string();
        village.add_tag("village");
        village.add_tag("peaceful");
        village.add_tag("farming");
        world.add_entity(village);
        
        // Shadow Ridge Camp
        let mut bandit = WorldEntity::new("location", "Shadow Ridge Camp", 280.0, 320.0);
        bandit.description = "Hidden bandit encampment.".to_string();
        bandit.add_tag("bandit");
        bandit.add_tag("dangerous");
        bandit.add_tag("mountain");
        bandit.set_int("power", 45);
        bandit.set_int("wealth", 200);
        bandit.set_int("black_mana", 80);
        world.add_entity(bandit);
        
        // Elder Moonthorn
        let mut elder = WorldEntity::new("character", "Elder Moonthorn", 145.0, 245.0);
        elder.description = "Wise guardian of the forest.".to_string();
        elder.add_tag("elf");
        elder.add_tag("wise");
        elder.add_tag("guardian");
        world.add_entity(elder);
        
        // Whisperwood Forest
        let mut forest = WorldEntity::new("location", "Whisperwood Forest", 140.0, 220.0);
        forest.description = "Ancient forest with strange magic.".to_string();
        forest.add_tag("forest");
        forest.add_tag("magical");
        forest.add_tag("ancient");
        world.add_entity(forest);
        
        // Silverstream Keep
        let mut keep = WorldEntity::new("location", "Silverstream Keep", 320.0, 180.0);
        keep.description = "Fortified castle overlooking the river.".to_string();
        keep.add_tag("castle");
        keep.add_tag("royal");
        keep.set_int("power", 100);
        keep.set_int("wealth", 500);
        world.add_entity(keep);
        
        // Ironforge Clan
        let mut clan = WorldEntity::new("faction", "Ironforge Clan", 420.0, 350.0);
        clan.description = "Mighty dwarven smiths and warriors.".to_string();
        clan.add_tag("dwarven");
        clan.add_tag("clan");
        clan.add_tag("smiths");
        world.add_entity(clan);
        
        // Mira the Merchant
        let mut merchant = WorldEntity::new("character", "Mira the Merchant", 200.0, 290.0);
        merchant.description = "Traveling merchant with exotic goods.".to_string();
        merchant.add_tag("merchant");
        merchant.add_tag("trader");
        world.add_entity(merchant);
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
                "entity_count": entity_count
            })).into_response()
        }
        Err(e) => error_json(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
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
        "password_var_name": password_var_name
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
    };
    
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
        // World management endpoints
        .route("/api/world/save", post(save_world_handler))
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
