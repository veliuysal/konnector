use crate::configs;
use crate::file_log;
use crate::proxy::{reload_routing, SharedRouting};
use crate::tcp_proxy::{self, TcpProxyManager};
use notify::{EventKind, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{sync::mpsc, thread, time::Duration};

fn watcher_log(level: &str, message: &str) {
    match level {
        "ERROR" => log::error!("{message}"),
        "WARN" => log::warn!("{message}"),
        _ => log::info!("{message}"),
    }
    file_log::write_watcher("config", &file_log::format_line(level, message));
}

pub fn start(routing: SharedRouting, tcp_manager: Arc<TcpProxyManager>) {
    let directory = configs::config_dir();
    thread::Builder::new()
        .name("config-watcher".to_owned())
        .spawn(move || {
            let (sender, receiver) = mpsc::channel();
            let mut watcher = match notify::recommended_watcher(sender) {
                Ok(watcher) => watcher,
                Err(error) => {
                    watcher_log("ERROR", &format!("cannot create config watcher: {error}"));
                    return;
                }
            };
            if let Err(error) = watcher.watch(&directory, RecursiveMode::NonRecursive) {
                watcher_log(
                    "ERROR",
                    &format!("cannot watch {}: {error}", directory.display()),
                );
                return;
            }
            watcher_log(
                "INFO",
                &format!("watching configuration directory {}", directory.display()),
            );

            while let Ok(event) = receiver.recv() {
                match event {
                    Err(error) => {
                        watcher_log("WARN", &format!("configuration watch error: {error}"));
                        continue;
                    }
                    Ok(event) if is_noise_event(&event.kind) => continue,
                    Ok(event) => {
                        let paths = format_paths(&event.paths);
                        thread::sleep(Duration::from_millis(750));
                        while receiver.try_recv().is_ok() {}
                        watcher_log(
                            "INFO",
                            &format!("configuration change detected ({paths}); reloading"),
                        );
                        reload_routing(&routing);
                        tcp_proxy::reload(&tcp_manager);
                    }
                }
            }
        })
        .unwrap_or_else(|error| panic!("cannot start config watcher: {error}"));
}

fn is_noise_event(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Access(_))
}

fn format_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "unknown path".to_owned();
    }
    paths
        .iter()
        .map(|path| display_name(path))
        .collect::<Vec<_>>()
        .join(", ")
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}
