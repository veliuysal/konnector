use crate::proxy::SiteRuntime;
use pingora::prelude::Session;

pub fn site_for(session: &Session, sites: &[SiteRuntime]) -> Option<usize> {
    let host = session.req_header().headers.get("host")?.to_str().ok()?;
    let host = normalize_host(host);
    sites.iter().position(|site| {
        site.domains
            .iter()
            .any(|domain| normalize_host(domain).eq_ignore_ascii_case(host))
    })
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
