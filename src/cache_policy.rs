use crate::{
    domain_routing, internal_routes,
    proxy::{RequestContext, SiteRuntime},
};
use pingora::{cache::CacheKey, prelude::*};

pub fn configure_request(
    session: &mut Session,
    ctx: &mut RequestContext,
    sites: &[SiteRuntime],
) -> Result<()> {
    let Some(site_index) = domain_routing::site_for(session, sites) else {
        return Ok(());
    };
    ctx.site = Some(site_index);
    let site = &sites[site_index];
    let request = session.req_header();
    if internal_routes::find(request.uri.path(), &site.internal_routes).is_some() {
        return Ok(());
    }
    let public =
        !request.headers.contains_key("authorization") && !request.headers.contains_key("cookie");
    let method = request.method.as_str();
    if site.cache.enabled && public && (method == "GET" || method == "HEAD") {
        session
            .cache
            .enable(site.cache_storage, None, None, None, None);
        session
            .cache
            .set_max_file_size_bytes(site.cache.max_file_bytes);
    }
    Ok(())
}

pub fn cache_key(
    session: &Session,
    ctx: &RequestContext,
    sites: &[SiteRuntime],
) -> Result<CacheKey> {
    let site = ctx
        .site
        .and_then(|index| sites.get(index))
        .ok_or_else(|| Error::explain(ErrorType::InternalError, "cache site is missing"))?;
    let host = session
        .req_header()
        .headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(site.primary_domain());
    Ok(CacheKey::new(
        site.primary_domain(),
        format!("{host}{}", session.req_header().uri),
        "",
    ))
}
