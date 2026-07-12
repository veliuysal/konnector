mod access_control;
mod application;
mod cache_policy;
mod cli;
mod config_watcher;
mod configs;
mod default_site;
mod domain_routing;
mod error_pages;
mod forwarding;
mod health_check;
mod internal_routes;
mod path_rewrite;
mod proxy;
mod redirects;
mod request_logging;
mod ssl;
mod ssl_watcher;
mod upstreams;
mod validation;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
