use crate::configs::{
    AccessPolicy, HttpVersion, ProxyTarget, RedirectMatch, ServerConfig, SiteConfig,
    TcpProxyConfig, UpstreamConfig,
};
use crate::ssl;
use std::{collections::HashSet, path::Path};

pub fn filter_valid_sites(sites: Vec<SiteConfig>) -> Vec<SiteConfig> {
    let mut domains = HashSet::new();
    let mut valid = Vec::new();
    for site in sites {
        if !site.enabled {
            log::info!("site {} is disabled", site.primary_domain());
            continue;
        }
        let mut trial = domains.clone();
        match validate_site(&site, &mut trial) {
            Ok(()) => {
                domains = trial;
                valid.push(site);
            }
            Err(error) => log::error!("skipping {}: {error}", site.primary_domain()),
        }
    }
    valid
}

pub fn filter_valid_tcp(proxies: Vec<TcpProxyConfig>) -> Vec<TcpProxyConfig> {
    let mut listens = HashSet::<u16>::new();
    let mut valid = Vec::new();
    for proxy in proxies {
        if !proxy.enabled {
            log::info!("tcp {} is disabled", proxy.name);
            continue;
        }
        match validate_tcp(&proxy, &mut listens) {
            Ok(()) => valid.push(proxy),
            Err(error) => log::error!("skipping tcp {}: {error}", proxy.name),
        }
    }
    valid
}

fn validate_tcp(proxy: &TcpProxyConfig, listens: &mut HashSet<u16>) -> Result<(), String> {
    let label = if proxy.name.is_empty() {
        "tcp proxy"
    } else {
        proxy.name.as_str()
    };
    if proxy.listen.trim().is_empty() {
        return Err(format!("{label} listen address must not be empty"));
    }
    let port = proxy
        .listen_port()
        .map_err(|error| format!("{label}: {error}"))?;
    if !listens.insert(port) {
        return Err(format!("duplicate tcp listen port: {port}"));
    }
    proxy.upstream.address().map_err(|error| format!("{label}: {error}"))?;
    Ok(())
}

pub fn validate_server(root: &ServerConfig, sites: &[SiteConfig]) -> Result<(), String> {
    if root.threads == 0 {
        return Err("threads must be greater than zero".into());
    }
    if let Some(https) = &root.https {
        if https.certificate_path.trim().is_empty() || https.private_key_path.trim().is_empty() {
            return Err("HTTPS certificate and private key must not be empty".into());
        }
        let auto = matches!(
            root.tls_provider.resolve(sites),
            crate::configs::TlsProviderKind::Acme
        );
        let cert_exists = Path::new(&https.certificate_path).is_file();
        let key_exists = Path::new(&https.private_key_path).is_file();
        if !auto {
            if !cert_exists {
                return Err(format!(
                    "HTTPS certificate file does not exist: {} (set tls.auto: true to obtain it)",
                    https.certificate_path
                ));
            }
            if !key_exists {
                return Err(format!(
                    "HTTPS private key file does not exist: {}",
                    https.private_key_path
                ));
            }
            let domains = ssl::proxied_tls_domains(sites);
            ssl::validate_certificate_files(https, &domains)?;
        } else if cert_exists && key_exists {
            let domains = ssl::proxied_tls_domains(sites);
            // Allow temporary/invalid files when auto will refresh them.
            if let Err(error) = ssl::validate_certificate_files(https, &domains) {
                log::warn!("existing TLS files at configured paths are not ready yet: {error}");
            }
        }
    }
    if root.http_listen == root.https_listen {
        return Err("HTTP and HTTPS listeners must be different".into());
    }
    if let Some(root_proxy) = &root.root_proxy {
        validate_proxy_target("root proxy", root_proxy)?;
    }
    Ok(())
}

#[cfg(test)]
pub fn validate(root: &ServerConfig, sites: &[SiteConfig]) -> Result<(), String> {
    validate_server(root, sites)?;
    if sites.is_empty() {
        return Err("at least one site config is required".into());
    }
    let mut domains = HashSet::new();
    for site in sites {
        validate_site(site, &mut domains)?;
    }
    Ok(())
}

fn validate_site(site: &SiteConfig, domains: &mut HashSet<String>) -> Result<(), String> {
    if site.domains.is_empty() {
        return Err("site domains must not be empty".into());
    }
    let label = site.primary_domain();
    for domain in &site.domains {
        let normalized = crate::domain_routing::normalize_host(domain);
        if normalized.is_empty() {
            return Err(format!("{label} has an empty domain"));
        }
        if !domains.insert(normalized.to_ascii_lowercase()) {
            return Err(format!("duplicate domain: {domain}"));
        }
    }
    if let AccessPolicy::OnlyPrefixes { prefixes } = &site.access {
        if prefixes.is_empty() {
            return Err(format!("{label} has an empty URL allowlist"));
        }
        if prefixes
            .iter()
            .any(|prefix| !prefix.starts_with('/') || prefix.contains('?') || prefix.contains('#'))
        {
            return Err(format!("{label} has an invalid URL prefix"));
        }
    }
    let mut route_prefixes = HashSet::new();
    for route in &site.internal_routes {
        if !route.prefix.starts_with('/')
            || route.prefix.contains('?')
            || route.prefix.contains('#')
        {
            return Err(format!("{label} has an invalid internal route"));
        }
        if !route_prefixes.insert(route.prefix.as_str()) {
            return Err(format!("{label} has a duplicate internal route"));
        }
        validate_upstream(site, &route.upstream)?;
    }
    let mut redirects = HashSet::new();
    for rule in &site.redirects {
        if !matches!(rule.status, 301 | 302 | 307 | 308) {
            return Err(format!("{label} has an invalid redirect status"));
        }
        if !rule.from.starts_with('/') || rule.from.contains('?') || rule.from.contains('#') {
            return Err(format!("{label} has an invalid redirect source"));
        }
        if !(rule.to.starts_with('/')
            || rule.to.starts_with("http://")
            || rule.to.starts_with("https://"))
            || rule.to.contains(['\r', '\n'])
        {
            return Err(format!("{} has an invalid redirect destination", label));
        }
        let prefix = matches!(rule.match_type, RedirectMatch::Prefix);
        if prefix && (!rule.from.ends_with('/') || !rule.to.ends_with('/')) {
            return Err(format!(
                "{} prefix redirects must end source and destination with /",
                label
            ));
        }
        if !redirects.insert((rule.from.as_str(), prefix)) {
            return Err(format!("{label} has a duplicate redirect rule"));
        }
    }

    let upstreams = proxy_target_upstreams(label, &site.target)?;
    for upstream in upstreams {
        validate_upstream(site, upstream)?;
    }
    if site.cache.enabled && site.cache.max_file_bytes == 0 {
        return Err(format!("{label} has an invalid cache file limit"));
    }
    Ok(())
}

