use std::{fmt::Display, str::FromStr};

/// Service configuration, read and validated once at startup (§11).
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub admin_token: String,
    pub ip_hash_salt: String,
    /// Lowercased, `www.`-stripped, punycode.
    pub allowed_hosts: Vec<String>,
    pub public_port: u16,
    pub admin_port: u16,
    pub events_per_ip_min: u32,
    pub events_per_min: u32,
    pub new_topics_per_ip_hour: i64,
    pub max_pending_topics: i64,
    pub toggle_top_n: i64,
}

pub const MIN_ADMIN_TOKEN_LEN: usize = 32;

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Empty values are treated as unset.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let required = |k: &str| get(k).ok_or_else(|| format!("{k} is required"));

        let admin_token = required("ADMIN_TOKEN")?;
        if admin_token.chars().count() < MIN_ADMIN_TOKEN_LEN {
            return Err(format!(
                "ADMIN_TOKEN must be at least {MIN_ADMIN_TOKEN_LEN} characters"
            ));
        }
        let allowed_hosts = match get("ALLOWED_HOSTS") {
            Some(v) => v
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(normalize_host)
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        };

        Ok(Self {
            database_url: required("DATABASE_URL")?,
            admin_token,
            ip_hash_salt: required("IP_HASH_SALT")?,
            allowed_hosts,
            public_port: num(&get, "PUBLIC_PORT", 3001, 1)?,
            admin_port: num(&get, "ADMIN_PORT", 3002, 1)?,
            events_per_ip_min: num(&get, "EVENTS_PER_IP_MIN", 60, 1)?,
            events_per_min: num(&get, "EVENTS_PER_MIN", 20, 1)?,
            new_topics_per_ip_hour: num(&get, "NEW_TOPICS_PER_IP_HOUR", 3, 0)?,
            max_pending_topics: num(&get, "MAX_PENDING_TOPICS", 100, 0)?,
            toggle_top_n: num(&get, "TOGGLE_TOP_N", 20, 1)?,
        })
    }
}

fn num<T>(get: &impl Fn(&str) -> Option<String>, key: &str, default: T, min: T) -> Result<T, String>
where
    T: FromStr + PartialOrd + Display,
{
    let Some(raw) = get(key) else {
        return Ok(default);
    };
    let n: T = raw
        .trim()
        .parse()
        .map_err(|_| format!("{key}: invalid number {raw:?}"))?;
    if n < min {
        return Err(format!("{key} must be >= {min}"));
    }
    Ok(n)
}

/// Lowercase, strip `www.`, convert IDN to punycode (`url::Url::host_str()` returns punycode).
pub fn normalize_host(raw: &str) -> Result<String, String> {
    let h = raw.trim().to_lowercase();
    let h = h.strip_prefix("www.").unwrap_or(&h);
    match url::Host::parse(h) {
        Ok(url::Host::Domain(d)) => Ok(d),
        Ok(other) => Ok(other.to_string()),
        Err(e) => Err(format!("ALLOWED_HOSTS: invalid host {raw:?}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    const BASE: [(&str, &str); 3] = [
        ("DATABASE_URL", "postgres://x"),
        ("ADMIN_TOKEN", "0123456789abcdef0123456789abcdef"),
        ("IP_HASH_SALT", "salt"),
    ];

    #[test]
    fn defaults() {
        let c = Config::from_lookup(env(&BASE)).unwrap();
        assert_eq!((c.public_port, c.admin_port), (3001, 3002));
        assert_eq!((c.events_per_ip_min, c.events_per_min), (60, 20));
        assert_eq!(
            (
                c.new_topics_per_ip_hour,
                c.max_pending_topics,
                c.toggle_top_n
            ),
            (3, 100, 20)
        );
        assert!(c.allowed_hosts.is_empty());
    }

    #[test]
    fn short_admin_token_rejected() {
        let mut e = BASE.to_vec();
        e[1] = ("ADMIN_TOKEN", "short");
        assert!(Config::from_lookup(env(&e)).is_err());
    }

    #[test]
    fn missing_required_rejected() {
        assert!(Config::from_lookup(env(&BASE[1..])).is_err());
    }

    #[test]
    fn allowed_hosts_normalized() {
        let mut e = BASE.to_vec();
        e.push(("ALLOWED_HOSTS", " WWW.Site.com , blog.site.com,,сайт.укр "));
        let c = Config::from_lookup(env(&e)).unwrap();
        assert_eq!(
            c.allowed_hosts,
            ["site.com", "blog.site.com", "xn--80aswg.xn--j1amh"]
        );
    }

    #[test]
    fn invalid_number_rejected() {
        let mut e = BASE.to_vec();
        e.push(("EVENTS_PER_MIN", "0"));
        assert!(Config::from_lookup(env(&e)).is_err());
        e.pop();
        e.push(("PUBLIC_PORT", "abc"));
        assert!(Config::from_lookup(env(&e)).is_err());
    }
}
