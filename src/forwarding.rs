use crate::{
    configs::ForwardingConfig,
    proxy::{RequestContext, SiteRuntime},
};
use pingora::prelude::*;

pub fn public_origin(session: &Session, site: &SiteRuntime) -> String {
    format!("{}://{}", public_scheme(session, site), public_host(session, site))
}

pub fn public_port(session: &Session) -> Option<u16> {
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .and_then(|host| host.rsplit_once(':').and_then(|(_, port)| port.parse().ok()))
}

pub fn public_scheme(session: &Session, site: &SiteRuntime) -> String {
    let native_scheme = if session
        .as_downstream()
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    match site.forwarding {
        ForwardingConfig::Direct => native_scheme.to_owned(),
        ForwardingConfig::Cloudflare | ForwardingConfig::TrustedProxy => session
            .req_header()
            .headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .filter(|value| *value == "http" || *value == "https")
            .unwrap_or(native_scheme)
            .to_owned(),
    }
}

pub fn public_host(session: &Session, site: &SiteRuntime) -> String {
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| site.primary_domain())
        .to_string()
}

pub async fn apply(
    session: &Session,
    request: &mut RequestHeader,
    ctx: &RequestContext,
    sites: &[SiteRuntime],
) -> Result<()> {
    let Some(site) = ctx.site.and_then(|index| sites.get(index)) else {
        return Ok(());
    };

    request.insert_header("x-forwarded-host", public_host(session, site))?;
    request.insert_header("x-forwarded-proto", public_scheme(session, site))?;
    if let Some(port) = public_port(session) {
        request.insert_header("x-forwarded-port", port.to_string())?;
    }

    // Provider-specific identity headers are only retained for explicitly
    // configured Cloudflare traffic. Network access should also be restricted
    // to Cloudflare at the firewall or tunnel layer.
    if !matches!(site.forwarding, ForwardingConfig::Cloudflare) {
        request.remove_header("cf-connecting-ip");
        request.remove_header("cf-ipcountry");
        request.remove_header("cf-ray");
    }
    if matches!(site.forwarding, ForwardingConfig::Direct) {
        request.remove_header("forwarded");
        request.remove_header("x-forwarded-for");
    }
    Ok(())
}
