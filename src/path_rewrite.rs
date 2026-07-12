use crate::{
    configs::UpstreamConfig,
    forwarding,
    proxy::{RequestContext, SiteRuntime},
};
use bytes::Bytes;
use pingora::prelude::*;
use std::time::Duration;

const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

pub struct PathRewriteState {
    pub prefix: String,
    rewrite_body: bool,
    buffer: Vec<u8>,
    upstream_origins: Vec<String>,
    public_origin: String,
}

pub async fn prepare(session: &Session, ctx: &mut RequestContext, sites: &[SiteRuntime]) {
    let Some(site_index) = ctx.site else {
        return;
    };
    let Some(site) = sites.get(site_index) else {
        return;
    };
    let upstream = selected_upstream(ctx, site);
    let Some(upstream) = upstream else {
        return;
    };
    let prefix = active_prefix(upstream).unwrap_or_default();
    let upstream_origins = upstream_origins_for(upstream);
    let public_origin = forwarding::public_origin(session, site);
    let rewrite_location = upstream_origins
        .iter()
        .any(|origin| location_rewrite_needed(origin, &public_origin));
    let rewrite_body = !prefix.is_empty();
    if !rewrite_location && !rewrite_body {
        return;
    }
    ctx.path_rewrite = Some(PathRewriteState {
        prefix,
        rewrite_body,
        buffer: Vec::new(),
        upstream_origins,
        public_origin,
    });
}

pub async fn upstream_response_filter(
    response: &mut ResponseHeader,
    ctx: &mut RequestContext,
) -> Result<()> {
    let Some(state) = ctx.path_rewrite.as_mut() else {
        return Ok(());
    };
    if let Some(location) = response.headers.get("location").and_then(|v| v.to_str().ok()) {
        let rewritten = rewrite_location_header(location, state);
        if rewritten != location {
            response.insert_header("location", rewritten)?;
        }
    }
    if state.rewrite_body && response_is_rewritable(response) {
        response.remove_header("Content-Length");
        response.insert_header("Transfer-Encoding", "chunked")?;
    }
    Ok(())
}

pub fn upstream_response_body_filter(
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut RequestContext,
) -> Result<Option<Duration>> {
    let Some(state) = ctx.path_rewrite.as_mut() else {
        return Ok(None);
    };
    if !state.rewrite_body || state.prefix.is_empty() {
        return Ok(None);
    }
    if let Some(chunk) = body {
        if state.buffer.len() + chunk.len() > MAX_BODY_BYTES {
            state.rewrite_body = false;
            state.buffer.clear();
            return Ok(None);
        }
        state.buffer.extend_from_slice(chunk);
        chunk.clear();
    }
    if end_of_stream && !state.buffer.is_empty() {
        let content = std::str::from_utf8(&state.buffer).unwrap_or_default();
        let rewritten = rewrite_prefixed_assets(content, &state.prefix);
        *body = Some(Bytes::from(rewritten));
        state.buffer.clear();
    }
    Ok(None)
}

pub fn strip_internal_prefix(path: &str, prefix: &str) -> String {
    let bare = prefix.trim_end_matches('/');
    if path == bare || path == prefix {
        return "/".to_owned();
    }
    if prefix.ends_with('/') && path.starts_with(prefix) {
        return path[prefix.len() - 1..].to_owned();
    }
    if let Some(rest) = path.strip_prefix(bare).filter(|rest| rest.is_empty() || rest.starts_with('/'))
    {
        return if rest.is_empty() {
            "/".to_owned()
        } else {
            rest.to_owned()
        };
    }
    path.to_owned()
}

pub fn sanitize_upstream_request(request: &mut RequestHeader) {
    if request.uri.path().starts_with("/_next/") {
        request.remove_header("origin");
    }
}

pub fn upstream_host_header(upstream: &UpstreamConfig) -> String {
    upstream
        .host
        .as_deref()
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| upstream.address.clone())
}

fn selected_upstream<'a>(
    ctx: &RequestContext,
    site: &'a SiteRuntime,
) -> Option<&'a UpstreamConfig> {
    if let Some(route) = ctx.internal_route {
        site.internal_routes.get(route).map(|route| &route.upstream)
    } else {
        ctx.upstream.and_then(|index| site.target.get(index))
    }
}

fn active_prefix(upstream: &UpstreamConfig) -> Option<String> {
    let prefix = upstream.base_path.trim();
    if prefix.is_empty() || prefix == "/" {
        None
    } else {
        Some(prefix.trim_end_matches('/').to_owned())
    }
}

