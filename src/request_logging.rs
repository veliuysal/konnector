use crate::{
    configs::{LogLevel, UpstreamConfig},
    file_log,
    proxy::{RequestContext, SiteRuntime},
};
use pingora::prelude::*;
use std::{
    io::Write,
    net::SocketAddr,
    time::{Duration, Instant},
};

pub fn access_logging_enabled(level: LogLevel) -> bool {
    level.is_enabled()
}

pub fn log_level_for(
    ctx: &RequestContext,
    sites: &[SiteRuntime],
    default: LogLevel,
    root_site: Option<usize>,
) -> LogLevel {
    let Some(index) = ctx.site else {
        return default;
    };
    if root_site == Some(index) {
        return default;
    }
    sites
        .get(index)
        .map(|site| site.logging)
        .unwrap_or(default)
}

fn site_log_name(
    ctx: &RequestContext,
    sites: &[SiteRuntime],
    root_site: Option<usize>,
) -> String {
    let Some(index) = ctx.site else {
        return "root".to_owned();
    };
    if root_site == Some(index) {
        return "root".to_owned();
    }
    sites
        .get(index)
        .map(|site| {
            if site.source_file.is_empty() {
                site.primary_domain().to_owned()
            } else {
                site.source_file.clone()
            }
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn emit_access(site_name: &str, message: &str) {
    let line = file_log::format_line("INFO", message);
    // Journal/stderr only — keep access traffic out of logs/main.
    let _ = writeln!(std::io::stderr(), "{line}");
    let _ = std::io::stderr().flush();
    file_log::write_site(site_name, &line);
}

pub fn log_request(
    session: &Session,
    ctx: &RequestContext,
    sites: &[SiteRuntime],
    default: LogLevel,
    root_site: Option<usize>,
    error: Option<&Error>,
) {
    if ctx.skip_access_log || !ctx.proxied {
        return;
    }

    let level = log_level_for(ctx, sites, default, root_site);
    if !level.is_enabled() {
        return;
    }

    let status = session
        .response_written()
        .map_or(0, |response| response.status.as_u16());
    let summary = session.as_ref().request_summary();
    let http_version = upstream_http_version(ctx, sites);
    let site_name = site_log_name(ctx, sites, root_site);
    let ws = if ctx.websocket { " websocket=true" } else { "" };

    if level.is_debug() {
        let upstream = upstream_summary(ctx, sites);
        let duration_ms = ctx
            .started_at
            .map(|started| started.elapsed().as_millis())
            .unwrap_or(0);
        let message = if let Some(error) = error {
            format!(
                "{summary} -> {status} ({duration_ms}ms) upstream={upstream} http={http_version}{ws} error={error}"
            )
        } else {
            format!(
                "{summary} -> {status} ({duration_ms}ms) upstream={upstream} http={http_version}{ws}"
            )
        };
        emit_access(&site_name, &message);
        return;
    }

    let message = if let Some(error) = error {
        format!("{summary} -> {status} http={http_version}{ws} error={error}")
    } else {
        format!("{summary} -> {status} http={http_version}{ws}")
    };
    emit_access(&site_name, &message);
}

pub fn log_proxy_started(
    session: &Session,
    ctx: &RequestContext,
    sites: &[SiteRuntime],
    default: LogLevel,
    root_site: Option<usize>,
) {
    if ctx.skip_access_log || !ctx.proxied {
        return;
    }
    let level = log_level_for(ctx, sites, default, root_site);
    if !level.is_enabled() {
        return;
    }
    let summary = session.as_ref().request_summary();
    let upstream = upstream_summary(ctx, sites);
    let http_version = upstream_http_version(ctx, sites);
    let site_name = site_log_name(ctx, sites, root_site);
    let ws = if ctx.websocket { " websocket=true" } else { "" };
    emit_access(
        &site_name,
        &format!("{summary} proxy upstream={upstream} http={http_version}{ws}"),
    );
}

pub fn log_tcp_connection(
    name: &str,
    source_file: &str,
    client: SocketAddr,
    upstream: &str,
    bytes_up: u64,
    bytes_down: u64,
    duration: Duration,
    error: Option<&str>,
) {
    let duration_ms = duration.as_millis();
    let message = match error {
        Some(error) => format!(
            "TCP {name} {client} -> {upstream} ({duration_ms}ms) up={bytes_up} down={bytes_down} error={error}"
        ),
        None => format!(
            "TCP {name} {client} -> {upstream} ({duration_ms}ms) up={bytes_up} down={bytes_down}"
        ),
    };
    let folder = if source_file.is_empty() {
        name
    } else {
        source_file
    };
    emit_access(folder, &message);
}

fn selected_upstream<'a>(
    ctx: &RequestContext,
    sites: &'a [SiteRuntime],
) -> Option<&'a UpstreamConfig> {
    let site_index = ctx.site?;
    let site = sites.get(site_index)?;
    if let Some(route_index) = ctx.internal_route {
        return site.internal_routes.get(route_index).map(|route| &route.upstream);
    }
    let upstream_index = ctx.upstream?;
    site.target.get(upstream_index)
}

fn upstream_http_version(ctx: &RequestContext, sites: &[SiteRuntime]) -> &'static str {
    selected_upstream(ctx, sites)
        .map(|upstream| upstream.http_version.log_label())
        .unwrap_or("-")
}

