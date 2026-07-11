use crate::{
    config_watcher,
    configs::{self, ProxyTarget},
    proxy::{DomainProxy, SiteRuntime, UpstreamRuntime},
    ssl, ssl_watcher, upstreams, validation,
};
use pingora::{cache::MemCache, prelude::*};
use std::sync::Arc;

pub fn run() {
    env_logger::init();
    let root = configs::server();
    let sites =
        configs::load_sites().unwrap_or_else(|error| panic!("configuration error: {error}"));
    let provider = configs::tls_provider();
    if let Some(https) = &root.https {
        ssl::ensure_valid_certificate(https, &sites, &provider)
            .unwrap_or_else(|error| panic!("TLS certificate error: {error}"));
    }
    validation::validate(&root, &sites)
        .unwrap_or_else(|error| panic!("configuration error: {error}"));
    config_watcher::start(root.clone());
    ssl_watcher::start(root.clone(), sites.clone());

    let mut server = Server::new(None).expect("failed to create server");
    {
        let conf = Arc::get_mut(&mut server.configuration).expect("configuration already shared");
        conf.threads = root.threads;
        conf.listener_tasks_per_fd = root.threads.clamp(1, 4);
    }
    server.bootstrap();

    let mut runtimes = Vec::with_capacity(sites.len());
    for site in sites {
        let primary_domain = site.primary_domain().to_owned();
        let target = match site.target {
            ProxyTarget::Direct { upstream } => UpstreamRuntime::Direct(upstream),
            ProxyTarget::LoadBalanced {
                upstreams: pool,
                health_check,
                health_check_interval_seconds,
            } => {
                let load_balancer = upstreams::create_load_balancer(
                    &primary_domain,
                    &pool,
                    health_check,
                    health_check_interval_seconds,
                );
                let background =
                    background_service(&format!("{primary_domain} health check"), load_balancer);
                let runtime = UpstreamRuntime::LoadBalanced {
                    upstreams: pool,
                    load_balancer: background.task(),
                };
                server.add_service(background);
                runtime
            }
        };
        runtimes.push(SiteRuntime {
            domains: site.domains,
            target,
            internal_routes: site.internal_routes,
            redirects: site.redirects,
            access: site.access,
            cache: site.cache,
            cache_storage: Box::leak(Box::new(MemCache::new())),
            forwarding: site.forwarding,
        });
    }

    let root_site = root.root_proxy.map(|root_proxy| {
        let index = runtimes.len();
        runtimes.push(SiteRuntime {
            domains: Vec::new(),
            target: UpstreamRuntime::Direct(root_proxy),
            internal_routes: Vec::new(),
            redirects: Vec::new(),
            access: configs::AccessPolicy::All,
            cache: configs::CacheConfig {
                enabled: false,
                max_file_bytes: 0,
            },
            cache_storage: Box::leak(Box::new(MemCache::new())),
            forwarding: configs::ForwardingConfig::Direct,
        });
        index
    });

    let mut service =
        http_proxy_service(&server.configuration, DomainProxy::new(runtimes, root_site));
    assert_port_available(&root.http_listen);
    service.add_tcp(&root.http_listen);
    if let Some(https) = root.https {
        let mut tls = pingora::listeners::tls::TlsSettings::intermediate(
            &https.certificate_path,
            &https.private_key_path,
        )
        .unwrap_or_else(|error| panic!("cannot load HTTPS certificate: {error}"));
        tls.enable_h2();
        service.add_tls_with_settings(&root.https_listen, None, tls);
        log::info!("HTTPS enabled on {}", root.https_listen);
    }
    server.add_service(service);
    server.run_forever();
}

fn assert_port_available(addr: &str) {
    match std::net::TcpListener::bind(addr) {
        Ok(listener) => drop(listener),
        Err(error) => {
            eprintln!(
                "Cannot bind {addr}: {error}\n\
                 Another process is already using this port.\n\
                 Stop it first:\n\
                   pkill -f konnector\n\
                 Then check the port is free:\n\
                   lsof -i :{port}",
                port = addr.rsplit_once(':').map(|(_, port)| port).unwrap_or("80")
            );
            std::process::exit(1);
        }
    }
}
