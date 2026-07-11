use crate::{configs, validation};
use notify::{RecursiveMode, Watcher};
use std::{sync::mpsc, thread, time::Duration};

pub fn start(_root: configs::ServerConfig) {
    let directory = configs::config_dir();
    thread::Builder::new()
        .name("config-watcher".to_owned())
        .spawn(move || {
            let (sender, receiver) = mpsc::channel();
            let mut watcher = match notify::recommended_watcher(sender) {
                Ok(watcher) => watcher,
                Err(error) => {
                    log::error!("cannot create config watcher: {error}");
                    return;
                }
            };
            if let Err(error) = watcher.watch(&directory, RecursiveMode::NonRecursive) {
                log::error!("cannot watch {}: {error}", directory.display());
                return;
            }
            log::info!("watching configuration directory {}", directory.display());

            while let Ok(event) = receiver.recv() {
                if let Err(error) = event {
                    log::warn!("configuration watch error: {error}");
                    continue;
                }
                // Editors and deployment tools commonly emit several events for
                // one atomic file replacement. Wait until writes settle.
                thread::sleep(Duration::from_millis(750));
                while receiver.try_recv().is_ok() {}

                match configs::load_sites_from(&directory).and_then(|sites| {
                    let root = configs::server();
                    validation::validate(&root, &sites)
                })
                {
                    Ok(()) => {
                        log::info!("valid configuration change detected; restarting");
                        // systemd Restart=on-failure starts a fresh process that
                        // builds all proxy and health-check services consistently.
                        std::process::exit(75);
                    }
                    Err(error) => {
                        log::warn!("configuration change rejected: {error}");
                    }
                }
            }
        })
        .unwrap_or_else(|error| panic!("cannot start config watcher: {error}"));
}