fn validate_proxy_target(label: &str, target: &ProxyTarget) -> Result<(), String> {
    for upstream in proxy_target_upstreams(label, target)? {
        if upstream.address.trim().is_empty() {
            return Err(format!("{label} has an empty upstream address"));
        }
        if upstream.tls && upstream.sni.trim().is_empty() {
            return Err(format!("{label} has a TLS upstream without SNI"));
        }
        validate_upstream_http_version(label, upstream)?;
        validate_ca(upstream)?;
    }
    Ok(())
}

fn proxy_target_upstreams<'a>(
    label: &str,
    target: &'a ProxyTarget,
) -> Result<&'a [UpstreamConfig], String> {
    match target {
        ProxyTarget::Direct { upstream } => Ok(std::slice::from_ref(upstream)),
        ProxyTarget::LoadBalanced {
            upstreams,
            health_check,
            health_check_interval_seconds,
        } => {
            if upstreams.is_empty() {
                return Err(format!("{label} requires at least one upstream"));
            }
            if *health_check && *health_check_interval_seconds == 0 {
                return Err(format!("{label} has an invalid health-check interval"));
            }
            Ok(upstreams)
        }
    }
}

fn validate_upstream(site: &SiteConfig, upstream: &UpstreamConfig) -> Result<(), String> {
    let label = site.primary_domain();
    if upstream.address.trim().is_empty() {
        return Err(format!("{label} has an empty upstream address"));
    }
    if upstream.tls && upstream.sni.trim().is_empty() {
        return Err(format!("{label} has a TLS upstream without SNI"));
    }
    validate_upstream_http_version(label, upstream)?;
    validate_ca(upstream)?;
    Ok(())
}

fn validate_upstream_http_version(label: &str, upstream: &UpstreamConfig) -> Result<(), String> {
    if upstream.http_version == HttpVersion::Http3 && !upstream.tls {
        return Err(format!("{label} requires TLS for HTTP/3 upstream connections"));
    }
    if upstream.http_version == HttpVersion::Http3 && upstream.sni.trim().is_empty() {
        return Err(format!("{label} requires SNI for HTTP/3 upstream connections"));
    }
    if upstream.http_version == HttpVersion::Http2 && !upstream.tls {
        return Err(format!(
            "{label} requires TLS for HTTP/2 upstream connections"
        ));
    }
    Ok(())
}

fn validate_ca(upstream: &UpstreamConfig) -> Result<(), String> {
    let Some(path) = &upstream.ca_path else {
        return Ok(());
    };
    if !upstream.tls {
        return Err("a custom upstream CA requires TLS".into());
    }
    if !Path::new(path).is_file() {
        return Err(format!("upstream CA file does not exist: {path}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configs;
    use std::collections::HashSet;

    #[test]
    fn rust_configuration_is_valid() {
        let sites = configs::load_sites().unwrap();
        validate(&configs::server(), &sites).unwrap();
    }

    #[test]
    fn disabled_site_is_skipped() {
        let site: SiteConfig = serde_yaml::from_str(
            "enabled: false\ndomains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\n",
        )
        .unwrap();
        assert!(!site.enabled);
        let valid = crate::validation::filter_valid_sites(vec![site]);
        assert!(valid.is_empty());
    }

    #[test]
    fn disabled_tcp_proxy_is_skipped() {
        let proxy: TcpProxyConfig = serde_yaml::from_str(
            "enabled: false\nname: postgres\nlisten: 5432\nupstream:\n  instance: localhost\n",
        )
        .unwrap();
        assert!(!proxy.enabled);
        let valid = crate::validation::filter_valid_tcp(vec![proxy]);
        assert!(valid.is_empty());
    }

    #[test]
    fn http2_upstream_requires_tls() {
        let site: SiteConfig = serde_yaml::from_str(
            "domains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\n    version: \"2\"\n",
        )
        .unwrap();
        let mut domains = HashSet::new();
        let error = validate_site(&site, &mut domains).unwrap_err();
        assert!(error.contains("HTTP/2"));
    }

    #[test]
    fn http3_upstream_requires_tls_and_sni() {
        let site: SiteConfig = serde_yaml::from_str(
            "domains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\n    version: \"3\"\n",
        )
        .unwrap();
        let mut domains = HashSet::new();
        let error = validate_site(&site, &mut domains).unwrap_err();
        assert!(error.contains("HTTP/3"));
    }
}
