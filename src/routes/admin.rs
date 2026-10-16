//! Admin API on `:3002`, behind a bearer token (§8).

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Row, postgres::PgRow};
use subtle::ConstantTimeEq;
use utoipa::{IntoParams, OpenApi as _, ToSchema, openapi::OpenApi};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    error::{AppError, ErrorBody},
    extract::{JsonBody, Path, Query},
    openapi::{self, AdminDoc},
    resource::{normalize_url, valid_visitor_id, validate_resource_id},
    routes::nonempty,
    state::AppState,
    topics::{TOPIC_COLS, Topic, TopicMode, TopicRow, TopicStatus, valid_key_field},
};

pub fn parts() -> (Router<AppState>, OpenApi) {
    OpenApiRouter::with_openapi(AdminDoc::openapi())
        .routes(routes!(list_topics))
        .routes(routes!(patch_topic, delete_topic))
        .routes(routes!(sample))
        .routes(routes!(feed, erase_visitor))
        .routes(routes!(public_spec))
        .routes(routes!(admin_spec))
        .split_for_parts()
}

pub fn router(state: AppState) -> Router {
    let (api, _) = parts();
    api.route_layer(middleware::from_fn_with_state(
        state.clone(),
        require_admin_token,
    ))
    .with_state(state)
}

async fn require_admin_token(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let authorized = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| bool::from(t.as_bytes().ct_eq(st.cfg.admin_token.as_bytes())));
    if authorized {
        next.run(req).await
    } else {
        AppError::Unauthorized.into_response()
    }
}

// ---- types ----

/// Topic with activity stats.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicListItem {
    #[serde(flatten)]
    pub topic: Topic,
    pub event_count: i64,
    pub last_event_at: Option<DateTime<Utc>>,
}

impl<'r> FromRow<'r, PgRow> for TopicListItem {
    fn from_row(row: &'r PgRow) -> sqlx::Result<Self> {
        Ok(Self {
            topic: TopicRow::from_row(row)?.try_into()?,
            event_count: row.try_get("event_count")?,
            last_event_at: row.try_get("last_event_at")?,
        })
    }
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TopicsQuery {
    /// Only topics with this status.
    pub status: Option<TopicStatus>,
    /// Sort by creation time (default `desc`, newest first).
    pub order: Option<SortOrder>,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    Desc,
}

impl SortOrder {
    fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// Partial update; absent fields are left unchanged.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TopicPatch {
    pub status: Option<TopicStatus>,
    pub mode: Option<TopicMode>,
    /// Field in `data` used as the toggle key. `null` clears it; absent leaves it unchanged.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>, max_length = 50, pattern = "^[A-Za-z0-9_]{1,50}$")]
    pub key_field: Option<Option<String>>,
    pub public: Option<bool>,
}

/// Absent → `None` (via `#[serde(default)]`), `null` → `Some(None)`, value → `Some(Some(v))`.
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SampleQuery {
    /// Number of latest events (default 10).
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<i64>,
}

#[derive(Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct FeedQuery {
    pub topic: Option<String>,
    #[param(max_length = 200)]
    pub resource_id: Option<String>,
    /// Normalized with the same rule as on ingest.
    #[param(max_length = 2048)]
    pub resource_url: Option<String>,
    /// Return events with `id > after` (default 0). Cursor for `order=asc`.
    #[param(minimum = 0)]
    pub after: Option<i64>,
    /// Return events with `id < before`. Cursor for `order=desc`.
    #[param(minimum = 1)]
    pub before: Option<i64>,
    /// Sort by `id` (default `asc`, oldest first; `desc` is newest first).
    pub order: Option<SortOrder>,
    /// Page size (default 100).
    #[param(minimum = 1, maximum = 1000)]
    pub limit: Option<i64>,
}

#[derive(Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct EraseQuery {
    /// Visitor whose events are deleted across all topics.
    #[param(min_length = 1, max_length = 64)]
    pub visitor_id: String,
}

/// Stored event.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct EventItem {
    pub id: i64,
    pub topic: String,
    pub resource_id: Option<String>,
    pub resource_url: Option<String>,
    pub visitor_id: String,
    #[schema(value_type = Object)]
    pub data: Value,
    pub created_at: DateTime<Utc>,
}

/// A page of the event feed.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    pub items: Vec<EventItem>,
    /// Cursor (`after`) for the next page with `order=asc`; `null` when fewer than `limit` items
    /// were returned or with `order=desc`.
    pub next_after: Option<i64>,
    /// Cursor (`before`) for the next page with `order=desc`; `null` when fewer than `limit` items
    /// were returned or with `order=asc`.
    pub next_before: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct Deleted {
    /// Number of deleted events.
    pub deleted: i64,
}

const EVENT_COLS: &str = "id, topic, resource_id, resource_url, visitor_id, data, created_at";