fn upstream_origins_for(upstream: &UpstreamConfig) -> Vec<String> {
    let scheme = if upstream.tls { "https" } else { "http" };
    let mut origins = vec![format!("{scheme}://{}", upstream.address)];
    if let Some(host) = upstream.host.as_deref().filter(|host| !host.is_empty()) {
        origins.push(format!("{scheme}://{host}"));
    }
    origins.sort();
    origins.dedup();
    origins
}

fn location_rewrite_needed(upstream_origin: &str, public_origin: &str) -> bool {
    let upstream_origin = upstream_origin.trim_end_matches('/');
    let public_origin = public_origin.trim_end_matches('/');
    upstream_origin != public_origin
}

fn response_is_rewritable(response: &ResponseHeader) -> bool {
    if response.headers.contains_key("content-encoding") {
        return false;
    }
    let Some(content_type) = response
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let media = content_type.split(';').next().unwrap_or(content_type).trim();
    matches!(
        media,
        "text/html"
            | "text/css"
            | "text/javascript"
            | "application/javascript"
            | "application/xhtml+xml"
    )
}

fn rewrite_location_header(location: &str, state: &PathRewriteState) -> String {
    let mut rewritten = location.to_owned();
    for origin in &state.upstream_origins {
        if location_rewrite_needed(origin, &state.public_origin) {
            rewritten = rewritten.replace(origin, &state.public_origin);
        }
    }
    if !state.prefix.is_empty() {
        rewritten = rewrite_base_path(&rewritten, &state.prefix);
    }
    rewritten
}

pub fn rewrite_base_path(location: &str, base_path: &str) -> String {
    let base = base_path.trim_end_matches('/');
    if let Ok(mut url) = url::Url::parse(location) {
        let current = url.path().to_owned();
        if let Some(rest) = current.strip_prefix(base) {
            let path = if rest.is_empty() { "/" } else { rest };
            url.set_path(path);
            return url.to_string();
        }
        return location.to_owned();
    }
    if location == base {
        return "/".to_owned();
    }
    if let Some(rest) = location.strip_prefix(base) {
        return if rest.is_empty() {
            "/".to_owned()
        } else {
            rest.to_owned()
        };
    }
    location.to_owned()
}

fn rewrite_prefixed_assets(content: &str, base_path: &str) -> String {
    let base = base_path.trim_end_matches('/');
    let rooted = format!("{base}/");
    let replacements = [
        (format!("href=\"{rooted}"), "href=\"/"),
        (format!("href='{rooted}"), "href='/"),
        (format!("src=\"{rooted}"), "src=\"/"),
        (format!("src='{rooted}"), "src='/"),
        (format!("action=\"{rooted}"), "action=\"/"),
        (format!("action='{rooted}"), "action='/"),
        (format!("url({rooted}"), "url(/"),
        (format!("url(\"{rooted}"), "url(\"/"),
        (format!("url('{rooted}"), "url('/"),
        (format!("<base href=\"{rooted}\""), "<base href=\"/\""),
        (format!("<base href='{rooted}'"), "<base href='/"),
        (format!("href=\"{base}\""), "href=\"/\""),
        (format!("href='{base}'"), "href='/'"),
        (format!("src=\"{base}\""), "src=\"/\""),
        (format!("src='{base}'"), "src='/'"),
    ];
    let mut rewritten = content.to_owned();
    for (from, to) in replacements {
        rewritten = rewritten.replace(&from, to);
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_internal_api_prefix() {
        assert_eq!(strip_internal_prefix("/api/users", "/api/"), "/users");
        assert_eq!(strip_internal_prefix("/api", "/api/"), "/");
    }

    #[test]
    fn rewrites_upstream_location_to_public_origin() {
        let state = PathRewriteState {
            public_origin: "https://example.com".to_owned(),
            upstream_origins: vec!["http://127.0.0.1:8080".to_owned()],
            prefix: String::new(),
            rewrite_body: false,
            buffer: Vec::new(),
        };
        assert_eq!(
            rewrite_location_header("http://127.0.0.1:8080/dashboard", &state),
            "https://example.com/dashboard"
        );
    }

    #[test]
    fn skips_localhost_body_rewrite() {
        let upstream = UpstreamConfig {
            address: "127.0.0.1:3000".to_owned(),
            tls: false,
            sni: String::new(),
            host: None,
            ca_path: None,
            base_path: String::new(),
            http_version: crate::configs::HttpVersion::Auto,
        };
        assert_eq!(upstream_host_header(&upstream), "127.0.0.1:3000");
        assert!(active_prefix(&upstream).is_none());
    }

    #[test]
    fn rewrites_base_path_assets() {
        assert_eq!(rewrite_base_path("/base/login", "/base"), "/login");
        let body = r#"<link href="/base/app.css"><base href="/base/">"#;
        assert_eq!(
            rewrite_prefixed_assets(body, "/base"),
            r#"<link href="/app.css"><base href="/">"#
        );
    }
}
