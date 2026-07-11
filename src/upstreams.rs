use crate::{
    configs::UpstreamConfig,
    internal_routes,
    path_rewrite,
    proxy::{RequestContext, SiteRuntime, UpstreamRuntime},
};
use once_cell::sync::OnceCell;
use openssl::x509::X509;
use pingora::{lb::health_check::TcpHealthCheck, prelude::*};
use std::{fs, sync::Arc, time::Duration};

static ROOT_CA: OnceCell<Arc<pingora::protocols::tls::CaType>> = OnceCell::new();

const UPSTREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub fn create_load_balancer(
    domain: &str,
    upstreams: &[UpstreamConfig],
    health_check: bool,
    health_check_interval_seconds: u64,
) -> LoadBalancer<RoundRobin> {
    let addresses = upstreams.iter().map(|upstream| upstream.address.as_str());
    let mut load_balancer = LoadBalancer::try_from_iter(addresses)
        .unwrap_or_else(|error| panic!("invalid upstream for {domain}: {error}"));
    if health_check {
        load_balancer.set_health_check(TcpHealthCheck::new());
        load_balancer.health_check_frequency =
            Some(Duration::from_secs(health_check_interval_seconds));
    }
    load_balancer
}

pub async fn select_peer(
    site: &SiteRuntime,
    path: &str,
    ctx: &mut RequestContext,
) -> Result<Box<HttpPeer>> {
    if let Some(route_index) = internal_routes::find(path, &site.internal_routes) {
        ctx.internal_route = Some(route_index);
        ctx.upstream = Some(0);
        let upstream = &site.internal_routes[route_index].upstream;
        let peer = HttpPeer::new(
            upstream.address.as_str(),
            upstream.tls,
            upstream.sni.clone(),
        );
        return configure_peer(peer, upstream);
    }

    match &site.target {
        UpstreamRuntime::Direct(upstream) => {
            ctx.upstream = Some(0);
            let peer = HttpPeer::new(
                upstream.address.as_str(),
                upstream.tls,
                upstream.sni.to_string(),
            );
            configure_peer(peer, upstream)
        }
        UpstreamRuntime::LoadBalanced {
            upstreams,
            load_balancer,
        } => {
            let backend = load_balancer
                .select(b"", 256)
                .ok_or_else(|| Error::explain(ErrorType::HTTPStatus(503), "no healthy upstream"))?;
            let address = backend.addr.to_string();
            let upstream_index = upstreams
                .iter()
                .position(|upstream| upstream.address == address)
                .ok_or_else(|| {
                    Error::explain(
                        ErrorType::InternalError,
                        "selected upstream is not configured",
                    )
                })?;
            ctx.upstream = Some(upstream_index);
            let upstream = &upstreams[upstream_index];
            let peer = HttpPeer::new(backend, upstream.tls, upstream.sni.to_string());
            configure_peer(peer, upstream)
        }
    }
}

fn configure_peer(mut peer: HttpPeer, upstream: &UpstreamConfig) -> Result<Box<HttpPeer>> {
    peer.options.idle_timeout = Some(UPSTREAM_IDLE_TIMEOUT);
    if let Some(path) = &upstream.ca_path {
        let ca = ROOT_CA.get_or_try_init(|| load_ca(path))?;
        peer.options.ca = Some(Arc::clone(ca));
    }
    Ok(Box::new(peer))
}

fn load_ca(path: &str) -> Result<Arc<pingora::protocols::tls::CaType>> {
    let pem = fs::read(path).map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("cannot read root proxy CA: {error}"),
        )
    })?;
    let certificates = X509::stack_from_pem(&pem).map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("invalid root proxy CA: {error}"),
        )
    })?;
    if certificates.is_empty() {
        return Err(Error::explain(
            ErrorType::InternalError,
            "root proxy CA file contains no certificates",
        ));
    }
    Ok(Arc::new(certificates.into_boxed_slice()))
}

pub async fn apply_request_transform(
    sites: &[SiteRuntime],
    request: &mut RequestHeader,
    ctx: &RequestContext,
) -> Result<()> {
    if let (Some(site), Some(upstream)) = (ctx.site, ctx.upstream) {
        let selected = if let Some(route) = ctx.internal_route {
            Some(&sites[site].internal_routes[route].upstream)
        } else {
            sites[site].target.get(upstream)
        };
        if let Some(route) = ctx.internal_route {
            let route = &sites[site].internal_routes[route];
            if route.strip_prefix {
                let stripped = path_rewrite::strip_internal_prefix(
                    request.uri.path(),
                    &route.prefix,
                );
                set_request_path(request, &stripped)?;
            }
        }
        if let Some(upstream) = selected {
            request.insert_header("host", path_rewrite::upstream_host_header(upstream))?;
            apply_base_path(request, &upstream.base_path)?;
        }
        path_rewrite::sanitize_upstream_request(request);
    }
    Ok(())
}

fn set_request_path(request: &mut RequestHeader, path: &str) -> Result<()> {
    let mut uri = path.to_owned();
    if let Some(query) = request.uri.query() {
        uri.push('?');
        uri.push_str(query);
    }
    let parsed = uri.parse().map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("cannot build upstream uri: {error}"),
        )
    })?;
    request.set_uri(parsed);
    Ok(())
}

fn apply_base_path(request: &mut RequestHeader, base_path: &str) -> Result<()> {
    if base_path.is_empty() {
        return Ok(());
    }
    let mut path = format!("{base_path}{}", request.uri.path());
    if let Some(query) = request.uri.query() {
        path.push('?');
        path.push_str(query);
    }
    let uri = path.parse().map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("cannot build upstream uri: {error}"),
        )
    })?;
    request.set_uri(uri);
    Ok(())
}
