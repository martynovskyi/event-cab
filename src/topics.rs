//! Topics: name rules, lookup, and auto-creation with garbage protection (§4).

use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::config::Config;

pub const MAX_TOPIC_NAME: usize = 50;
pub const TOPIC_NAME_PATTERN: &str = "^[a-z0-9][a-z0-9_.-]{0,49}$";
pub const KEY_FIELD_PATTERN: &str = "^[A-Za-z0-9_]{1,50}$";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TopicStatus {
    Pending,
    Approved,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TopicMode {
    Append,
    Toggle,
}

impl TopicStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Blocked => "blocked",
        }
    }
}

impl FromStr for TopicStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "blocked" => Ok(Self::Blocked),
            _ => Err(format!("unknown topic status {s:?}")),
        }
    }
}

impl TopicMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Toggle => "toggle",
        }
    }
}

impl FromStr for TopicMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "append" => Ok(Self::Append),
            "toggle" => Ok(Self::Toggle),
            _ => Err(format!("unknown topic mode {s:?}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Topic {
    pub name: String,
    pub status: TopicStatus,
    pub mode: TopicMode,
    /// For toggle: field name in `data` used as the key (e.g. `type`).
    pub key_field: Option<String>,
    pub public: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Topic {
    /// The public summary is available only for approved + public topics.
    pub fn is_published(&self) -> bool {
        self.status == TopicStatus::Approved && self.public
    }
}

/// Columns of [`TopicRow`], for `SELECT`/`RETURNING`.
pub const TOPIC_COLS: &str = "name, status, mode, key_field, public, created_at, updated_at";

#[derive(sqlx::FromRow)]
pub struct TopicRow {
    name: String,
    status: String,
    mode: String,
    key_field: Option<String>,
    public: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<TopicRow> for Topic {
    type Error = sqlx::Error;
    fn try_from(r: TopicRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            status: r
                .status
                .parse()
                .map_err(|e: String| sqlx::Error::Decode(e.into()))?,
            mode: r
                .mode
                .parse()
                .map_err(|e: String| sqlx::Error::Decode(e.into()))?,
            name: r.name,
            key_field: r.key_field,
            public: r.public,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

pub fn valid_topic_name(s: &str) -> bool {
    let b = s.as_bytes();
    let tail_ok = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(c);
    !b.is_empty()
        && b.len() <= MAX_TOPIC_NAME
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..].iter().all(tail_ok)
}

pub fn valid_key_field(s: &str) -> bool {
    (1..=50).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

pub async fn get(pool: &PgPool, name: &str) -> sqlx::Result<Option<Topic>> {
    sqlx::query_as::<_, TopicRow>(&format!("SELECT {TOPIC_COLS} FROM topics WHERE name = $1"))
        .bind(name)
        .fetch_optional(pool)
        .await?
        .map(Topic::try_from)
        .transpose()
}

/// Creates an unknown topic as `pending`, subject to the per-client and pending caps.
/// Returns `None` when a limit drops the event. The checks are check-then-insert (accepted risk).
pub async fn auto_create(
    pool: &PgPool,
    cfg: &Config,
    name: &str,
    ip_hash: &str,
) -> sqlx::Result<Option<Topic>> {
    let (by_client,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM topics \
         WHERE created_ip_hash = $1 AND created_at > now() - interval '1 hour'",
    )
    .bind(ip_hash)
    .fetch_one(pool)
    .await?;
    if by_client >= cfg.new_topics_per_ip_hour {
        tracing::warn!(
            topic = name,
            "new-topic limit per client reached; event dropped"
        );
        return Ok(None);
    }

    let (pending,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM topics WHERE status = 'pending'")
        .fetch_one(pool)
        .await?;
    if pending >= cfg.max_pending_topics {
        tracing::warn!(
            topic = name,
            pending,
            "pending-topic cap reached; event dropped"
        );
        return Ok(None);
    }

    let created = sqlx::query_as::<_, TopicRow>(&format!(
        "INSERT INTO topics (name, created_ip_hash) VALUES ($1, $2) \
         ON CONFLICT (name) DO NOTHING RETURNING {TOPIC_COLS}"
    ))
    .bind(name)
    .bind(ip_hash)
    .fetch_optional(pool)
    .await?;
    match created {
        Some(row) => {
            tracing::info!(topic = name, "topic auto-created as pending");
            Ok(Some(row.try_into()?))
        }
        // Lost a creation race: the topic exists now.
        None => get(pool, name).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_names() {
        for ok in [
            "a",
            "reactions",
            "0",
            "blog.reactions",
            "a-b_c.d",
            &"a".repeat(50),
        ] {
            assert!(valid_topic_name(ok), "{ok}");
        }
        for bad in [
            "",
            "A",
            "_a",
            ".a",
            "-a",
            "a b",
            "a/b",
            "ä",
            &"a".repeat(51),
        ] {
            assert!(!valid_topic_name(bad), "{bad}");
        }
    }

    #[test]
    fn key_fields() {
        assert!(valid_key_field("type"));
        assert!(valid_key_field("Type_2"));
        assert!(!valid_key_field(""));
        assert!(!valid_key_field("a-b"));
        assert!(!valid_key_field(&"a".repeat(51)));
    }
}
