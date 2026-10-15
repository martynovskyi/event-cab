#![allow(dead_code)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use events::{config::Config, routes, state::AppState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

pub const TOKEN: &str = "test-admin-token-0123456789abcdef";
pub const IP: &str = "203.0.113.1";

pub fn cfg() -> Config {
    Config {
        database_url: String::new(),
        admin_token: TOKEN.into(),
        ip_hash_salt: "test-salt".into(),
        allowed_hosts: vec!["site.com".into()],
        public_port: 0,
        admin_port: 0,
        events_per_ip_min: 1000,
        events_per_min: 1000,
        new_topics_per_ip_hour: 100,
        max_pending_topics: 100,
        toggle_top_n: 20,
    }
}

pub struct App {
    pub public: Router,
    pub admin: Router,
    pub pool: PgPool,
}

pub struct Resp {
    pub status: StatusCode,
    pub body: Value,
}

impl App {
    pub fn new(pool: PgPool) -> Self {
        Self::with(pool, |_| {})
    }

    pub fn with(pool: PgPool, tweak: impl FnOnce(&mut Config)) -> Self {
        let mut c = cfg();
        tweak(&mut c);
        let state = AppState::new(pool.clone(), c);
        Self {
            public: routes::public::router(state.clone()),
            admin: routes::admin::router(state),
            pool,
        }
    }

    pub async fn post(&self, topic: &str, body: Value) -> Resp {
        self.post_from(topic, IP, body).await
    }

    pub async fn post_from(&self, topic: &str, ip: &str, body: Value) -> Resp {
        let req = Request::post(format!("/api/events/{topic}"))
            .header("x-real-ip", ip)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        call(&self.public, req).await
    }

    /// Toggle-style event: `{ resourceId, visitorId, data: { type, active } }`.
    pub async fn react(
        &self,
        topic: &str,
        res: &str,
        visitor: &str,
        ty: &str,
        active: bool,
    ) -> Resp {
        self.post(
            topic,
            json!({ "resourceId": res, "visitorId": visitor, "data": { "type": ty, "active": active } }),
        )
        .await
    }

    pub async fn summary(&self, topic: &str, query: &str) -> Resp {
        let req = Request::get(format!("/api/events/{topic}/summary?{query}"))
            .body(Body::empty())
            .unwrap();
        call(&self.public, req).await
    }

    pub async fn admin(&self, method: Method, uri: &str, body: Option<Value>) -> Resp {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {TOKEN}"));
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        call(&self.admin, b.body(body).unwrap()).await
    }

    pub async fn patch(&self, topic: &str, body: Value) -> Resp {
        self.admin(
            Method::PATCH,
            &format!("/api/admin/topics/{topic}"),
            Some(body),
        )
        .await
    }

    /// approved + public + toggle on `type`.
    pub async fn publish_toggle(&self, topic: &str) {
        let r = self
            .patch(
                topic,
                json!({ "status": "approved", "public": true, "mode": "toggle", "keyField": "type" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }

    pub async fn event_count(&self, topic: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE topic = $1")
            .bind(topic)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    pub async fn topic_exists(&self, topic: &str) -> bool {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM topics WHERE name = $1)")
            .bind(topic)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

pub async fn call(router: &Router, req: Request<Body>) -> Resp {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    Resp { status, body }
}