fn limit_in(v: Option<i64>, default: i64, max: i64) -> Result<i64, AppError> {
    let n = v.unwrap_or(default);
    if !(1..=max).contains(&n) {
        return Err(AppError::bad(format!("limit must be 1..{max}")));
    }
    Ok(n)
}

// ---- handlers ----

/// List topics.
#[utoipa::path(
    get,
    path = "/api/admin/topics",
    tag = "topics",
    params(TopicsQuery),
    responses(
        (status = 200, description = "Topics, newest first unless `order=asc`",body = Vec<TopicListItem>),
        (status = 400, description = "Invalid filter", body = ErrorBody),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
    ),
)]
pub async fn list_topics(
    State(st): State<AppState>,
    Query(q): Query<TopicsQuery>,
) -> Result<Json<Vec<TopicListItem>>, AppError> {
    let cols = TOPIC_COLS
        .split(", ")
        .map(|c| format!("t.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = sqlx::query_as::<_, TopicListItem>(&format!(
        "SELECT {cols}, COALESCE(e.n, 0) AS event_count, e.last AS last_event_at \
         FROM topics t \
         LEFT JOIN LATERAL ( \
           SELECT COUNT(*) AS n, MAX(created_at) AS last FROM events WHERE topic = t.name \
         ) e ON true \
         WHERE ($1::text IS NULL OR t.status = $1) \
         ORDER BY t.created_at {order}, t.name",
        order = q.order.unwrap_or(SortOrder::Desc).sql(),
    ))
    .bind(q.status.map(TopicStatus::as_str))
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

/// Update a topic.
///
/// The resulting row is validated before the update: `mode = toggle` requires a `keyField`.
#[utoipa::path(
    patch,
    path = "/api/admin/topics/{name}",
    tag = "topics",
    params(("name" = String, Path, description = "Topic name")),
    request_body = TopicPatch,
    responses(
        (status = 200, description = "Updated topic", body = Topic),
        (status = 400, description = "Invalid body, invalid `keyField`, or toggle without `keyField`", body = ErrorBody),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
        (status = 404, description = "Unknown topic", body = ErrorBody),
        (status = 415, description = "`Content-Type` is not `application/json`", body = ErrorBody),
    ),
)]
pub async fn patch_topic(
    State(st): State<AppState>,
    Path(name): Path<String>,
    JsonBody(p): JsonBody<TopicPatch>,
) -> Result<Json<Topic>, AppError> {
    if let Some(Some(k)) = &p.key_field
        && !valid_key_field(k)
    {
        return Err(AppError::bad("keyField must match ^[A-Za-z0-9_]{1,50}$"));
    }

    let mut tx = st.pool.begin().await?;
    let cur: Topic = sqlx::query_as::<_, TopicRow>(&format!(
        "SELECT {TOPIC_COLS} FROM topics WHERE name = $1 FOR UPDATE"
    ))
    .bind(&name)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?
    .try_into()?;

    let status = p.status.unwrap_or(cur.status);
    let mode = p.mode.unwrap_or(cur.mode);
    let key_field = p.key_field.unwrap_or(cur.key_field);
    let public = p.public.unwrap_or(cur.public);
    if mode == TopicMode::Toggle && key_field.is_none() {
        return Err(AppError::bad("mode toggle requires keyField"));
    }

    let updated: Topic = sqlx::query_as::<_, TopicRow>(&format!(
        "UPDATE topics SET status = $2, mode = $3, key_field = $4, public = $5, updated_at = now() \
         WHERE name = $1 RETURNING {TOPIC_COLS}"
    ))
    .bind(&name)
    .bind(status.as_str())
    .bind(mode.as_str())
    .bind(&key_field)
    .bind(public)
    .fetch_one(&mut *tx)
    .await?
    .try_into()?;
    tx.commit().await?;
    tracing::info!(topic = %name, status = status.as_str(), mode = mode.as_str(), public, "topic updated");
    Ok(Json(updated))
}

/// Delete a topic and all its events.
///
/// If traffic for the topic continues, it is auto-created again as `pending`; to stop ongoing
/// spam, block the topic instead.
#[utoipa::path(
    delete,
    path = "/api/admin/topics/{name}",
    tag = "topics",
    params(("name" = String, Path, description = "Topic name")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
        (status = 404, description = "Unknown topic", body = ErrorBody),
    ),
)]
pub async fn delete_topic(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    let r = sqlx::query("DELETE FROM topics WHERE name = $1")
        .bind(&name)
        .execute(&st.pool)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tracing::info!(topic = %name, "topic deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Latest events of a topic (for moderation).
#[utoipa::path(
    get,
    path = "/api/admin/topics/{name}/sample",
    tag = "topics",
    params(("name" = String, Path, description = "Topic name"), SampleQuery),
    responses(
        (status = 200, description = "Latest events, newest first", body = Vec<EventItem>),
        (status = 400, description = "Invalid limit", body = ErrorBody),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
        (status = 404, description = "Unknown topic", body = ErrorBody),
    ),
)]
pub async fn sample(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<SampleQuery>,
) -> Result<Json<Vec<EventItem>>, AppError> {
    let limit = limit_in(q.limit, 10, 100)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM topics WHERE name = $1)")
        .bind(&name)
        .fetch_one(&st.pool)
        .await?;
    if !exists {
        return Err(AppError::NotFound);
    }
    let items = sqlx::query_as::<_, EventItem>(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE topic = $1 ORDER BY id DESC LIMIT $2"
    ))
    .bind(&name)
    .bind(limit)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(items))
}

