//! Integration tests against a real PG. Needs `DATABASE_URL` pointing at a server where the
//! user may create databases; `#[sqlx::test]` gives each test a fresh, migrated database.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::{App, IP, Resp, call};
use serde_json::{Value, json};
use sqlx::PgPool;

const NO_CONTENT: StatusCode = StatusCode::NO_CONTENT;

fn counts(r: &common::Resp) -> &Value {
    &r.body["counts"]
}

// ---- topic lifecycle ----

#[sqlx::test]
async fn unknown_topic_is_pending_events_stored_summary_404(pool: PgPool) {
    let app = App::new(pool);
    let r = app.react("reactions", "post:1", "v1", "like", true).await;
    assert_eq!(r.status, NO_CONTENT);
    assert_eq!(app.event_count("reactions").await, 1);

    let t = app
        .admin(Method::GET, "/api/admin/topics?status=pending", None)
        .await;
    assert_eq!(t.status, StatusCode::OK);
    assert_eq!(t.body[0]["name"], "reactions");
    assert_eq!(t.body[0]["status"], "pending");
    assert_eq!(t.body[0]["mode"], "append");
    assert_eq!(t.body[0]["public"], false);
    assert_eq!(t.body[0]["eventCount"], 1);
    assert!(t.body[0]["lastEventAt"].is_string());
    assert!(t.body[0].get("createdIpHash").is_none());

    let s = app.summary("reactions", "resourceId=post:1").await;
    assert_eq!(s.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn toggle_counts_history_after_patch(pool: PgPool) {
    let app = App::new(pool);
    // Stored while the topic is still pending/append.
    app.react("r", "post:1", "v1", "like", true).await;
    app.react("r", "post:1", "v2", "like", true).await;
    app.react("r", "post:1", "v3", "like", true).await;
    app.react("r", "post:1", "v3", "like", false).await; // v3 un-likes
    app.react("r", "post:1", "v1", "fire", true).await;
    app.react("r", "post:2", "v1", "like", true).await; // other resource
    // Historical events whose key is missing / not a string are ignored.
    for data in [
        json!({}),
        json!({"type": null}),
        json!({"type": 5}),
        json!({"type": ["like"]}),
    ] {
        let r = app
            .post(
                "r",
                json!({ "resourceId": "post:1", "visitorId": "v9", "data": data }),
            )
            .await;
        assert_eq!(r.status, NO_CONTENT);
    }
    // `active: null` counts as active.
    app.post(
        "r",
        json!({ "resourceId": "post:1", "visitorId": "v4", "data": { "type": "wow", "active": null } }),
    )
    .await;

    app.publish_toggle("r").await;

    let s = app.summary("r", "resourceId=post:1&visitorId=v1").await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.body);
    assert_eq!(s.body["mode"], "toggle");
    assert_eq!(*counts(&s), json!({ "like": 2, "fire": 1, "wow": 1 }));
    assert_eq!(s.body["mine"], json!(["fire", "like"]));

    let s = app.summary("r", "resourceId=post:1").await;
    assert_eq!(s.body["mine"], json!([]));
    let s = app.summary("r", "resourceId=post:1&visitorId=v3").await;
    assert_eq!(s.body["mine"], json!([]));
}

#[sqlx::test]
async fn toggle_is_idempotent_and_returns_summary(pool: PgPool) {
    let app = App::new(pool);
    app.react("r", "post:1", "seed", "like", false).await;
    app.publish_toggle("r").await;

    for _ in 0..3 {
        let r = app.react("r", "post:1", "v1", "like", true).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(*counts(&r), json!({ "like": 1 }));
        assert_eq!(r.body["mine"], json!(["like"]));
    }
    let r = app.react("r", "post:1", "v1", "like", false).await;
    assert_eq!(*counts(&r), json!({}));
    assert_eq!(r.body["mine"], json!([]));
}

#[sqlx::test]
async fn toggle_requires_key_on_ingest(pool: PgPool) {
    let app = App::new(pool);
    app.react("r", "post:1", "v1", "like", true).await;
    app.publish_toggle("r").await;

    for data in [
        json!({}),
        json!({ "type": 1 }),
        json!({ "type": "" }),
        json!({ "type": "x".repeat(65) }),
    ] {
        let r = app
            .post(
                "r",
                json!({ "resourceId": "post:1", "visitorId": "v1", "data": data }),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{data}");
    }
    let r = app.react("r", "post:1", "v1", &"x".repeat(64), true).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[sqlx::test]
async fn toggle_top_n_ties_by_key(pool: PgPool) {
    let app = App::with(pool, |c| c.toggle_top_n = 2);
    for (v, ty) in [
        ("1", "c"),
        ("2", "c"),
        ("3", "b"),
        ("4", "b"),
        ("5", "a"),
        ("6", "a"),
        ("7", "a"),
        ("8", "d"),
    ] {
        app.react("r", "post:1", v, ty, true).await;
    }
    app.publish_toggle("r").await;
    let s = app.summary("r", "resourceId=post:1&visitorId=8").await;
    assert_eq!(*counts(&s), json!({ "a": 3, "b": 2 }));
    assert_eq!(s.body["mine"], json!(["d"]), "mine is not truncated");
}

#[sqlx::test]
async fn append_summary_counts_all(pool: PgPool) {
    let app = App::new(pool);
    for _ in 0..3 {
        app.post(
            "feedback",
            json!({ "resourceUrl": "/blog/x", "visitorId": "v1", "data": { "text": "hi" } }),
        )
        .await;
    }
    app.patch("feedback", json!({ "status": "approved", "public": true }))
        .await;
    let s = app.summary("feedback", "resourceUrl=/blog/x/").await;
    assert_eq!(
        s.body,
        json!({ "mode": "append", "total": 3, "latest": null })
    );
    let r = app
        .post(
            "feedback",
            json!({ "resourceUrl": "/blog/x", "visitorId": "v1" }),
        )
        .await;
    assert_eq!(
        (&r.body["mode"], &r.body["total"]),
        (&json!("append"), &json!(4))
    );
    assert_eq!(r.body["latest"]["data"], json!({}));
    assert_eq!(r.body["latest"]["resourceUrl"], "/blog/x");
}

#[sqlx::test]
async fn approved_but_private_is_404(pool: PgPool) {
    let app = App::new(pool);
    app.react("r", "post:1", "v1", "like", true).await;
    app.patch("r", json!({ "status": "approved" })).await;
    assert_eq!(
        app.summary("r", "resourceId=post:1").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.react("r", "post:1", "v1", "like", true).await.status,
        NO_CONTENT
    );
}

#[sqlx::test]
async fn blocked_topic_drops_events(pool: PgPool) {
    let app = App::new(pool);
    app.react("spam", "post:1", "v1", "like", true).await;
    app.patch("spam", json!({ "status": "blocked" })).await;
    let r = app.react("spam", "post:1", "v1", "like", true).await;
    assert_eq!(r.status, NO_CONTENT);
    assert_eq!(app.event_count("spam").await, 1);
}

// ---- garbage protection ----

#[sqlx::test]
async fn pending_cap(pool: PgPool) {
    let app = App::with(pool, |c| c.max_pending_topics = 2);
    app.react("t1", "p", "v", "x", true).await;
    app.react("t2", "p", "v", "x", true).await;
    assert_eq!(
        app.react("t3", "p", "v", "x", true).await.status,
        NO_CONTENT
    );
    assert!(!app.topic_exists("t3").await);
    // Existing topics keep accepting events.
    app.react("t1", "p", "v", "x", true).await;
    assert_eq!(app.event_count("t1").await, 2);
    // Approving one frees a slot.
    app.patch("t1", json!({ "status": "approved" })).await;
    app.react("t3", "p", "v", "x", true).await;
    assert!(app.topic_exists("t3").await);
}

#[sqlx::test]
async fn new_topics_per_client_per_hour(pool: PgPool) {
    let app = App::with(pool, |c| c.new_topics_per_ip_hour = 2);
    let body = json!({ "resourceId": "p", "visitorId": "v" });
    app.post_from("a1", "198.51.100.1", body.clone()).await;
    app.post_from("a2", "198.51.100.1", body.clone()).await;
    assert_eq!(
        app.post_from("a3", "198.51.100.1", body.clone())
            .await
            .status,
        NO_CONTENT
    );
    assert!(!app.topic_exists("a3").await);
    // Same IPv6 /64 is the same client.
    app.post_from("b1", "2001:db8:1:1::1", body.clone()).await;
    app.post_from("b2", "2001:db8:1:1::2", body.clone()).await;
    app.post_from("b3", "2001:db8:1:1::3", body.clone()).await;
    assert!(!app.topic_exists("b3").await);
    // Another client may still create topics.
    app.post_from("a3", "198.51.100.2", body).await;
    assert!(app.topic_exists("a3").await);
}

#[sqlx::test]
async fn rate_limit_per_topic_and_global(pool: PgPool) {
    let app = App::with(pool, |c| {
        c.events_per_min = 2;
        c.events_per_ip_min = 5;
    });
    let body = json!({ "resourceId": "p", "visitorId": "v" });
    assert_eq!(app.post("t", body.clone()).await.status, NO_CONTENT);
    assert_eq!(app.post("t", body.clone()).await.status, NO_CONTENT);
    assert_eq!(
        app.post("t", body.clone()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Rotating topic names runs into the global per-IP bucket (3 used so far).
    assert_eq!(app.post("u1", body.clone()).await.status, NO_CONTENT);
    assert_eq!(app.post("u2", body.clone()).await.status, NO_CONTENT);
    assert_eq!(
        app.post("u3", body.clone()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Other clients are unaffected; reads are not limited.
    assert_eq!(
        app.post_from("u3", "198.51.100.9", body).await.status,
        NO_CONTENT
    );
    for _ in 0..20 {
        assert_eq!(
            app.summary("t", "resourceId=p").await.status,
            StatusCode::NOT_FOUND
        );
    }
}

// ---- input validation ----

async fn raw_post(
    app: &App,
    topic: &str,
    ip: Option<&str>,
    ct: Option<&str>,
    body: String,
) -> common::Resp {
    let mut b = Request::post(format!("/api/events/{topic}"));
    if let Some(ip) = ip {
        b = b.header("x-real-ip", ip);
    }
    if let Some(ct) = ct {
        b = b.header("content-type", ct);
    }
    call(&app.public, b.body(Body::from(body)).unwrap()).await
}

#[sqlx::test]
async fn rejections(pool: PgPool) {
    let app = App::new(pool);
    let ok = json!({ "resourceId": "p", "visitorId": "v" }).to_string();
    let json_ct = Some("application/json");
    let bad = StatusCode::BAD_REQUEST;

    assert_eq!(
        raw_post(&app, "t", None, json_ct, ok.clone()).await.status,
        bad
    );
    assert_eq!(
        raw_post(&app, "t", Some("nope"), json_ct, ok.clone())
            .await
            .status,
        bad
    );
    assert_eq!(
        raw_post(&app, "Bad", Some(IP), json_ct, ok.clone())
            .await
            .status,
        bad
    );
    assert_eq!(
        raw_post(&app, &"a".repeat(51), Some(IP), json_ct, ok.clone())
            .await
            .status,
        bad
    );
    assert_eq!(
        raw_post(&app, "t", Some(IP), None, ok.clone()).await.status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        raw_post(&app, "t", Some(IP), Some("text/plain"), ok.clone())
            .await
            .status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        raw_post(
            &app,
            "t",
            Some(IP),
            Some("application/json; charset=utf-8"),
            ok.clone()
        )
        .await
        .status,
        NO_CONTENT
    );
    assert_eq!(
        raw_post(&app, "t", Some(IP), json_ct, "{not json".into())
            .await
            .status,
        bad
    );
    assert_eq!(
        raw_post(&app, "t", Some(IP), json_ct, "[]".into())
            .await
            .status,
        bad
    );
    assert_eq!(
        raw_post(&app, "t", Some(IP), json_ct, "x".repeat(9 * 1024))
            .await
            .status,
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let cases = [
        json!({ "resourceId": "p" }),
        json!({ "resourceId": "p", "visitorId": "" }),
        json!({ "resourceId": "p", "visitorId": "v".repeat(65) }),
        json!({ "visitorId": "v" }),
        json!({ "resourceId": "  ", "visitorId": "v" }),
        json!({ "resourceId": "p", "resourceUrl": "https://evil.com/x", "visitorId": "v" }),
        json!({ "resourceUrl": format!("/{}", "a".repeat(1024)), "visitorId": "v" }),
        json!({ "resourceId": "p", "visitorId": "v", "data": [1] }),
        json!({ "resourceId": "p", "visitorId": "v", "data": "x" }),
        json!({ "resourceId": "p", "visitorId": "v", "data": { "active": "yes" } }),
    ];
    for c in cases {
        assert_eq!(app.post("t", c.clone()).await.status, bad, "{c}");
    }
    let big = json!({ "resourceId": "p", "visitorId": "v", "data": { "t": "x".repeat(4100) } });
    assert_eq!(
        app.post("t", big).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );

    // Only the one valid charset request was stored.
    assert_eq!(app.event_count("t").await, 1);
}

#[sqlx::test]
async fn honeypot_drops_silently(pool: PgPool) {
    let app = App::new(pool);
    for website in [json!("http://spam"), json!(1)] {
        let r = app
            .post(
                "t",
                json!({ "resourceId": "p", "visitorId": "v", "website": website }),
            )
            .await;
        assert_eq!(r.status, NO_CONTENT);
    }
    // Even an otherwise invalid body is dropped silently.
    assert_eq!(
        app.post("t", json!({ "website": "x" })).await.status,
        NO_CONTENT
    );
    assert!(!app.topic_exists("t").await);

    app.post(
        "t",
        json!({ "resourceId": "p", "visitorId": "v", "website": "" }),
    )
    .await;
    assert_eq!(app.event_count("t").await, 1);
}

#[sqlx::test]
async fn url_variants_share_resource_key(pool: PgPool) {
    let app = App::new(pool);
    for url in [
        "/блог",
        "/%d0%b1%d0%bb%d0%be%d0%b3",
        "/%D0%B1%D0%BB%D0%BE%D0%B3/",
        "https://www.site.com//%D0%B1%D0%BB%D0%BE%D0%B3?x=1#y",
    ] {
        let r = app
            .post("f", json!({ "resourceUrl": url, "visitorId": "v" }))
            .await;
        assert_eq!(r.status, NO_CONTENT, "{url}");
    }
    let keys: Vec<String> = sqlx::query_scalar("SELECT DISTINCT resource_key FROM events")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert_eq!(keys, ["url:/%D0%B1%D0%BB%D0%BE%D0%B3"]);
}

#[sqlx::test]
async fn summary_validation(pool: PgPool) {
    let app = App::new(pool);
    app.react("r", "post:1", "v1", "like", true).await;
    app.publish_toggle("r").await;
    let bad = StatusCode::BAD_REQUEST;
    assert_eq!(app.summary("Bad", "resourceId=x").await.status, bad);
    assert_eq!(app.summary("r", "").await.status, bad);
    assert_eq!(
        app.summary("r", "resourceId=&resourceUrl=").await.status,
        bad
    );
    assert_eq!(
        app.summary("r", "resourceUrl=https://evil.com/")
            .await
            .status,
        bad
    );
    assert_eq!(
        app.summary(
            "r",
            &format!("resourceId=post:1&visitorId={}", "v".repeat(65))
        )
        .await
        .status,
        bad
    );
    assert_eq!(
        app.summary("nope", "resourceId=post:1").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.summary("r", "resourceId=&resourceUrl=/x").await.status,
        StatusCode::OK
    );
}

#[sqlx::test]
async fn summary_by_id_url_or_both_with_latest(pool: PgPool) {
    let app = App::new(pool);
    let ev = |id: Option<&str>, url: Option<&str>, v: &str, n: i64| json!({ "resourceId": id, "resourceUrl": url, "visitorId": v, "data": { "n": n } });
    app.post("fb", ev(Some("post:1"), Some("/a"), "v1", 1))
        .await;
    app.post("fb", ev(Some("post:1"), Some("/b"), "v1", 2))
        .await;
    app.post("fb", ev(None, Some("/a/"), "v2", 3)).await;
    app.post("fb", ev(Some("post:2"), None, "v1", 4)).await;
    app.post("other", ev(Some("post:1"), Some("/a"), "v1", 5))
        .await;

    assert_eq!(
        app.summary("fb", "resourceId=post:1").await.status,
        StatusCode::NOT_FOUND,
        "pending topic"
    );
    app.patch("fb", json!({ "status": "approved", "public": true }))
        .await;

    let total = |r: Resp| (r.status, r.body["total"].clone());
    let ok = StatusCode::OK;
    assert_eq!(
        total(app.summary("fb", "resourceId=post:1").await),
        (ok, json!(2))
    );
    assert_eq!(
        total(app.summary("fb", "resourceUrl=/a").await),
        (ok, json!(2))
    );
    assert_eq!(
        total(app.summary("fb", "resourceId=post:1&resourceUrl=/a").await),
        (ok, json!(1))
    );
    assert_eq!(
        total(app.summary("fb", "resourceUrl=/nope").await),
        (ok, json!(0))
    );

    let r = app.summary("fb", "resourceId=post:1").await;
    assert_eq!(r.body["latest"], Value::Null, "no visitorId");
    let r = app.summary("fb", "resourceId=post:1&visitorId=v1").await;
    let latest = &r.body["latest"];
    assert_eq!(latest["resourceId"], "post:1");
    assert_eq!(latest["resourceUrl"], "/b");
    assert_eq!(latest["data"], json!({ "n": 2 }));
    assert!(latest["createdAt"].is_string());
    let r = app.summary("fb", "resourceUrl=/a&visitorId=v1").await;
    assert_eq!(r.body["latest"]["data"], json!({ "n": 1 }));
    let r = app.summary("fb", "resourceUrl=/a&visitorId=v3").await;
    assert_eq!(r.body["latest"], Value::Null);

    // Toggle: counts and `mine` use the same filters, `latest` is the raw last event.
    let react = |id: Option<&str>, v: &str, ty: &str, active: bool| {
        json!({ "resourceId": id, "resourceUrl": "/a", "visitorId": v,
                "data": { "type": ty, "active": active } })
    };
    app.post("r", react(Some("p"), "v1", "like", true)).await;
    app.post("r", react(Some("p"), "v1", "fire", true)).await;
    app.post("r", react(Some("p"), "v1", "fire", false)).await;
    app.post("r", react(None, "v2", "like", true)).await;
    app.publish_toggle("r").await;
    let r = app.summary("r", "resourceId=p&visitorId=v1").await;
    assert_eq!(r.body["counts"], json!({ "like": 1 }));
    assert_eq!(r.body["mine"], json!(["like"]));
    assert_eq!(
        r.body["latest"]["data"],
        json!({ "type": "fire", "active": false })
    );
    let r = app.summary("r", "resourceUrl=/a&visitorId=v2").await;
    assert_eq!(r.body["counts"], json!({ "like": 2 }));
    assert_eq!(r.body["mine"], json!(["like"]));
    let r = app.summary("r", "resourceId=p&resourceUrl=/a").await;
    assert_eq!(r.body["counts"], json!({ "like": 1 }));
    assert_eq!(r.body["latest"], Value::Null);
}

// ---- admin ----

#[sqlx::test]
async fn admin_requires_token_and_is_not_on_public_port(pool: PgPool) {
    let app = App::new(pool);
    for auth in [None, Some("Bearer wrong"), Some(common::TOKEN)] {
        let mut b = Request::get("/api/admin/topics");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        let r = call(&app.admin, b.body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = call(
        &app.public,
        Request::get("/api/admin/topics")
            .header("authorization", format!("Bearer {}", common::TOKEN))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn patch_validation_and_key_field_null(pool: PgPool) {
    let app = App::new(pool);
    app.react("r", "p", "v", "like", true).await;

    assert_eq!(
        app.patch("nope", json!({ "public": true })).await.status,
        StatusCode::NOT_FOUND
    );
    for body in [
        json!({ "mode": "toggle" }),
        json!({ "keyField": "bad-key" }),
        json!({ "status": "weird" }),
        json!({ "unknown": 1 }),
    ] {
        assert_eq!(
            app.patch("r", body.clone()).await.status,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }

    let r = app
        .patch("r", json!({ "mode": "toggle", "keyField": "type" }))
        .await;
    assert_eq!(
        (r.body["mode"].as_str(), r.body["keyField"].as_str()),
        (Some("toggle"), Some("type"))
    );
    // toggle → clearing keyField is rejected, not a 500 from the DB CHECK.
    assert_eq!(
        app.patch("r", json!({ "keyField": null })).await.status,
        StatusCode::BAD_REQUEST
    );

    let r = app.patch("r", json!({ "mode": "append" })).await;
    assert_eq!(r.body["keyField"], "type", "absent keyField is unchanged");
    let r = app.patch("r", json!({ "keyField": null })).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["keyField"], Value::Null, "null clears keyField");
    assert_ne!(r.body["updatedAt"], r.body["createdAt"]);
}

#[sqlx::test]
async fn delete_topic_cascades(pool: PgPool) {
    let app = App::new(pool);
    app.react("r", "p", "v", "like", true).await;
    let d = app.admin(Method::DELETE, "/api/admin/topics/r", None).await;
    assert_eq!(d.status, NO_CONTENT);
    assert!(!app.topic_exists("r").await);
    assert_eq!(app.event_count("r").await, 0);
    let d = app.admin(Method::DELETE, "/api/admin/topics/r", None).await;
    assert_eq!(d.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn erase_visitor(pool: PgPool) {
    let app = App::new(pool);
    app.react("r", "p", "v1", "like", true).await;
    app.react("r", "p", "v2", "like", true).await;
    app.react("other", "p", "v1", "x", true).await;
    app.publish_toggle("r").await;

    let missing = app.admin(Method::DELETE, "/api/admin/events", None).await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    let empty = app
        .admin(Method::DELETE, "/api/admin/events?visitorId=", None)
        .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);

    let r = app
        .admin(Method::DELETE, "/api/admin/events?visitorId=v1", None)
        .await;
    assert_eq!(r.body, json!({ "deleted": 2 }));
    let s = app.summary("r", "resourceId=p&visitorId=v1").await;
    assert_eq!(*counts(&s), json!({ "like": 1 }));
    assert_eq!(s.body["mine"], json!([]));
}

#[sqlx::test]
async fn sample_and_feed(pool: PgPool) {
    let app = App::new(pool);
    for i in 0..5 {
        app.post("a", json!({ "resourceId": format!("p{}", i % 2), "resourceUrl": "/x/", "visitorId": "v", "data": { "i": i } }))
            .await;
    }
    app.post("b", json!({ "resourceUrl": "/y", "visitorId": "v" }))
        .await;

    let s = app
        .admin(Method::GET, "/api/admin/topics/a/sample?limit=2", None)
        .await;
    assert_eq!(s.status, StatusCode::OK);
    let items = s.body.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["data"]["i"], 4);
    assert!(items[0].get("ipHash").is_none());
    assert_eq!(
        app.admin(Method::GET, "/api/admin/topics/a/sample?limit=0", None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.admin(Method::GET, "/api/admin/topics/zz/sample", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Paginate topic `a` with limit 2: 2 + 2 + 1.
    let mut after = 0;
    let mut seen = vec![];
    loop {
        let f = app
            .admin(
                Method::GET,
                &format!("/api/admin/events?topic=a&limit=2&after={after}"),
                None,
            )
            .await;
        assert_eq!(f.status, StatusCode::OK);
        seen.extend(
            f.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["data"]["i"].as_i64().unwrap()),
        );
        match f.body["nextAfter"].as_i64() {
            Some(n) => after = n,
            None => break,
        }
    }
    assert_eq!(seen, [0, 1, 2, 3, 4]);

    let f = app
        .admin(
            Method::GET,
            "/api/admin/events?resourceId=p1&resourceUrl=https://site.com/x",
            None,
        )
        .await;
    assert_eq!(f.body["items"].as_array().unwrap().len(), 2);
    let f = app
        .admin(Method::GET, "/api/admin/events?resourceUrl=/y/", None)
        .await;
    assert_eq!(f.body["items"][0]["topic"], "b");
    assert_eq!(f.body["nextAfter"], Value::Null);
    assert_eq!(
        app.admin(Method::GET, "/api/admin/events?limit=1001", None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.admin(Method::GET, "/api/admin/events?after=x", None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test]
async fn openapi_served_on_admin(pool: PgPool) {
    let app = App::new(pool);
    for doc in ["public", "admin"] {
        let r = app
            .admin(Method::GET, &format!("/api/admin/openapi/{doc}.json"), None)
            .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.body["openapi"], "3.1.0");
    }
}

#[sqlx::test]
async fn health(pool: PgPool) {
    let app = App::new(pool);
    let r = call(
        &app.public,
        Request::get("/health").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}
