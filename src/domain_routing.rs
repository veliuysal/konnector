use crate::{
    configs::{ListenMode, TrafficMode},
    proxy::SiteRuntime,
    websocket,
};
use pingora::prelude::Session;

pub fn site_for(session: &Session, sites: &[SiteRuntime]) -> Option<usize> {
    let host = request_host(session)?;
    let want_https = session_is_https(session);
    let want_websocket = websocket::is_upgrade_request(session);
    // Prefer exact domain matches over wildcards when several sites could apply.
    if let Some(index) = sites.iter().position(|site| {
        site_matches(site, host, want_https, want_websocket, true)
    }) {
        return Some(index);
    }
    sites
        .iter()
        .position(|site| site_matches(site, host, want_https, want_websocket, false))
}

fn site_matches(
    site: &SiteRuntime,
    host: &str,
    want_https: bool,
    want_websocket: bool,
    exact_only: bool,
) -> bool {
    if !site.listen.accepts(want_https) || !site.traffic.accepts(want_websocket) {
        return false;
    }
    site.domains.iter().any(|domain| {
        if exact_only && is_wildcard(domain) {
            return false;
        }
        host_matches(domain, host)
    })
}

pub fn session_is_https(session: &Session) -> bool {
    session
        .as_downstream()
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
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

/// Track domain claims so sites may share a hostname when listen/traffic modes do not overlap.
#[derive(Clone, Copy, Default)]
pub struct DomainClaim {
    plain: ProtocolClaim,
    tls: ProtocolClaim,
}

#[derive(Clone, Copy, Default)]
struct ProtocolClaim {
    http: bool,
    websocket: bool,
}

impl ProtocolClaim {
    fn claim(&mut self, traffic: TrafficMode) -> Result<(), &'static str> {
        if traffic.http && self.http {
            return Err("http");
        }
        if traffic.websocket && self.websocket {
            return Err("websocket");
        }
        self.http |= traffic.http;
        self.websocket |= traffic.websocket;
        Ok(())
    }
}

impl DomainClaim {
    pub fn claim(&mut self, listen: ListenMode, traffic: TrafficMode) -> Result<(), String> {
        if listen.http {
            self.plain.claim(traffic).map_err(|kind| {
                format!("{kind} traffic on http listen")
            })?;
        }
        if listen.https {
            self.tls.claim(traffic).map_err(|kind| {
                format!("{kind} traffic on https listen")
            })?;
        }
        Ok(())
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
        assert!(host_matches("api.shop.com", "api.shop.com"));
        assert!(host_matches("*.shop.com", "api.shop.com"));
    }

    #[test]
    fn domain_claims_allow_split_listen_and_traffic() {
        let mut claim = DomainClaim::default();
        assert!(claim
            .claim(ListenMode::http_only(), TrafficMode::http_only())
            .is_ok());
        assert!(claim
            .claim(ListenMode::https_only(), TrafficMode::http_only())
            .is_ok());
        assert!(claim
            .claim(ListenMode::http_only(), TrafficMode::websocket_only())
            .is_ok());
        assert!(claim
            .claim(ListenMode::http_only(), TrafficMode::http_only())
            .is_err());
    }
}
