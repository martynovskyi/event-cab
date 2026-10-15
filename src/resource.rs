//! Resource identification and URL normalization (§6), plus visitor id validation.

use crate::error::AppError;

pub const MAX_RESOURCE_ID: usize = 200;
pub const MAX_URL_INPUT: usize = 2048;
pub const MAX_PATH: usize = 1024;
pub const MAX_VISITOR_ID: usize = 64;

/// A validated resource. `key` matches the `events.resource_key` generated column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub id: Option<String>,
    pub url: Option<String>,
    pub key: String,
}

impl Resource {
    /// Both fields are validated when present; at least one is required.
    pub fn resolve(
        id: Option<&str>,
        url: Option<&str>,
        allowed_hosts: &[String],
    ) -> Result<Self, AppError> {
        let id = id
            .map(|s| validate_resource_id(s).ok_or_else(|| AppError::bad("invalid resourceId")))
            .transpose()?;
        let url = url
            .map(|s| {
                normalize_url(s, allowed_hosts).ok_or_else(|| AppError::bad("invalid resourceUrl"))
            })
            .transpose()?;
        let key = match (&id, &url) {
            (Some(id), _) => format!("id:{id}"),
            (None, Some(url)) => format!("url:{url}"),
            (None, None) => return Err(AppError::bad("resourceId or resourceUrl is required")),
        };
        Ok(Self { id, url, key })
    }
}

/// Trimmed, 1..200 chars, no control characters.
pub fn validate_resource_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    let ok = (1..=MAX_RESOURCE_ID).contains(&s.chars().count()) && !s.chars().any(char::is_control);
    ok.then(|| s.to_owned())
}

/// 1..64 chars, no control characters.
pub fn valid_visitor_id(s: &str) -> bool {
    (1..=MAX_VISITOR_ID).contains(&s.chars().count()) && !s.chars().any(char::is_control)
}

/// `allowed_hosts` must already be lowercased, `www.`-stripped and punycode (done in config.rs).
pub fn normalize_url(raw: &str, allowed_hosts: &[String]) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_INPUT {
        return None;
    }
    const LOCAL: &str = "local.invalid";
    let base = url::Url::parse(&format!("https://{LOCAL}")).unwrap();
    let u = base.join(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host != LOCAL && !allowed_hosts.iter().any(|h| h == host) {
        return None;
    }
    let mut path = String::with_capacity(u.path().len());
    for c in canonical_escapes(u.path()).chars() {
        if !(c == '/' && path.ends_with('/')) {
            path.push(c); // collapse `//`
        }
    }
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    (path.len() <= MAX_PATH).then_some(path)
}

