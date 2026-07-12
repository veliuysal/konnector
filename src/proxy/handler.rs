use super::{routing::snapshot, routing::SharedRouting, RequestContext};
use crate::{
    access_control, cache_policy, default_site, domain_routing, error_pages,
    forwarding, health_check, path_rewrite, redirects, request_logging, upstreams,
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
        session.set_keepalive(Some(DOWNSTREAM_KEEPALIVE_SECS));
        ctx.mark_started();
        Ok(())
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        if health_check::respond(session).await? {
            ctx.skip_access_log = true;
            return Ok(true);
        }
        let routing = snapshot(&self.routing);
        let Some(site_index) = domain_routing::site_for(session, &routing.sites) else {
            if let Some(root_site) = routing.root_site {
                ctx.site = Some(root_site);
                return Ok(false);
            }
            return default_site::respond(session).await;
        };
        ctx.site = Some(site_index);
        if redirects::apply(session, &routing.sites[site_index]).await? {
            return Ok(true);
        }
        access_control::reject_disallowed(session, ctx, &routing.sites).await
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let routing = snapshot(&self.routing);
        let site_index = ctx
            .site
            .or_else(|| domain_routing::site_for(session, &routing.sites))
            .or(routing.root_site)
            .ok_or_else(|| Error::explain(ErrorType::InternalError, "root proxy is disabled"))?;
        ctx.site = Some(site_index);
        upstreams::select_peer(
            &routing.sites[site_index],
            session.req_header().uri.path(),
            ctx,
        )
        .await
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
        path_rewrite::prepare(session, ctx, &routing.sites).await;
        Ok(())
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        path_rewrite::upstream_response_filter(upstream_response, ctx).await
    }

    fn upstream_response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<bytes::Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>> {
        path_rewrite::upstream_response_body_filter(body, end_of_stream, ctx)
    }

    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()> {
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
            error,
        );
    }

    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        error: &Error,
        ctx: &mut Self::CTX,
    ) -> FailToProxy {
        let routing = snapshot(&self.routing);
        request_logging::log_proxy_failure(
            session,
            ctx,
            &routing.sites,
            routing.default_logging,
            error,
        );
        error_pages::respond_to_proxy_failure(session, error).await
    }
}
