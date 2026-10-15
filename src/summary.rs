//! Read-time interpretation of the event log per topic mode (§5).

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::topics::{Topic, TopicMode};

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

const TOGGLE_COUNTS: &str = "\
SELECT k, COUNT(*) AS n FROM (
  SELECT DISTINCT ON (visitor_id, data->>$3)
         data->>$3 AS k,
         COALESCE((data->>'active')::bool, true) AS active
  FROM events
  WHERE topic = $1 AND resource_key = $2 AND jsonb_typeof(data->$3) = 'string'
  ORDER BY visitor_id, data->>$3, id DESC
) s WHERE active GROUP BY k
ORDER BY n DESC, k
LIMIT $4";

const TOGGLE_MINE: &str = "\
SELECT k FROM (
  SELECT DISTINCT ON (data->>$3)
         data->>$3 AS k,
         COALESCE((data->>'active')::bool, true) AS active
  FROM events
  WHERE topic = $1 AND resource_key = $2 AND visitor_id = $4
    AND jsonb_typeof(data->$3) = 'string'
  ORDER BY data->>$3, id DESC
) s WHERE active
ORDER BY k";

pub async fn compute(
    pool: &PgPool,
    topic: &Topic,
    resource_key: &str,
    visitor_id: Option<&str>,
    top_n: i64,
) -> sqlx::Result<Summary> {
    let key_field = match (topic.mode, topic.key_field.as_deref()) {
        (TopicMode::Toggle, Some(k)) => k,
        // toggle without a key can't exist (DB CHECK); treat defensively as append.
        _ => {
            let (total,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM events WHERE topic = $1 AND resource_key = $2",
            )
            .bind(&topic.name)
            .bind(resource_key)
            .fetch_one(pool)
            .await?;
            return Ok(Summary::Append(AppendSummary {
                mode: AppendTag::Append,
                total,
            }));
        }
    };

    let counts: Vec<(String, i64)> = sqlx::query_as(TOGGLE_COUNTS)
        .bind(&topic.name)
        .bind(resource_key)
        .bind(key_field)
        .bind(top_n)
        .fetch_all(pool)
        .await?;
    let mine: Vec<String> = match visitor_id {
        Some(v) => {
            sqlx::query_scalar(TOGGLE_MINE)
                .bind(&topic.name)
                .bind(resource_key)
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
    }))
}
