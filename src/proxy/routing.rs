use super::{SiteRuntime, UpstreamRuntime};
use crate::{
    configs::{self, AccessPolicy, CacheConfig, ForwardingConfig, LogLevel, ProxyTarget, SiteConfig},
    upstreams, validation,
};
use pingora::{cache::MemCache, prelude::*};
use std::sync::{Arc, RwLock};

pub struct ProxyRouting {
    pub sites: Vec<SiteRuntime>,
    pub root_site: Option<usize>,
    pub default_logging: LogLevel,
}

pub type SharedRouting = Arc<RwLock<Arc<ProxyRouting>>>;

pub fn shared(routing: ProxyRouting) -> SharedRouting {
    Arc::new(RwLock::new(Arc::new(routing)))
}

pub fn snapshot(routing: &SharedRouting) -> Arc<ProxyRouting> {
    routing.read().expect("routing lock poisoned").clone()
}

pub fn build_proxy_routing(
    site_configs: Vec<SiteConfig>,
    root_proxy: Option<ProxyTarget>,
    default_logging: LogLevel,
    mut server: Option<&mut Server>,
) -> ProxyRouting {
    let mut sites = Vec::with_capacity(site_configs.len());
    for site in site_configs {
        let primary_domain = site.primary_domain().to_owned();
        let logging = site.resolved_logging(default_logging);
        let target = upstream_runtime_from_target(
            &primary_domain,
            site.target,
            server.as_deref_mut(),
        );
        sites.push(SiteRuntime {
            domains: site.domains,
            target,
            internal_routes: site.internal_routes,
            redirects: site.redirects,
            access: site.access,
            cache: site.cache,
            cache_storage: Box::leak(Box::new(MemCache::new())),
            forwarding: site.forwarding,
            logging,
            source_file: if site.source_file.is_empty() {
                primary_domain.clone()
            } else {
                site.source_file
            },
        });
    }

    let root_site = root_proxy.map(|root_proxy| {
        let index = sites.len();
        let target = upstream_runtime_from_target("root", root_proxy, server.as_deref_mut());
        sites.push(SiteRuntime {
            domains: Vec::new(),
            target,
            internal_routes: Vec::new(),
            redirects: Vec::new(),
            access: AccessPolicy::All,
            cache: CacheConfig {
                enabled: false,
                max_file_bytes: 0,
            },
            cache_storage: Box::leak(Box::new(MemCache::new())),
            forwarding: ForwardingConfig::Direct,
            logging: default_logging,
            source_file: "root".to_owned(),
        });
        index
    });

    ProxyRouting {
        sites,
        root_site,
        default_logging,
    }
}

fn upstream_runtime_from_target(
    name: &str,
    target: ProxyTarget,
    server: Option<&mut Server>,
) -> UpstreamRuntime {
    match target {
        ProxyTarget::Direct { upstream } => UpstreamRuntime::Direct(upstream),
        ProxyTarget::LoadBalanced {
            upstreams: pool,
            health_check,
            health_check_interval_seconds,
        } => {
            let load_balancer = upstreams::create_load_balancer(
                name,
                &pool,
                health_check,
                health_check_interval_seconds,
            );
            match server {
                Some(server) => {
                    let background =
                        background_service(&format!("{name} health check"), load_balancer);
                    let runtime = UpstreamRuntime::LoadBalanced {
                        upstreams: pool,
                        load_balancer: background.task(),
                    };
                    server.add_service(background);
                    runtime
                }
                None => UpstreamRuntime::LoadBalanced {
                    upstreams: pool,
                    load_balancer: Arc::new(load_balancer),
                },
            }
        }
    }
}

pub fn reload_routing(routing: &SharedRouting) {
    let directory = configs::config_dir();
    configs::warn_root_file(&directory);
    let sites = validation::filter_valid_sites(configs::load_sites_from_lenient(&directory));
    let root = configs::server();
    if let Err(error) = validation::validate_server(&root, &sites) {
        log::error!("configuration reload skipped: {error}");
        return;
    }
    for site in &sites {
        let stem = if site.source_file.is_empty() {
            site.primary_domain().to_owned()
        } else {
            site.source_file.clone()
        };
        crate::file_log::prepare_site(
            &stem,
            &format!("reloaded; domains={}", site.domains.join(", ")),
        );
    }
    let new_routing = build_proxy_routing(sites, root.root_proxy.clone(), root.logging, None);
    let root_enabled = new_routing.root_site.is_some();
    let named_sites = if root_enabled {
        new_routing.sites.len().saturating_sub(1)
    } else {
        new_routing.sites.len()
    };
    if root_enabled {
        crate::file_log::prepare_site("root", "root proxy reloaded");
    }
    {
        let mut guard = routing.write().expect("routing lock poisoned");
        *guard = Arc::new(new_routing);
    }
    if root_enabled {
        log::info!("configuration reloaded: {named_sites} site(s), root proxy enabled");
    } else {
        log::info!("configuration reloaded: {named_sites} site(s), root proxy disabled");
    }
}
