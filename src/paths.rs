use std::path::{Path, PathBuf};

#[cfg(windows)]
const APP_DIR: &str = r"C:\Program Files\Konnector";
#[cfg(unix)]
const APP_DIR: &str = "/opt/konnector";

#[cfg(windows)]
const DATA_DIR: &str = r"C:\ProgramData\Konnector";
#[cfg(unix)]
const DATA_DIR: &str = "/etc/konnector";

#[cfg(windows)]
pub const SERVICE_NAME: &str = "Konnector";
#[cfg(unix)]
pub const SERVICE_NAME: &str = "konnector.service";

#[cfg(windows)]
pub const BINARY_NAME: &str = "konnector.exe";
#[cfg(unix)]
pub const BINARY_NAME: &str = "konnector";

pub fn app_dir() -> PathBuf {
    PathBuf::from(APP_DIR)
}

pub fn releases_dir() -> PathBuf {
    app_dir().join("releases")
}

pub fn current_dir() -> PathBuf {
    app_dir().join("current")
}

pub fn current_binary() -> PathBuf {
    current_dir().join(BINARY_NAME)
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(DATA_DIR)
}

pub fn env_file() -> PathBuf {
    #[cfg(windows)]
    {
        data_dir().join("konnector.env")
    }
    #[cfg(unix)]
    {
        PathBuf::from("/etc/konnector.env")
    }
}

pub fn ssl_dir() -> PathBuf {
    #[cfg(windows)]
    {
        data_dir().join("ssl")
    }
    #[cfg(unix)]
    {
        PathBuf::from("/etc/ssl/konnector")
    }
}

#[cfg(windows)]
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

#[cfg(windows)]
pub fn log_file() -> PathBuf {
    logs_dir().join("konnector.log")
}

pub fn default_config_dir() -> PathBuf {
    current_dir().join("configs")
}

pub fn production_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let persistent = data_dir().join("configs");
        if persistent.is_dir() {
            return persistent;
        }
        default_config_dir()
    }
    #[cfg(unix)]
    {
        default_config_dir()
    }
}

pub fn cli_link_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\Program Files\Konnector\konnector.exe")
    }
    #[cfg(unix)]
    {
        PathBuf::from("/usr/bin/konnector")
    }
}

pub fn service_unit_path() -> PathBuf {
    PathBuf::from("/lib/systemd/system/konnector.service")
}

pub fn path_display(path: &Path) -> String {
    path.display().to_string()
}