/// Decode escapes of unreserved chars, upper-case all other escapes.
fn canonical_escapes(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = String::with_capacity(p.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            let v = u8::from_str_radix(&p[i + 1..i + 3], 16).unwrap();
            if v.is_ascii_alphanumeric() || b"-._~".contains(&v) {
                out.push(v as char);
            } else {
                out.push_str(&format!("%{v:02X}"));
            }
            i += 3;
            continue;
        }
        out.push(b[i] as char); // url::Url::path() is pure ASCII
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<String> {
        vec!["site.com".into(), "blog.site.com".into()]
    }

    fn n(raw: &str) -> Option<String> {
        normalize_url(raw, &hosts())
    }

    #[test]
    fn relative_and_absolute() {
        assert_eq!(n("/blog/x").as_deref(), Some("/blog/x"));
        assert_eq!(n("blog/x").as_deref(), Some("/blog/x"));
        assert_eq!(n("https://site.com/blog/x").as_deref(), Some("/blog/x"));
        assert_eq!(
            n("http://WWW.Site.COM:8080/blog/x").as_deref(),
            Some("/blog/x")
        );
        assert_eq!(n("https://blog.site.com/x").as_deref(), Some("/x"));
    }

    #[test]
    fn rejects_foreign_hosts_and_schemes() {
        assert_eq!(n("https://evil.com/x"), None);
        assert_eq!(n("//evil.com/x"), None);
        assert_eq!(n("\\\\evil.com/x"), None);
        assert_eq!(n("javascript:alert(1)"), None);
        assert_eq!(n("ftp://site.com/x"), None);
        assert_eq!(n(""), None);
        assert_eq!(n("   "), None);
        assert_eq!(normalize_url("https://site.com/x", &[]), None);
        assert_eq!(normalize_url("/x", &[]).as_deref(), Some("/x"));
    }

    #[test]
    fn strips_query_hash_and_slashes() {
        assert_eq!(
            n("/blog/post-1/?utm=x#top").as_deref(),
            Some("/blog/post-1")
        );
        assert_eq!(n("/").as_deref(), Some("/"));
        assert_eq!(n("https://site.com").as_deref(), Some("/"));
        assert_eq!(n("/a//b///c//").as_deref(), Some("/a/b/c"));
        assert_eq!(n("//").as_deref(), None); // `//` is a network-path reference with empty host
    }

    #[test]
    fn dot_segments_resolved() {
        assert_eq!(n("/a/./b/../c").as_deref(), Some("/a/c"));
        assert_eq!(n("/a/%2e%2E/c").as_deref(), Some("/c"));
    }

    #[test]
    fn percent_encoding_canonicalized() {
        let expected = "/%D0%B1%D0%BB%D0%BE%D0%B3";
        assert_eq!(n("/блог").as_deref(), Some(expected));
        assert_eq!(n("/%d0%b1%d0%bb%d0%be%d0%b3").as_deref(), Some(expected));
        assert_eq!(n("/%D0%B1%D0%BB%D0%BE%D0%B3/").as_deref(), Some(expected));
        assert_eq!(n("/%41%7e%2D").as_deref(), Some("/A~-"));
        assert_eq!(n("/a%2fb").as_deref(), Some("/a%2Fb"));
        assert_eq!(n("/100%").as_deref(), Some("/100%"));
        assert_eq!(n("/a%zz").as_deref(), Some("/a%zz"));
    }

    #[test]
    fn length_limits() {
        let ok = format!("/{}", "a".repeat(MAX_PATH - 1));
        assert_eq!(n(&ok).as_deref(), Some(ok.as_str()));
        assert_eq!(n(&format!("/{}", "a".repeat(MAX_PATH))), None);
        // 342 Cyrillic chars → 684 bytes of input, but ~2052 bytes once percent-encoded
        assert_eq!(n(&format!("/{}", "б".repeat(342))), None);
        assert_eq!(n(&format!("/{}", "a".repeat(MAX_URL_INPUT))), None);
    }

    #[test]
    fn resource_resolve() {
        let h = hosts();
        let r = Resource::resolve(Some(" post:1 "), Some("/x/"), &h).unwrap();
        assert_eq!(
            (r.id.as_deref(), r.url.as_deref()),
            (Some("post:1"), Some("/x"))
        );
        assert_eq!(r.key, "id:post:1");
        assert_eq!(
            Resource::resolve(None, Some("/x"), &h).unwrap().key,
            "url:/x"
        );
        assert!(Resource::resolve(None, None, &h).is_err());
        assert!(Resource::resolve(Some("  "), None, &h).is_err());
        assert!(Resource::resolve(Some("a\u{7}"), None, &h).is_err());
        assert!(Resource::resolve(Some(&"a".repeat(201)), None, &h).is_err());
        assert!(Resource::resolve(Some("ok"), Some("https://evil.com/x"), &h).is_err());
    }

    #[test]
    fn visitor_id() {
        assert!(valid_visitor_id("0b6a9a2e-6c1e-4d2b-9f1a-3c5e7d9b1a2f"));
        assert!(!valid_visitor_id(""));
        assert!(!valid_visitor_id(&"v".repeat(65)));
        assert!(!valid_visitor_id("a\nb"));
    }
}
