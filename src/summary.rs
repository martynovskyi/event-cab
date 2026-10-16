//! Read-time interpretation of the event log per topic mode (§5).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::{
    resource::Resource,
    topics::{Topic, TopicMode},
};

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppendTag {
    Append,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ToggleTag {
    Toggle,
}

/// Summary of an `append` topic.
#[derive(Debug, Serialize, ToSchema)]
pub struct AppendSummary {
    #[schema(inline)]
    pub mode: AppendTag,
    /// Number of events for the resource.
    pub total: i64,
    /// The requesting visitor's latest event for the resource; `null` without `visitorId` or if there is none.
    pub latest: Option<VisitorEvent>,
}

/// Summary of a `toggle` topic.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToggleSummary {
    #[schema(inline)]
    pub mode: ToggleTag,
    /// Active count per key; at most `TOGGLE_TOP_N` most frequent keys.
    pub counts: BTreeMap<String, i64>,
    /// Keys currently active for the requesting visitor (not truncated; `[]` without `visitorId`).
    pub mine: Vec<String>,
    /// The requesting visitor's latest event for the resource; `null` without `visitorId` or if there is none.
    pub latest: Option<VisitorEvent>,
}

/// An event of the requesting visitor.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VisitorEvent {
    pub resource_id: Option<String>,
    pub resource_url: Option<String>,
    #[schema(value_type = Object)]
    pub data: Value,
    pub created_at: DateTime<Utc>,
}

// utoipa only emits a `discriminator` for untagged enums, so each variant carries its own `mode`.
/// Resource summary, discriminated by `mode`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
#[schema(discriminator(property_name = "mode", mapping(
    ("append" = "#/components/schemas/AppendSummary"),
    ("toggle" = "#/components/schemas/ToggleSummary"),
)))]
pub enum Summary {
    Append(AppendSummary),
    Toggle(ToggleSummary),
}

/// `WHERE` clause for a topic's events of a resource: by id, by URL, or both (an event must match both).
/// Only the present filters go into the SQL, so the planner can use the per-column indexes.
struct Filter<'a> {
    sql: String,
    binds: Vec<&'a str>,
}

impl<'a> Filter<'a> {
    fn new(topic: &'a str, resource: &'a Resource) -> Self {
        let mut f = Self {
            sql: "topic = $1".into(),
            binds: vec![topic],
        };
        for (col, v) in [
            ("resource_id", &resource.id),
            ("resource_url", &resource.url),
        ] {
            if let Some(v) = v {
                f.binds.push(v);
                f.sql.push_str(&format!(" AND {col} = ${}", f.binds.len()));
            }
        }
        f
    }

    /// Placeholder of the `i`-th (1-based) parameter after the filter's own.
    fn p(&self, i: usize) -> String {
        format!("${}", self.binds.len() + i)
    }
}

/// Binds the filter's parameters to a sqlx query, in order.
macro_rules! bind_filter {
    ($q:expr, $f:expr) => {{
        let mut q = $q;
        for b in &$f.binds {
            q = q.bind(*b);
        }
        q
    }};
}

pub async fn compute(
    pool: &PgPool,
    topic: &Topic,
    resource: &Resource,
    visitor_id: Option<&str>,
    top_n: i64,
) -> sqlx::Result<Summary> {
    let f = Filter::new(&topic.name, resource);
    let latest = match visitor_id {
        Some(v) => {
            let sql = format!(
                "SELECT resource_id, resource_url, data, created_at FROM events \
                 WHERE {} AND visitor_id = {} ORDER BY id DESC LIMIT 1",
                f.sql,
                f.p(1)
            );
            bind_filter!(sqlx::query_as::<_, VisitorEvent>(&sql), f)
                .bind(v)
                .fetch_optional(pool)
                .await?
        }
        None => None,
    };

    let key_field = match (topic.mode, topic.key_field.as_deref()) {
        (TopicMode::Toggle, Some(k)) => k,
        // toggle without a key can't exist (DB CHECK); treat defensively as append.
        _ => {
            let sql = format!("SELECT COUNT(*) FROM events WHERE {}", f.sql);
            let total = bind_filter!(sqlx::query_scalar::<_, i64>(&sql), f)
                .fetch_one(pool)
                .await?;
            return Ok(Summary::Append(AppendSummary {
                mode: AppendTag::Append,
                total,
                latest,
            }));
        }
    };

    let (key, limit) = (f.p(1), f.p(2));
    let sql = format!(
        "SELECT k, COUNT(*) AS n FROM (
           SELECT DISTINCT ON (visitor_id, data->>{key})
                  data->>{key} AS k,
                  COALESCE((data->>'active')::bool, true) AS active
           FROM events
           WHERE {filter} AND jsonb_typeof(data->{key}) = 'string'
           ORDER BY visitor_id, data->>{key}, id DESC
         ) s WHERE active GROUP BY k
         ORDER BY n DESC, k
         LIMIT {limit}",
        filter = f.sql
    );
    let counts: Vec<(String, i64)> = bind_filter!(sqlx::query_as(&sql), f)
        .bind(key_field)
        .bind(top_n)
        .fetch_all(pool)
        .await?;

    let mine: Vec<String> = match visitor_id {
        Some(v) => {
            let visitor = f.p(2);
            let sql = format!(
                "SELECT k FROM (
                   SELECT DISTINCT ON (data->>{key})
                          data->>{key} AS k,
                          COALESCE((data->>'active')::bool, true) AS active
                   FROM events
                   WHERE {filter} AND visitor_id = {visitor}
                     AND jsonb_typeof(data->{key}) = 'string'
                   ORDER BY data->>{key}, id DESC
                 ) s WHERE active
                 ORDER BY k",
                filter = f.sql
            );
            bind_filter!(sqlx::query_scalar(&sql), f)
                .bind(key_field)
                .bind(v)
                .fetch_all(pool)
                .await?
        }
        None => Vec::new(),
    };
    Ok(Summary::Toggle(ToggleSummary {
        mode: ToggleTag::Toggle,
        counts: counts.into_iter().collect(),
        mine,
        latest,
    }))
}
