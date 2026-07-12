use crate::{
    config_watcher,
    configs,
    proxy::{build_proxy_routing, shared, DomainProxy},
    ssl, ssl_watcher, tcp_proxy::TcpProxyManager, validation,
};
use pingora::prelude::*;
use std::sync::Arc;

pub fn run() {
    init_logging();
    configs::warn_root_file(&configs::config_dir());
    let sites = validation::filter_valid_sites(configs::load_sites_lenient());
    if sites.is_empty() {
        log::warn!("no valid site configs loaded; working page will be used for unmatched hosts");
    }
    let root = configs::server();
    if root.root_proxy.is_some() && !root.logging.is_enabled() {
        log::warn!(
            "root proxy is enabled but logging.level is off; localhost and unmatched host requests will not be logged"
        );
    }
    if let Err(error) = validation::validate_server(&root, &sites) {
        log::error!("server configuration issue: {error}");
    }
    let provider = configs::tls_provider();
    if let Some(https) = &root.https {
        ssl::ensure_valid_certificate(https, &sites, &provider)
            .unwrap_or_else(|error| panic!("TLS certificate error: {error}"));
    }

    let mut server = Server::new(None).expect("failed to create server");
    {
        let conf = Arc::get_mut(&mut server.configuration).expect("configuration already shared");
        conf.threads = root.threads;
        conf.listener_tasks_per_fd = root.threads.clamp(1, 4);
    }
    server.bootstrap();

    let routing = shared(build_proxy_routing(
        sites.clone(),
        root.root_proxy.clone(),
        root.logging,
        Some(&mut server),
    ));
    let tcp_manager = TcpProxyManager::new();
    tcp_manager.apply(validation::filter_valid_tcp(configs::load_tcp_lenient()), root.logging);
    config_watcher::start(routing.clone(), tcp_manager.clone());
    ssl_watcher::start(root.clone(), sites);

    let mut service = http_proxy_service(&server.configuration, DomainProxy::new(routing));
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

fn init_logging() {
    use env_logger::{Builder, Env};
    Builder::from_env(Env::default().default_filter_or("info"))
        .filter_module("pingora", log::LevelFilter::Off)
        .filter_module("pingora_proxy", log::LevelFilter::Off)
        .filter_module("pingora_core", log::LevelFilter::Off)
        .filter_module("pingora_cache", log::LevelFilter::Off)
        .filter_module("pingora_load_balancing", log::LevelFilter::Off)
        .format(|buf, record| {
            use std::io::Write;
            writeln!(
                buf,
                "[{} konnector] {}",
                record.level(),
                record.args()
            )
        })
        .init();
}

fn assert_port_available(addr: &str) {
    match std::net::TcpListener::bind(addr) {
        Ok(listener) => drop(listener),
        Err(error) => {
            let port = addr.rsplit_once(':').map(|(_, port)| port).unwrap_or("80");
            let hint = if error.raw_os_error() == Some(13) {
                "Permission denied binding to a privileged port.\n\
                 Do not start the proxy with bare `konnector`. Use:\n\
                   sudo systemctl start konnector\n\
                   konnector status\n\
                 Or grant the capability:\n\
                   sudo apt install -y libcap2-bin\n\
                   sudo setcap cap_net_bind_service=+ep /opt/konnector/current/konnector"
            } else if error.raw_os_error() == Some(98) {
                "Port {port} is already in use. Konnector may already be running.\n\
                 Use CLI commands instead of starting a second instance:\n\
                   konnector status\n\
                   konnector health\n\
                   sudo systemctl restart konnector"
            } else {
                "Check which process owns the port:\n\
                   sudo ss -tlnp | grep ':{port} '"
            };
            eprintln!(
                "Cannot bind {addr}: {error}\n{hint}\n\
                 Then check the port is free:\n\
                   sudo ss -tlnp | grep ':{port} '",
            );
            std::process::exit(1);
        }
    }
}
