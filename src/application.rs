use crate::{
    config_watcher,
    configs,
    file_log,
    proxy::{build_proxy_routing, shared, DomainProxy},
    ssl, ssl_watcher, tcp_proxy::TcpProxyManager, validation,
};
use pingora::prelude::*;
use std::sync::Arc;

pub fn run() {
    file_log::init();
    init_logging();
    configs::warn_root_file(&configs::config_dir());
    let sites = validation::filter_valid_sites(configs::load_sites_lenient());
    if sites.is_empty() {
        log::warn!("no valid site configs loaded; working page will be used for unmatched hosts");
    } else {
        for site in &sites {
            let stem = if site.source_file.is_empty() {
                site.primary_domain().to_owned()
            } else {
                site.source_file.clone()
            };
            let detail = format!("domains={}", site.domains.join(", "));
            file_log::prepare_site(&stem, &detail);
            log::info!("loaded site {stem}.yaml -> {}", site.domains.join(", "));
        }
    }
    let root = configs::server();
    if root.root_proxy.is_some() {
        let level = if root.logging.is_enabled() {
            "on"
        } else {
            "off"
        };
        file_log::prepare_site("root", &format!("root proxy enabled; logging={level}"));
    }
    if root.root_proxy.is_some() && !root.logging.is_enabled() {
        log::warn!(
            "root proxy is enabled but logging.level is off; localhost and unmatched host requests will not be logged"
        );
    }
    if let Err(error) = validation::validate_server(&root, &sites) {
        log::error!("server configuration issue: {error}");
    }
    let provider = root.tls_provider.clone();
    let mut acme_bootstrap = false;
    if let Some(https) = &root.https {
        ssl::ensure_valid_certificate(https, &sites, &provider)
            .unwrap_or_else(|error| panic!("TLS certificate error: {error}"));
        if provider.resolve(&sites) == configs::TlsProviderKind::Acme {
            let domains = ssl::proxied_tls_domains(&sites);
            match ssl::validate_certificate_files(https, &domains) {
                Ok(()) if !crate::acme::certificate_expires_within(&https.certificate_path, 30) => {}
                _ => acme_bootstrap = true,
            }
        }
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
    ssl_watcher::start(root.clone(), sites.clone());
    if acme_bootstrap {
        if let Some(https) = root.https.clone() {
            crate::acme::start_background_issuer(
                https,
                ssl::proxied_tls_domains(&sites),
                provider.clone(),
            );
        }
    }

    let mut service = http_proxy_service(&server.configuration, DomainProxy::new(routing));
    assert_port_available(&root.http_listen);
    service.add_tcp(&root.http_listen);
    if let Some(https) = root.https {
        log::info!(
            "HTTPS enabled on {} using certificate {}",
            root.https_listen,
            https.certificate_path
        );
        let mut tls = pingora::listeners::tls::TlsSettings::intermediate(
            &https.certificate_path,
            &https.private_key_path,
        )
        .unwrap_or_else(|error| panic!("cannot load HTTPS certificate from {}: {error}", https.certificate_path));
        tls.enable_h2();
        service.add_tls_with_settings(&root.https_listen, None, tls);
    }
    server.add_service(service);
    server.run_forever();
}

fn init_logging() {
    use env_logger::{Builder, Env, Target};
    use std::io::Write;

    let mut builder = Builder::from_env(Env::default().default_filter_or("info"));
    builder
        .filter_module("pingora", log::LevelFilter::Off)
        .filter_module("pingora_proxy", log::LevelFilter::Off)
        .filter_module("pingora_core", log::LevelFilter::Off)
        .filter_module("pingora_cache", log::LevelFilter::Off)
        .filter_module("pingora_load_balancing", log::LevelFilter::Off)
        .target(Target::Pipe(Box::new(file_log::MainTee::new())))
        .format(|buf, record| {
            writeln!(
                buf,
                "{} [{} konnector] {}",
                crate::platform_ops::log_timestamp(),
                record.level(),
                record.args()
            )?;
            // systemd captures stderr as a pipe; flush so watcher/startup lines
            // show up immediately instead of waiting for a later burst of logs.
            buf.flush()
        })
        .init();
}

fn assert_port_available(addr: &str) {
    match std::net::TcpListener::bind(addr) {
        Ok(listener) => drop(listener),
        Err(error) => {
            let port = addr.rsplit_once(':').map(|(_, port)| port).unwrap_or("80");
            let hint = bind_error_hint(port, error.raw_os_error());
            eprintln!(
                "Cannot bind {addr}: {error}\n{hint}"
            );
            std::process::exit(1);
        }
    }
}

#[cfg(unix)]
fn bind_error_hint(port: &str, code: Option<i32>) -> String {
    if code == Some(13) {
        "Permission denied binding to a privileged port.\n\
         Do not start the proxy with bare `konnector`. Use:\n\
           sudo systemctl start konnector\n\
           konnector status\n\
         Or grant the capability:\n\
           sudo apt install -y libcap2-bin\n\
           sudo setcap cap_net_bind_service=+ep /opt/konnector/current/konnector"
            .into()
    } else if code == Some(98) {
        format!(
            "Port {port} is already in use. Konnector may already be running.\n\
             Use CLI commands instead of starting a second instance:\n\
               konnector status\n\
               konnector health\n\
               sudo systemctl restart konnector"
        )
    } else {
        format!(
            "Check which process owns the port:\n\
               sudo ss -tlnp | grep ':{port} '"
        )
    }
}

#[cfg(windows)]
fn bind_error_hint(port: &str, code: Option<i32>) -> String {
    // WSAEACCES = 10013, WSAEADDRINUSE = 10048
    if code == Some(10013) || code == Some(5) {
        format!(
            "Access denied binding to port {port}.\n\
             Run an elevated shell, or start the Windows service:\n\
               konnector start\n\
               konnector status\n\
             Privileged ports (<1024) require Administrator on Windows."
        )
    } else if code == Some(10048) {
        format!(
            "Port {port} is already in use. Konnector may already be running.\n\
             Use:\n\
               konnector status\n\
               konnector health\n\
               konnector restart"
        )
    } else {
        format!(
            "Check which process owns the port:\n\
               netstat -ano | findstr :{port}"
        )
    }
}
