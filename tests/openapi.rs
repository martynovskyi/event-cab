//! Drift test: the committed OpenAPI documents must match the code (§9). No DB needed.

use serde_json::Value;

#[test]
fn committed_openapi_matches_code() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi");
    for (name, generated) in events::openapi::files() {
        let committed = std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            committed == generated,
            "openapi/{name} is out of date; run `cargo run -- openapi openapi/`"
        );
    }
}

#[test]
fn documents_cover_every_route() {
    let public: Value = serde_json::from_str(events::openapi::public_json()).unwrap();
    let admin: Value = serde_json::from_str(events::openapi::admin_json()).unwrap();
    for doc in [&public, &admin] {
        assert_eq!(doc["openapi"], "3.1.0");
    }

    let paths = |d: &Value| {
        d["paths"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        paths(&public),
        ["/api/events/{topic}", "/api/events/{topic}/summary"]
    );
    let mut admin_paths = paths(&admin);
    admin_paths.sort();
    assert_eq!(
        admin_paths,
        [
            "/api/admin/events",
            "/api/admin/openapi/admin.json",
            "/api/admin/openapi/public.json",
            "/api/admin/topics",
            "/api/admin/topics/{name}",
            "/api/admin/topics/{name}/sample",
        ]
    );

    let post = &public["paths"]["/api/events/{topic}"]["post"]["responses"];
    for code in ["200", "204", "400", "413", "415", "429"] {
        assert!(post.get(code).is_some(), "POST event lacks {code}");
    }
    assert_eq!(
        public["components"]["schemas"]["Summary"]["discriminator"]["propertyName"],
        "mode"
    );
    assert!(admin["components"]["securitySchemes"]["bearer"].is_object());
}