fn upstream_summary(ctx: &RequestContext, sites: &[SiteRuntime]) -> String {
    selected_upstream(ctx, sites)
        .map(|upstream| upstream.address.clone())
        .unwrap_or_else(|| "-".to_owned())
}

impl RequestContext {
    pub fn mark_started(&mut self) {
        self.started_at = Some(Instant::now());
    }

    pub fn mark_proxied(&mut self) {
        self.proxied = true;
    }
}

#[cfg(test)]
mod tests {
    use crate::configs::{HttpVersion, LogLevel, UpstreamConfig};
    use crate::proxy::{RequestContext, SiteRuntime, UpstreamRuntime};

    #[test]
    fn root_proxy_uses_global_logging_level() {
        let sites = vec![SiteRuntime {
            domains: Vec::new(),
            target: UpstreamRuntime::Direct(UpstreamConfig {
                address: "127.0.0.1:3000".into(),
                tls: false,
                sni: String::new(),
                host: None,
                ca_path: None,
                base_path: String::new(),
                http_version: HttpVersion::Auto,
            }),
            internal_routes: Vec::new(),
            redirects: Vec::new(),
            access: Default::default(),
            cache: Default::default(),
            cache_storage: Box::leak(Box::new(pingora::cache::MemCache::new())),
            forwarding: Default::default(),
            logging: LogLevel::Off,
            listen: crate::configs::ListenMode::both(),
            traffic: crate::configs::TrafficMode::both(),
            redirect_https: false,
            source_file: "root".to_owned(),
        }];
        let ctx = RequestContext {
            site: Some(0),
            proxied: true,
            upstream: Some(0),
            ..Default::default()
        };
        assert_eq!(
            super::log_level_for(&ctx, &sites, LogLevel::Info, Some(0)),
            LogLevel::Info
        );
    }

    #[test]
    fn proxied_requests_are_required_for_access_log() {
        let ctx = RequestContext {
            proxied: false,
            site: Some(0),
            upstream: Some(0),
            ..Default::default()
        };
        assert!(!ctx.proxied);
        let mut ctx = ctx;
        ctx.mark_proxied();
        assert!(ctx.proxied);
    }

    #[test]
    fn access_logging_disabled_when_off() {
        assert!(!super::access_logging_enabled(LogLevel::Off));
        assert!(super::access_logging_enabled(LogLevel::Info));
    }

    #[test]
    fn log_levels_log_every_request_when_enabled() {
        assert!(!LogLevel::Off.is_enabled());
        assert!(LogLevel::Error.is_enabled());
        assert!(LogLevel::Warn.is_enabled());
        assert!(LogLevel::Info.is_enabled());
        assert!(LogLevel::Debug.is_enabled());
    }

    #[test]
    fn upstream_http_version_uses_selected_upstream() {
        let upstream = UpstreamConfig {
            address: "127.0.0.1:443".into(),
            tls: true,
            sni: "backend.example.com".into(),
            host: None,
            ca_path: None,
            base_path: String::new(),
            http_version: HttpVersion::Http3,
        };
        let sites = vec![SiteRuntime {
            domains: vec!["example.com".into()],
            target: UpstreamRuntime::Direct(upstream),
            internal_routes: Vec::new(),
            redirects: Vec::new(),
            access: Default::default(),
            cache: Default::default(),
            cache_storage: Box::leak(Box::new(pingora::cache::MemCache::new())),
            forwarding: Default::default(),
            logging: LogLevel::Info,
            listen: crate::configs::ListenMode::default(),
            traffic: crate::configs::TrafficMode::default(),
            redirect_https: false,
            source_file: "example".to_owned(),
        }];
        let ctx = RequestContext {
            site: Some(0),
            upstream: Some(0),
            ..Default::default()
        };
        assert_eq!(super::upstream_http_version(&ctx, &sites), "3");
    }

    #[test]
    fn http_version_labels_are_stable() {
        assert_eq!(HttpVersion::Auto.log_label(), "auto");
        assert_eq!(HttpVersion::Http11.log_label(), "1.1");
        assert_eq!(HttpVersion::Http2.log_label(), "2");
        assert_eq!(HttpVersion::Http3.log_label(), "3");
    }
}
