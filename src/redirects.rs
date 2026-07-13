use crate::{
    configs::{RedirectBehavior, RedirectMatch, RedirectRule},
    forwarding,
    proxy::SiteRuntime,
};
use pingora::prelude::*;

pub async fn apply(session: &mut Session, site: &SiteRuntime) -> Result<bool> {
    if redirect_http_to_https(session, site).await? {
        return Ok(true);
    }
    let path = session.req_header().uri.path().to_owned();
    let Some(rule) = site.redirects.iter().find(|rule| matches(rule, &path)) else {
        return Ok(false);
    };
    match rule.behavior {
        RedirectBehavior::Rewrite => {
            rewrite_request(session, site, rule, &path)?;
            Ok(false)
        }
        RedirectBehavior::Redirect => respond(session, site, rule, &path).await,
    }
}

async fn redirect_http_to_https(session: &mut Session, site: &SiteRuntime) -> Result<bool> {
    if !site.redirect_https {
        return Ok(false);
    }
    if forwarding::public_scheme(session, site) != "http" {
        return Ok(false);
    }
    let location = https_location(session, site);
    let mut header = ResponseHeader::build(308, Some(3))?;
    header.insert_header("location", location)?;
    header.insert_header("cache-control", "no-store")?;
    header.insert_header("content-length", "0")?;
    session
        .write_response_header(Box::new(header), true)
        .await?;
    Ok(true)
}

fn https_location(session: &Session, site: &SiteRuntime) -> String {
    let host = https_host(session, site);
    let path = session.req_header().uri.path();
    let path = if path.is_empty() { "/" } else { path };
    let mut location = format!("https://{host}{path}");
    if let Some(query) = session.req_header().uri.query() {
        location.push('?');
        location.push_str(query);
    }
    location
}

fn https_host(session: &Session, site: &SiteRuntime) -> String {
    let host = forwarding::public_host(session, site);
    match host.rsplit_once(':') {
        Some((name, port)) if port == "80" || port == "443" => name.to_owned(),
        _ => host,
    }
}

async fn respond(
    session: &mut Session,
    site: &SiteRuntime,
    rule: &RedirectRule,
    path: &str,
) -> Result<bool> {
    let mut location = destination(rule, path);
    if !location.contains('?') {
        if let Some(query) = session.req_header().uri.query() {
            location.push('?');
            location.push_str(query);
        }
    }
    if location.starts_with('/') {
        let origin = forwarding::public_origin(session, site);
        location = format!("{origin}{}", location);
    }
    let mut header = ResponseHeader::build(rule.status, Some(3))?;
    header.insert_header("location", location)?;
    header.insert_header("cache-control", "no-store")?;
    header.insert_header("content-length", "0")?;
    session
        .write_response_header(Box::new(header), true)
        .await?;
    Ok(true)
}

fn rewrite_request(
    session: &mut Session,
    _site: &SiteRuntime,
    rule: &RedirectRule,
    path: &str,
) -> Result<()> {
    let mut uri = destination(rule, path);
    if !uri.contains('?') {
        if let Some(query) = session.req_header().uri.query() {
            uri.push('?');
            uri.push_str(query);
        }
    }
    let parsed = uri.parse().map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("cannot build rewritten uri: {error}"),
        )
    })?;
    session.req_header_mut().set_uri(parsed);
    Ok(())
}

fn matches(rule: &RedirectRule, path: &str) -> bool {
    match rule.match_type {
        RedirectMatch::Exact => path == rule.from,
        RedirectMatch::Prefix => path.starts_with(&rule.from),
    }
}

fn destination(rule: &RedirectRule, path: &str) -> String {
    match rule.match_type {
        RedirectMatch::Exact => rule.to.clone(),
        RedirectMatch::Prefix => format!("{}{}", rule.to, &path[rule.from.len()..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_redirect_keeps_suffix() {
        let rule = RedirectRule {
            from: "/old/".to_owned(),
            to: "/new/".to_owned(),
            status: 308,
            match_type: RedirectMatch::Prefix,
            behavior: RedirectBehavior::Redirect,
        };
        assert!(matches(&rule, "/old/page"));
        assert_eq!(destination(&rule, "/old/page"), "/new/page");
    }

    #[test]
    fn https_host_strips_default_ports() {
        assert_eq!(
            match "example.com:80".rsplit_once(':') {
                Some((name, port)) if port == "80" || port == "443" => name.to_owned(),
                _ => "example.com:80".to_owned(),
            },
            "example.com"
        );
    }
}
