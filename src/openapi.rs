//! OpenAPI 3.1 documents, generated from the handlers (§9).

use std::{io, path::Path, sync::OnceLock};

use utoipa::{
    Modify, OpenApi,
    openapi::security::{Http, HttpAuthScheme, SecurityScheme},
};

use crate::routes;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Events public API",
        description = "Collects events (reactions, feedback) for resources of the website."
    ),
    modifiers(&NoLicense)
)]
pub struct PublicDoc;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Events admin API",
        description = "Topic moderation and event access for the CMS backend. Internal port only."
    ),
    modifiers(&NoLicense, &BearerAuth),
    security(("bearer" = []))
)]
pub struct AdminDoc;

/// utoipa fills `info.license` from Cargo.toml; the crate has none, which would render as `""`.
struct NoLicense;

impl Modify for NoLicense {
    fn modify(&self, api: &mut utoipa::openapi::OpenApi) {
        api.info.license = None;
    }
}

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, api: &mut utoipa::openapi::OpenApi) {
        api.components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                "bearer",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
    }
}

pub fn public_doc() -> utoipa::openapi::OpenApi {
    routes::public::parts().1
}

pub fn admin_doc() -> utoipa::openapi::OpenApi {
    routes::admin::parts().1
}

pub fn render(doc: &utoipa::openapi::OpenApi) -> String {
    let mut s = doc.to_pretty_json().expect("OpenAPI document serializes");
    s.push('\n');
    s
}

pub fn public_json() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| render(&public_doc()))
}

pub fn admin_json() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| render(&admin_doc()))
}

/// `(file name, content)` of both documents.
pub fn files() -> [(&'static str, String); 2] {
    [
        ("public.json", render(&public_doc())),
        ("admin.json", render(&admin_doc())),
    ]
}

/// `events openapi <dir>`.
pub fn write(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, content) in files() {
        std::fs::write(dir.join(name), content)?;
    }
    Ok(())
}