/// Event feed, cursor-based by `id`. Filters combine with AND.
///
/// Ascending by default, paged with `after`/`nextAfter`. With `order=desc` the newest events come
/// first, paged with `before`/`nextBefore`.
#[utoipa::path(
    get,
    path = "/api/admin/events",
    tag = "events",
    params(FeedQuery),
    responses(
        (status = 200, description = "A page of events", body = Feed),
        (status = 400, description = "Invalid filter, cursor or limit", body = ErrorBody),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
    ),
)]
pub async fn feed(
    State(st): State<AppState>,
    Query(q): Query<FeedQuery>,
) -> Result<Json<Feed>, AppError> {
    let limit = limit_in(q.limit, 100, 1000)?;
    let after = q.after.unwrap_or(0);
    if after < 0 {
        return Err(AppError::bad("after must be >= 0"));
    }
    if q.before.is_some_and(|b| b < 1) {
        return Err(AppError::bad("before must be >= 1"));
    }
    let order = q.order.unwrap_or(SortOrder::Asc);
    let resource_id = nonempty(&q.resource_id)
        .map(|s| validate_resource_id(s).ok_or_else(|| AppError::bad("invalid resourceId")))
        .transpose()?;
    let resource_url = nonempty(&q.resource_url)
        .map(|s| {
            normalize_url(s, &st.cfg.allowed_hosts)
                .ok_or_else(|| AppError::bad("invalid resourceUrl"))
        })
        .transpose()?;

    let items = sqlx::query_as::<_, EventItem>(&format!(
        "SELECT {EVENT_COLS} FROM events \
         WHERE id > $1 \
           AND ($2::text IS NULL OR topic = $2) \
           AND ($3::text IS NULL OR resource_id = $3) \
           AND ($4::text IS NULL OR resource_url = $4) \
           AND ($6::bigint IS NULL OR id < $6) \
         ORDER BY id {} LIMIT $5",
        order.sql(),
    ))
    .bind(after)
    .bind(nonempty(&q.topic))
    .bind(resource_id)
    .bind(resource_url)
    .bind(limit)
    .bind(q.before)
    .fetch_all(&st.pool)
    .await?;
    let next = (items.len() as i64 == limit)
        .then(|| items.last().map(|e| e.id))
        .flatten();
    let (next_after, next_before) = match order {
        SortOrder::Asc => (next, None),
        SortOrder::Desc => (None, next),
    };
    Ok(Json(Feed {
        items,
        next_after,
        next_before,
    }))
}

/// Delete all events of a visitor across all topics (data-erasure requests).
#[utoipa::path(
    delete,
    path = "/api/admin/events",
    tag = "events",
    params(EraseQuery),
    responses(
        (status = 200, description = "Events deleted", body = Deleted),
        (status = 400, description = "Missing or invalid visitorId", body = ErrorBody),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
    ),
)]
pub async fn erase_visitor(
    State(st): State<AppState>,
    Query(q): Query<EraseQuery>,
) -> Result<Json<Deleted>, AppError> {
    if !valid_visitor_id(&q.visitor_id) {
        return Err(AppError::bad("invalid visitorId"));
    }
    let r = sqlx::query("DELETE FROM events WHERE visitor_id = $1")
        .bind(&q.visitor_id)
        .execute(&st.pool)
        .await?;
    let deleted = r.rows_affected() as i64;
    tracing::info!(deleted, "visitor events erased");
    Ok(Json(Deleted { deleted }))
}

/// OpenAPI document of the public API.
#[utoipa::path(
    get,
    path = "/api/admin/openapi/public.json",
    tag = "openapi",
    responses(
        (status = 200, description = "OpenAPI 3.1 document", body = Object, content_type = "application/json"),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
    ),
)]
pub async fn public_spec() -> impl IntoResponse {
    json_doc(openapi::public_json())
}

/// OpenAPI document of the admin API.
#[utoipa::path(
    get,
    path = "/api/admin/openapi/admin.json",
    tag = "openapi",
    responses(
        (status = 200, description = "OpenAPI 3.1 document", body = Object, content_type = "application/json"),
        (status = 401, description = "Missing or wrong bearer token", body = ErrorBody),
    ),
)]
pub async fn admin_spec() -> impl IntoResponse {
    json_doc(openapi::admin_json())
}

fn json_doc(body: &'static str) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
}
