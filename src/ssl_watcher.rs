use crate::{configs, ssl};
use notify::{RecursiveMode, Watcher};
use std::{
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

const RESTART_EXIT_CODE: i32 = 75;

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
                    log::error!("cannot create TLS watcher: {error}");
                    return;
                }
            };
            for path in &watch_paths {
                if let Err(error) = watcher.watch(path, RecursiveMode::NonRecursive) {
                    log::error!("cannot watch {}: {error}", path.display());
                    return;
                }
                log::info!("watching TLS directory {}", path.display());
            }

            loop {
                match receiver.recv_timeout(check_interval) {
                    Ok(Ok(_event)) => {
                        thread::sleep(Duration::from_millis(750));
                        while receiver.try_recv().is_ok() {}
                        handle_tls_change(&https, &sites, &provider, "file change");
                    }
                    Ok(Err(error)) => log::warn!("TLS watch error: {error}"),
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
    let domains = ssl::proxied_tls_domains(sites);
    let kind = provider.resolve(sites);
    let needs_refresh = match ssl::validate_certificate_files(https, &domains) {
        Ok(()) => {
            if kind == configs::TlsProviderKind::Acme
                && crate::acme::certificate_expires_within(&https.certificate_path, 30)
            {
                true
            } else if reason == "file change" {
                log::info!("valid TLS certificate change detected; restarting");
                std::process::exit(RESTART_EXIT_CODE);
            } else {
                false
            }
        }
        Err(error) => {
            log::warn!("TLS certificate check failed during {reason}: {error}");
            true
        }
    };
    if !needs_refresh {
        return;
    }
    match ssl::refresh_certificate(https, sites, provider) {
        Ok(()) => match ssl::validate_certificate_files(https, &domains) {
            Ok(()) => {
                log::info!("TLS certificate refreshed successfully; restarting");
                std::process::exit(RESTART_EXIT_CODE);
            }
            Err(error) => log::error!("refreshed TLS certificate is still invalid: {error}"),
        },
        Err(error) => log::error!("TLS certificate refresh failed: {error}"),
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
