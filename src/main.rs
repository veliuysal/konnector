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
mod ssl;
mod ssl_watcher;
mod upstreams;
mod validation;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if cli::is_admin_command(&args) {
        std::process::exit(cli::run(&args));
    }
    application::run();
}
