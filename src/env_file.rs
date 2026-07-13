use std::{
    env, fs,
    path::Path,
};

/// Load `KEY=VALUE` pairs from an env file without overriding existing variables.
pub fn load_if_present(path: &Path) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || env::var_os(key).is_some() {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        env::set_var(key, value);
    }
}
