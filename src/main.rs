mod access_control;
mod application;
mod cache_policy;
mod cli;
mod config_watcher;
mod configs;
mod default_site;
mod domain_routing;
mod env_file;
mod error_pages;
mod forwarding;
mod health_check;
mod http3;
mod internal_routes;
mod path_rewrite;
mod paths;
mod platform_ops;
mod proxy;
mod redirects;
mod request_logging;
mod ssl;
mod ssl_watcher;
mod tcp_proxy;
mod upstreams;
mod validation;

#[cfg(windows)]
mod windows_svc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(windows)]
    if args.is_empty() && windows_svc::try_start_dispatcher() {
        return;
    }

    env_file::load_if_present(&paths::env_file());

    if should_run_server(&args) {
        application::run();
        return;
    }
    if args.is_empty() || cli::is_admin_command(&args) {
        std::process::exit(cli::run(&args));
    }
    eprintln!("unknown command: {}", args[0]);
    std::process::exit(1);
}

fn should_run_server(args: &[String]) -> bool {
    if args.first().is_some_and(|arg| arg == "serve") {
        return true;
    }
    if !args.is_empty() {
        return false;
    }
    std::env::var("INVOCATION_ID").is_ok() || cfg!(debug_assertions)
}
