use crate::proxy::SiteRuntime;
use pingora::prelude::Session;

pub fn site_for(session: &Session, sites: &[SiteRuntime]) -> Option<usize> {
    let host = request_host(session)?;
    // Prefer exact domain matches over wildcards when several sites could apply.
    if let Some(index) = sites.iter().position(|site| {
        site.domains
            .iter()
            .any(|domain| host_matches(domain, host) && !is_wildcard(domain))
    }) {
        return Some(index);
    }
    sites.iter().position(|site| {
        site.domains
            .iter()
            .any(|domain| host_matches(domain, host))
    })
}

fn request_host(session: &Session) -> Option<&str> {
    let headers = &session.req_header().headers;
    if let Some(host) = headers.get("host").and_then(|value| value.to_str().ok()) {
        return Some(normalize_host(host));
    }
    session
        .req_header()
        .uri
        .authority()
        .map(|authority| normalize_host(authority.as_str()))
}

pub fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = normalize_host(pattern);
    let host = normalize_host(host);
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    let pattern = pattern.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if pattern == host {
        return true;
    }
    let Some(suffix) = pattern.strip_prefix("*.") else {
        return false;
    };
    if suffix.is_empty() {
        return false;
    }
    let expected = format!(".{suffix}");
    if !host.ends_with(&expected) {
        return false;
    }
    let left = &host[..host.len() - expected.len()];
    !left.is_empty() && !left.contains('.')
}


pub fn is_wildcard(domain: &str) -> bool {
    normalize_host(domain).starts_with("*.")
}

pub fn normalize_host(authority: &str) -> &str {
    let authority = authority.trim().trim_end_matches('.');
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host).unwrap_or(rest);
    }
    match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            host
        }
        _ => authority,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_domain_matches() {
        assert!(host_matches("Example.COM", "example.com"));
        assert!(host_matches("example.com:443", "example.com"));
        assert!(!host_matches("example.com", "app.example.com"));
    }

    #[test]
    fn wildcard_matches_one_label_subdomain() {
        assert!(host_matches("*.example.com", "app.example.com"));
        assert!(host_matches("*.example.com", "API.Example.Com"));
        assert!(!host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "deep.nested.example.com"));
        assert!(!host_matches("*.example.com", "other.com"));
    }

    #[test]
    fn exact_beats_wildcard_semantics() {
        // api.shop.com should prefer an exact site entry over *.shop.com on another site.
        assert!(host_matches("api.shop.com", "api.shop.com"));
        assert!(host_matches("*.shop.com", "api.shop.com"));
    }
}
