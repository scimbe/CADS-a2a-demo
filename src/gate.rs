//! Login gate, application side.
//!
//! Mechanism: Caddy `forward_auth` -> `https://bunsenbrenner.org/gate/check` (the portal's
//! Keycloak OIDC login gate, per-hostname e-mail allow-list). On success Caddy copies the
//! verified e-mail into `X-Gate-Email`. The control plane answers `200` with an EMPTY
//! `X-Gate-Email` when login is not switched on for the hostname, so this side FAILS
//! CLOSED: no non-empty verified e-mail, no access.

/// The verified viewer e-mail, or `None` (-> 403). Rejects empty values and header
/// injection characters; share-link sessions (`share:<id>`) carry no e-mail and are
/// accepted as an identity only when `allow_share` is set.
pub fn viewer(header: Option<&str>, allow_share: bool) -> Option<String> {
    let v = header?.trim();
    if v.is_empty() || v.len() > 254 || v.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    if v.starts_with("share:") {
        return allow_share.then(|| v.to_string());
    }
    (v.contains('@') && !v.starts_with('@') && !v.ends_with('@')).then(|| v.to_string())
}

/// Constant-time-ish token comparison for the per-run capability token.
pub fn tokens_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fails_closed() {
        assert_eq!(viewer(None, false), None);
        assert_eq!(viewer(Some(""), false), None);
        assert_eq!(viewer(Some("  "), false), None);
        assert_eq!(viewer(Some("nobody"), false), None);
        assert_eq!(viewer(Some("a b@c.de"), false), None);
        assert_eq!(viewer(Some("a@b.de"), false).as_deref(), Some("a@b.de"));
        assert_eq!(viewer(Some("share:abc"), false), None);
        assert_eq!(viewer(Some("share:abc"), true).as_deref(), Some("share:abc"));
    }

    #[test]
    fn token_compare() {
        assert!(tokens_equal("abc", "abc"));
        assert!(!tokens_equal("abc", "abd"));
        assert!(!tokens_equal("", ""));
        assert!(!tokens_equal("ab", "abc"));
    }
}
