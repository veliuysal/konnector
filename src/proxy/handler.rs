use super::{routing::snapshot, routing::SharedRouting, RequestContext};
use crate::{
    access_control, cache_policy, default_site, domain_routing, error_pages, forwarding,
    health_check, http3, path_rewrite, redirects, request_logging, upstreams, websocket,
};
use async_trait::async_trait;
use pingora::{cache::CacheKey, prelude::*, proxy::FailToProxy};

const DOWNSTREAM_KEEPALIVE_SECS: u64 = 75;

pub struct DomainProxy {
    routing: SharedRouting,
}

impl DomainProxy {
    pub fn new(routing: SharedRouting) -> Self {
        Self { routing }
    }
}

#[async_trait]
impl ProxyHttp for DomainProxy {
    type CTX = RequestContext;

    fn new_ctx(&self) -> Self::CTX {
        RequestContext::default()
    }

    async fn early_request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()> {
        ctx.mark_started();
        if websocket::is_upgrade_request(session) {
            ctx.websocket = true;
            // Keepalive must be off for upgraded tunnels.
            session.set_keepalive(None);
        } else {
            session.set_keepalive(Some(DOWNSTREAM_KEEPALIVE_SECS));
        }
        Ok(())
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        if health_check::respond(session).await? {
            ctx.skip_access_log = true;
            return Ok(true);
        }
        if crate::acme_challenge::respond(session).await? {
            ctx.skip_access_log = true;
            return Ok(true);
        }
        let routing = snapshot(&self.routing);
        let site_index = match domain_routing::site_for(session, &routing.sites) {
            Some(index) => index,
            None => {
                if let Some(root_site) = routing.root_site {
                    root_site
                } else {
                    return default_site::respond(session).await;
                }
            }
        };
        ctx.site = Some(site_index);
        if redirects::apply(session, &routing.sites[site_index]).await? {
            return Ok(true);
        }
        if access_control::reject_disallowed(session, ctx, &routing.sites).await? {
            return Ok(true);
        }
        // WebSocket upgrades are HTTP/1.1 only — skip the HTTP/3 client path.
        if ctx.websocket {
            return Ok(false);
        }
        http3::proxy_if_needed(
            session,
            ctx,
            &routing.sites,
            routing.default_logging,
            routing.root_site,
        )
        .await
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        ctx.mark_proxied();
        if websocket::is_upgrade_request(session) {
            ctx.websocket = true;
        }
        let routing = snapshot(&self.routing);
        let site_index = ctx
            .site
            .or_else(|| domain_routing::site_for(session, &routing.sites))
            .or(routing.root_site)
            .ok_or_else(|| Error::explain(ErrorType::InternalError, "root proxy is disabled"))?;
        ctx.site = Some(site_index);
        let peer = upstreams::select_peer(
            &routing.sites[site_index],
            session.req_header().uri.path(),
            ctx,
        )
        .await?;
        request_logging::log_proxy_started(
            session,
            ctx,
            &routing.sites,
            routing.default_logging,
            routing.root_site,
        );
        Ok(peer)
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        let routing = snapshot(&self.routing);
        forwarding::apply(session, request, ctx, &routing.sites).await?;
        upstreams::apply_request_transform(&routing.sites, request, ctx).await?;
        if !ctx.websocket {
            path_rewrite::prepare(session, ctx, &routing.sites).await;
        }
        Ok(())
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if ctx.websocket {
            return Ok(());
        }
        path_rewrite::upstream_response_filter(upstream_response, ctx).await
    }

    fn upstream_response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<bytes::Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>> {
        if ctx.websocket {
            return Ok(None);
        }
        path_rewrite::upstream_response_body_filter(body, end_of_stream, ctx)
    }

    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()> {
        if ctx.websocket || websocket::is_upgrade_request(session) {
            return Ok(());
        }
        let routing = snapshot(&self.routing);
        cache_policy::configure_request(session, ctx, &routing.sites)
    }

    fn cache_key_callback(&self, session: &Session, ctx: &mut Self::CTX) -> Result<CacheKey> {
        let routing = snapshot(&self.routing);
        cache_policy::cache_key(session, ctx, &routing.sites)
    }

    async fn logging(&self, session: &mut Session, error: Option<&Error>, ctx: &mut Self::CTX) {
        let routing = snapshot(&self.routing);
        request_logging::log_request(
            session,
            ctx,
            &routing.sites,
            routing.default_logging,
            routing.root_site,
            error,
        );
    }

    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        error: &Error,
        _ctx: &mut Self::CTX,
    ) -> FailToProxy {
        error_pages::respond_to_proxy_failure(session, error).await
    }
}
