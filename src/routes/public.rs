//! Public API on `:3001` (§7).

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::Value;
use utoipa::{IntoParams, OpenApi as _, ToSchema, openapi::OpenApi};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    client,
    error::{AppError, ErrorBody},
    extract::{Body, Path, Query},
    openapi::PublicDoc,
    resource::{Resource, valid_visitor_id},
    routes::nonempty,
    state::AppState,
    summary::{self, Summary},
    topics::{self, TopicMode, TopicStatus, valid_topic_name},
};

pub const MAX_BODY: usize = 8 * 1024;
pub const MAX_DATA: usize = 4 * 1024;
pub const MAX_TOGGLE_KEY: usize = 64;

/// Router (still needing state) and its OpenAPI document.
pub fn parts() -> (Router<AppState>, OpenApi) {
    OpenApiRouter::with_openapi(PublicDoc::openapi())
        .routes(routes!(post_event))
        .routes(routes!(get_summary))
        .split_for_parts()
}

pub fn router(state: AppState) -> Router {
    let (api, _) = parts();
    api.route("/health", get(health))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

/// Event to record.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventIn {
    /// Stable resource identifier (trimmed, no control characters). Takes precedence over `resourceUrl` for aggregation.
    #[schema(min_length = 1, max_length = 200, example = "post:123")]
    pub resource_id: Option<String>,
    /// Relative path (preferred: `location.pathname`) or absolute `http(s)` URL on an allowed host.
    /// Only the normalized path is stored. At least one of `resourceId` / `resourceUrl` is required.
    #[schema(max_length = 2048, example = "/blog/post-1/")]
    pub resource_url: Option<String>,
    /// Client-generated visitor id (no control characters).
    #[schema(
        min_length = 1,
        max_length = 64,
        example = "0b6a9a2e-6c1e-4d2b-9f1a-3c5e7d9b1a2f"
    )]
    pub visitor_id: String,
    /// Free-form JSON object, at most 4 KB serialized. `active`, if present, must be a boolean or `null`
    /// (`null` = `true`). For toggle topics `data[keyField]` is required: a string of 1..64 chars.
    #[serde(default)]
    #[schema(value_type = Option<Object>, example = json!({"type": "like", "active": true}))]
    pub data: Option<Value>,
    /// Honeypot: must be absent or empty. A non-empty value silently drops the event (`204`).
    #[serde(default)]
    #[schema(max_length = 0)]
    pub website: Option<String>,
}

#[derive(Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SummaryQuery {
    /// Same rules as on ingest.
    #[param(max_length = 200)]
    pub resource_id: Option<String>,
    /// Same normalization as on ingest.
    #[param(max_length = 2048)]
    pub resource_url: Option<String>,
    /// Without it, `mine` is `[]`.
    #[param(max_length = 64)]
    pub visitor_id: Option<String>,
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
}

