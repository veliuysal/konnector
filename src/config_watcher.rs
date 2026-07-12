use crate::proxy::{reload_routing, SharedRouting};
use crate::configs;
use notify::{RecursiveMode, Watcher};
use std::{sync::mpsc, thread, time::Duration};

pub fn start(routing: SharedRouting) {
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
                thread::sleep(Duration::from_millis(750));
                while receiver.try_recv().is_ok() {}

                reload_routing(&routing);
            }
        })
        .unwrap_or_else(|error| panic!("cannot start config watcher: {error}"));
}
