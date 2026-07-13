use crate::{configs, file_log, ssl};
use notify::{RecursiveMode, Watcher};
use std::{
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

const RESTART_EXIT_CODE: i32 = 75;

fn watcher_log(level: &str, message: &str) {
    match level {
        "ERROR" => log::error!("{message}"),
        "WARN" => log::warn!("{message}"),
        _ => log::info!("{message}"),
    }
    file_log::write_watcher("tls", &file_log::format_line(level, message));
}

pub fn start(root: configs::ServerConfig, sites: Vec<configs::SiteConfig>) {
    let Some(https) = root.https.clone() else {
        return;
    };
    let provider = root.tls_provider.clone();
    let check_interval = Duration::from_secs(provider.check_interval_seconds);
    let watch_paths = ssl::watch_paths(&https);

    thread::Builder::new()
        .name("ssl-watcher".to_owned())
        .spawn(move || {
            let (sender, receiver) = mpsc::channel();
            let mut watcher = match notify::recommended_watcher(sender) {
                Ok(watcher) => watcher,
                Err(error) => {
                    watcher_log("ERROR", &format!("cannot create TLS watcher: {error}"));
                    return;
                }
            };
            for path in &watch_paths {
                if let Err(error) = watcher.watch(path, RecursiveMode::NonRecursive) {
                    watcher_log(
                        "ERROR",
                        &format!("cannot watch {}: {error}", path.display()),
                    );
                    return;
                }
                watcher_log(
                    "INFO",
                    &format!("watching TLS directory {}", path.display()),
                );
            }

            loop {
                match receiver.recv_timeout(check_interval) {
                    Ok(Ok(event)) => {
                        let paths = event
                            .paths
                            .iter()
                            .map(|path| {
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| path.display().to_string())
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        let label = if paths.is_empty() {
                            "unknown path".to_owned()
                        } else {
                            paths
                        };
                        watcher_log(
                            "INFO",
                            &format!("TLS file change detected ({label}); checking certificate"),
                        );
                        thread::sleep(Duration::from_millis(750));
                        while receiver.try_recv().is_ok() {}
                        handle_tls_change(&https, &sites, &provider, "file change");
                    }
                    Ok(Err(error)) => {
                        watcher_log("WARN", &format!("TLS watch error: {error}"));
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        handle_tls_change(&https, &sites, &provider, "scheduled check");
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .unwrap_or_else(|error| panic!("cannot start TLS watcher: {error}"));
}

fn handle_tls_change(
    https: &configs::HttpsConfig,
    sites: &[configs::SiteConfig],
    provider: &configs::TlsProviderConfig,
    reason: &str,
) {
    let kind = provider.resolve(sites);
    let domains = ssl::certificate_domains(sites, kind);

    // Temporary self-signed placeholders expire in 7 days and would otherwise look
    // "soon to renew" forever, which re-hammered Let's Encrypt after every write.
    if kind == configs::TlsProviderKind::Acme {
        if let Some(remaining) = crate::acme::backoff_remaining(provider) {
            if reason == "file change" {
                // Self-signed / failed-issue writes must not kick off another ACME order.
                return;
            }
            watcher_log(
                "INFO",
                &format!(
                    "skipping ACME retry ({reason}); paused for {remaining}s after recent failure"
                ),
            );
            return;
        }
        if sites.iter().any(|site| {
            matches!(
                site.forwarding,
                configs::ForwardingConfig::Cloudflare
            )
        }) && provider
            .cloudflare_api_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .is_none()
        {
            watcher_log(
                "WARN",
                "site uses forwarding: cloudflare but CLOUDFLARE_API_TOKEN is unset; \
                 HTTP-01 often fails behind orange-cloud — set the token for Origin CA",
            );
        }
    }

    let needs_refresh = match ssl::validate_certificate_files(https, &domains) {
        Ok(()) => {
            let renew_soon = matches!(
                kind,
                configs::TlsProviderKind::Acme
                    | configs::TlsProviderKind::Cloudflare
                    | configs::TlsProviderKind::Command
            ) && crate::acme::certificate_expires_within(&https.certificate_path, 30);
            if renew_soon {
                // Placeholder self-signed certs are short-lived; only retry ACME on
                // the scheduled interval, not on every notify of our own write.
                if kind == configs::TlsProviderKind::Acme && reason == "file change" {
                    return;
                }
                watcher_log(
                    "INFO",
                    &format!("TLS certificate expires within 30 days; renewing ({reason})"),
                );
                true
            } else if reason == "file change" {
                watcher_log(
                    "INFO",
                    "valid TLS certificate change detected; restarting",
                );
                std::process::exit(RESTART_EXIT_CODE);
            } else {
                false
            }
        }
        Err(error) => {
            watcher_log(
                "WARN",
                &format!("TLS certificate check failed during {reason}: {error}"),
            );
            // Do not loop ACME on every placeholder rewrite while validating fails.
            if kind == configs::TlsProviderKind::Acme && reason == "file change" {
                false
            } else {
                true
            }
        }
    };
    if !needs_refresh {
        return;
    }
    match ssl::refresh_certificate(https, sites, provider) {
        Ok(()) => {
            let domains = ssl::certificate_domains(sites, kind);
            match ssl::validate_certificate_files(https, &domains) {
                Ok(()) => {
                    watcher_log(
                        "INFO",
                        "TLS certificate refreshed successfully; restarting",
                    );
                    std::process::exit(RESTART_EXIT_CODE);
                }
                Err(error) => watcher_log(
                    "ERROR",
                    &format!("refreshed TLS certificate is still invalid: {error}"),
                ),
            }
        }
        Err(error) => watcher_log(
            "ERROR",
            &format!("TLS certificate refresh failed: {error}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn watch_paths_include_certificate_parent_directory() {
        let https = configs::HttpsConfig {
            certificate_path: "/etc/ssl/konnector/fullchain.pem".to_owned(),
            private_key_path: "/etc/ssl/konnector/privkey.pem".to_owned(),
        };
        let paths = ssl::watch_paths(&https);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], Path::new("/etc/ssl/konnector"));
    }
}