/// Honeypot: anything but absent, `null` or `""` counts as filled.
fn honeypot_filled(v: &Value) -> bool {
    match v.get("website") {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Validates `data` for all topics: a JSON object (missing → `{}`), ≤ 4 KB, `active` boolean or null.
fn validate_data(data: Option<Value>) -> Result<Value, AppError> {
    let data = match data {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err(AppError::bad("data must be a JSON object")),
    };
    if serde_json::to_vec(&data).map_or(usize::MAX, |b| b.len()) > MAX_DATA {
        return Err(AppError::PayloadTooLarge("data exceeds 4 KB".into()));
    }
    if !matches!(
        data.get("active"),
        None | Some(Value::Null | Value::Bool(_))
    ) {
        return Err(AppError::bad("data.active must be a boolean"));
    }
    Ok(data)
}

fn validate_toggle_key(data: &Value, key_field: &str) -> Result<(), AppError> {
    match data.get(key_field) {
        Some(Value::String(s)) if (1..=MAX_TOGGLE_KEY).contains(&s.chars().count()) => Ok(()),
        _ => Err(AppError::bad(format!(
            "data.{key_field} must be a string of 1..{MAX_TOGGLE_KEY} chars"
        ))),
    }
}

/// Record an event.
///
/// Topics are created automatically (as `pending`) on first use. Processing order, first match
/// wins: missing `X-Real-IP` → 400, invalid topic name → 400, rate limit → 429, honeypot → 204,
/// body validation → 400/413/415, blocked topic → 204, topic-creation limits → 204, store.
#[utoipa::path(
    post,
    path = "/api/events/{topic}",
    tag = "events",
    params(
        ("topic" = String, Path, description = "Topic name", max_length = 50, pattern = "^[a-z0-9][a-z0-9_.-]{0,49}$"),
    ),
    request_body(content = EventIn, content_type = "application/json", description = "At most 8 KB"),
    responses(
        (status = 200, description = "Stored; the topic is approved and public, so its summary for this visitor is returned", body = Summary),
        (status = 204, description = "Stored (topic pending or private), or silently dropped (honeypot, blocked topic, topic-creation limits)"),
        (status = 400, description = "Missing/invalid `X-Real-IP`, invalid topic name, malformed JSON or invalid fields", body = ErrorBody),
        (status = 413, description = "Body over 8 KB or `data` over 4 KB", body = ErrorBody),
        (status = 415, description = "`Content-Type` is not `application/json`", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    ),
)]
pub async fn post_event(
    State(st): State<AppState>,
    Path(topic): Path<String>,
    headers: HeaderMap,
    Body(body): Body,
) -> Result<Response, AppError> {
    let ip = client::client_ip(&headers).ok_or_else(|| AppError::bad("missing X-Real-IP"))?;
    if !valid_topic_name(&topic) {
        return Err(AppError::bad("invalid topic name"));
    }
    let ck = client::client_key(ip);
    let cfg = &st.cfg;
    if !st
        .limiter
        .check_ingest(&ck, &topic, cfg.events_per_ip_min, cfg.events_per_min)
    {
        return Err(AppError::TooManyRequests);
    }

    if !is_json(&headers) {
        return Err(AppError::UnsupportedMediaType);
    }
    let raw: Value =
        serde_json::from_slice(&body).map_err(|e| AppError::bad(format!("invalid JSON: {e}")))?;
    if honeypot_filled(&raw) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let input: EventIn =
        serde_json::from_value(raw).map_err(|e| AppError::bad(format!("invalid body: {e}")))?;
    if !valid_visitor_id(&input.visitor_id) {
        return Err(AppError::bad("invalid visitorId"));
    }
    let data = validate_data(input.data)?;
    let resource = Resource::resolve(
        input.resource_id.as_deref(),
        input.resource_url.as_deref(),
        &cfg.allowed_hosts,
    )?;

    let existing = topics::get(&st.pool, &topic).await?;
    if let Some(t) = &existing
        && t.mode == TopicMode::Toggle
        && let Some(k) = &t.key_field
    {
        validate_toggle_key(&data, k)?;
    }

    let ip_hash = client::ip_hash(&cfg.ip_hash_salt, &ck);
    let t = match existing {
        Some(t) => t,
        None => match topics::auto_create(&st.pool, cfg, &topic, &ip_hash).await? {
            Some(t) => t,
            None => return Ok(StatusCode::NO_CONTENT.into_response()),
        },
    };
    if t.status == TopicStatus::Blocked {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    sqlx::query(
        "INSERT INTO events (topic, resource_id, resource_url, visitor_id, data, ip_hash) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&t.name)
    .bind(&resource.id)
    .bind(&resource.url)
    .bind(&input.visitor_id)
    .bind(&data)
    .bind(&ip_hash)
    .execute(&st.pool)
    .await?;

    if !t.is_published() {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let s = summary::compute(
        &st.pool,
        &t,
        &resource.key,
        Some(&input.visitor_id),
        cfg.toggle_top_n,
    )
    .await?;
    Ok(Json(s).into_response())
}

/// Summary for a resource.
///
/// Only for approved + public topics. Not rate limited.
#[utoipa::path(
    get,
    path = "/api/events/{topic}/summary",
    tag = "events",
    params(
        ("topic" = String, Path, description = "Topic name", max_length = 50, pattern = "^[a-z0-9][a-z0-9_.-]{0,49}$"),
        SummaryQuery,
    ),
    responses(
        (status = 200, description = "Summary of the resource", body = Summary),
        (status = 400, description = "Invalid topic name, resource or visitorId", body = ErrorBody),
        (status = 404, description = "Topic unknown, not approved, or not public", body = ErrorBody),
    ),
)]
pub async fn get_summary(
    State(st): State<AppState>,
    Path(topic): Path<String>,
    Query(q): Query<SummaryQuery>,
) -> Result<Json<Summary>, AppError> {
    if !valid_topic_name(&topic) {
        return Err(AppError::bad("invalid topic name"));
    }
    let resource = Resource::resolve(
        nonempty(&q.resource_id),
        nonempty(&q.resource_url),
        &st.cfg.allowed_hosts,
    )?;
    let visitor = nonempty(&q.visitor_id);
    if visitor.is_some_and(|v| !valid_visitor_id(v)) {
        return Err(AppError::bad("invalid visitorId"));
    }
    let t = topics::get(&st.pool, &topic)
        .await?
        .filter(|t| t.is_published())
        .ok_or(AppError::NotFound)?;
    let s = summary::compute(&st.pool, &t, &resource.key, visitor, st.cfg.toggle_top_n).await?;
    Ok(Json(s))
}

/// `200` if PG responds, otherwise `503`. Not part of the OpenAPI document.
async fn health(State(st): State<AppState>) -> StatusCode {
    match sqlx::query("SELECT 1").execute(&st.pool).await {
        Ok(_) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(error = %e, "health check failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
