use crate::paths;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

static MAIN_LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Ensure `logs/main`, `logs/watchers`, and known site folders exist.
pub fn init() {
    let root = paths::logs_dir();
    for sub in ["main", "watchers"] {
        let path = root.join(sub);
        if let Err(error) = fs::create_dir_all(&path) {
            eprintln!(
                "konnector: cannot create log directory {}: {error}",
                path.display()
            );
        }
    }
    let main_path = main_log_path();
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&main_path)
    {
        Ok(file) => {
            if let Ok(mut guard) = MAIN_LOG.lock() {
                *guard = Some(file);
            }
        }
        Err(error) => {
            eprintln!(
                "konnector: cannot open main log {}: {error}",
                main_path.display()
            );
        }
    }
    log_line(
        "main",
        "konnector.log",
        &format!("file logging enabled under {}", root.display()),
    );
}

/// Create `logs/{stem}/` and record that this YAML is enabled and loggable.
pub fn prepare_site(config_stem: &str, detail: &str) {
    let stem = sanitize_name(config_stem);
    let dir = paths::logs_dir().join(&stem);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!(
            "konnector: cannot create site log directory {}: {error}",
            dir.display()
        );
        return;
    }
    write_site(
        &stem,
        &format_line("INFO", &format!("site enabled; {detail}")),
    );
}

pub fn main_log_path() -> PathBuf {
    paths::log_file()
}

pub fn site_log_path(config_stem: &str) -> PathBuf {
    paths::logs_dir()
        .join(sanitize_name(config_stem))
        .join("access.log")
}

pub fn watcher_log_path(watcher: &str) -> PathBuf {
    paths::logs_dir()
        .join("watchers")
        .join(format!("{}.log", sanitize_name(watcher)))
}

/// Append a line to the main application log (also used by the env_logger tee).
pub fn write_main(line: &str) {
    if let Ok(mut guard) = MAIN_LOG.lock() {
        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
            return;
        }
    }
    // Fallback if init() has not opened the handle yet.
    append_file(&main_log_path(), line);
}

/// Access / proxy log for a YAML config stem (`root`, `example`, `postgres.tcp`, …).
pub fn write_site(config_stem: &str, line: &str) {
    let path = site_log_path(config_stem);
    ensure_parent(&path);
    append_file(&path, line);
}

/// Watcher-specific log (`config`, `tls`, …).
pub fn write_watcher(watcher: &str, line: &str) {
    let path = watcher_log_path(watcher);
    ensure_parent(&path);
    append_file(&path, line);
}

pub fn format_line(level: &str, message: &str) -> String {
    format!("[{level} konnector] {message}")
}

fn log_line(folder: &str, file: &str, message: &str) {
    let path = paths::logs_dir().join(folder).join(file);
    ensure_parent(&path);
    append_file(&path, &format_line("INFO", message));
}

fn ensure_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
}

fn append_file(path: &Path, line: &str) {
    ensure_parent(path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

fn sanitize_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "unknown".to_owned();
    }
    trimmed
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect()
}

/// Writer that mirrors each line to stderr and `logs/main/konnector.log`.
pub struct MainTee {
    stderr: std::io::Stderr,
}

impl MainTee {
    pub fn new() -> Self {
        Self {
            stderr: std::io::stderr(),
        }
    }
}

impl Write for MainTee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.stderr.write_all(buf);
        let _ = self.stderr.flush();
        if let Ok(text) = std::str::from_utf8(buf) {
            for line in text.lines() {
                if !line.is_empty() {
                    write_main(line);
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stderr.flush()?;
        if let Ok(mut guard) = MAIN_LOG.lock() {
            if let Some(file) = guard.as_mut() {
                file.flush()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_names() {
        assert_eq!(sanitize_name("example"), "example");
        assert_eq!(sanitize_name("postgres.tcp"), "postgres.tcp");
        assert_eq!(sanitize_name("../evil"), ".._evil");
        assert_eq!(sanitize_name(""), "unknown");
    }
}
