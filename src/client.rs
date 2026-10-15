//! Client identity: IP from `X-Real-IP`, the rate-limit client key, and its HMAC hash (§10).

use std::net::{IpAddr, Ipv6Addr};

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers.get("x-real-ip")?.to_str().ok()?.trim().parse().ok()
}

/// IPv4 → the full address; IPv6 → its `/64` prefix (IPv4-mapped addresses count as IPv4).
pub fn client_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{}/64", Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

/// `hex(HMAC-SHA256(key = salt, msg = client_key))`.
pub fn ip_hash(salt: &str, client_key: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(salt.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(client_key.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> String {
        client_key(s.parse().unwrap())
    }

    #[test]
    fn ipv4_is_full_address() {
        assert_eq!(key("203.0.113.7"), "203.0.113.7");
    }

    #[test]
    fn ipv6_is_slash_64() {
        assert_eq!(key("2001:db8:1:2:aaaa:bbbb:cccc:dddd"), "2001:db8:1:2::/64");
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2:ffff::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
    }

    #[test]
    fn ipv4_mapped_is_ipv4() {
        assert_eq!(key("::ffff:203.0.113.7"), "203.0.113.7");
    }

    #[test]
    fn hash_is_keyed() {
        let h = ip_hash("salt", "203.0.113.7");
        assert_eq!(h.len(), 64);
        assert_eq!(h, ip_hash("salt", "203.0.113.7"));
        assert_ne!(h, ip_hash("other", "203.0.113.7"));
    }

    #[test]
    fn real_ip_header() {
        let mut h = HeaderMap::new();
        assert_eq!(client_ip(&h), None);
        h.insert("x-real-ip", "nope".parse().unwrap());
        assert_eq!(client_ip(&h), None);
        h.insert("x-real-ip", "2001:db8::1".parse().unwrap());
        assert_eq!(client_ip(&h), Some("2001:db8::1".parse().unwrap()));
    }
}
