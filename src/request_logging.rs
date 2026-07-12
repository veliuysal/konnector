use crate::{
    configs::LogLevel,
    proxy::{RequestContext, SiteRuntime},
};
use pingora::prelude::*;
use std::time::Instant;

pub fn log_level_for(ctx: &RequestContext, sites: &[SiteRuntime], default: LogLevel) -> LogLevel {
    ctx.site
        .and_then(|index| sites.get(index))
        .map(|site| site.logging)
        .unwrap_or(default)
}

pub fn log_request(
    session: &Session,
    ctx: &RequestContext,
    sites: &[SiteRuntime],
    default: LogLevel,
    error: Option<&Error>,
) {
    if ctx.skip_access_log {
        return;
    }

    let level = log_level_for(ctx, sites, default);
    let status = session
        .response_written()
        .map_or(0, |response| response.status.as_u16());
    let has_error = error.is_some();

    if !level.should_log(status, has_error) {
        return;
    }

    let summary = session.as_ref().request_summary();
    if level.is_debug() {
        let upstream = upstream_summary(ctx, sites);
        let duration_ms = ctx
            .started_at
            .map(|started| started.elapsed().as_millis())
            .unwrap_or(0);
        if let Some(error) = error {
            log::debug!("{summary} -> {status} ({duration_ms}ms) upstream={upstream} error={error}");
        } else {
            log::debug!("{summary} -> {status} ({duration_ms}ms) upstream={upstream}");
        }
        return;
    }

    if let Some(error) = error {
        log::info!("{summary} -> {status} error={error}");
    } else {
        log::info!("{summary} -> {status}");
    }
}

pub fn log_proxy_failure(
    session: &Session,
    ctx: &RequestContext,
    sites: &[SiteRuntime],
    default: LogLevel,
    error: &Error,
) {
    if ctx.skip_access_log {
        return;
    }
    let level = log_level_for(ctx, sites, default);
    if level < LogLevel::Warn {
        return;
    }
    let host = session
        .req_header()
        .headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    let path = session.req_header().uri.path();
    log::warn!("proxy error for {host}{path}: {error}");
}

fn upstream_summary(ctx: &RequestContext, sites: &[SiteRuntime]) -> String {
    let Some(site_index) = ctx.site else {
        return "-".to_owned();
    };
    let Some(site) = sites.get(site_index) else {
        return "-".to_owned();
    };
    if let Some(route_index) = ctx.internal_route {
        return site.internal_routes[route_index]
            .upstream
            .address
            .clone();
    }
    let upstream_index = ctx.upstream.unwrap_or(0);
    site.target
        .get(upstream_index)
        .map(|upstream| upstream.address.clone())
        .unwrap_or_else(|| "-".to_owned())
}

impl RequestContext {
    pub fn mark_started(&mut self) {
        self.started_at = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use crate::configs::LogLevel;

    #[test]
    fn log_levels_filter_by_status_and_errors() {
        assert!(!LogLevel::Off.should_log(200, false));
        assert!(LogLevel::Error.should_log(500, false));
        assert!(LogLevel::Error.should_log(200, true));
        assert!(!LogLevel::Error.should_log(404, false));
        assert!(LogLevel::Warn.should_log(404, false));
        assert!(LogLevel::Info.should_log(200, false));
        assert!(LogLevel::Debug.should_log(200, false));
    }
}
