use crate::paths;
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const HEALTH_URL: &str = "http://127.0.0.1/_health";
const KEEP_RELEASES: usize = 5;

fn run_command(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited with status {}",
            status.code().unwrap_or(-1)
        ))
    }
}

fn run_output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} exited with status {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("invalid {program} output: {error}"))
}

pub fn health_check() -> Result<(), String> {
    let response = ureq::get(HEALTH_URL)
        .timeout(Duration::from_secs(5))
        .call()
        .map_err(|error| format!("health check failed: {error}"))?;
    let status = response.status();
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("health check failed: HTTP {status}"))
    }
}

pub fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    let (year, month, day, hour, minute, second) = civil_datetime_utc(secs);
    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

pub fn short_hash(value: &str) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
        .chars()
        .take(7)
        .collect()
}

/// Convert Unix seconds to UTC Y-M-D h:m:s (Howard Hinnant civil_from_days).
fn civil_datetime_utc(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month as u32, day, hour, minute, second)
}

pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|error| {
        format!(
            "cannot create {}: {error}",
            paths::path_display(dst)
        )
    })?;
    for entry in fs::read_dir(src).map_err(|error| {
        format!(
            "cannot read {}: {error}",
            paths::path_display(src)
        )
    })? {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot read file type: {error}"))?;
        let destination = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(entry.path())
                    .map_err(|error| format!("cannot read symlink: {error}"))?;
                std::os::unix::fs::symlink(&target, &destination)
                    .map_err(|error| format!("cannot copy symlink: {error}"))?;
            }
            #[cfg(windows)]
            {
                fs::copy(entry.path(), &destination).map_err(|error| {
                    format!(
                        "cannot copy {}: {error}",
                        paths::path_display(&entry.path())
                    )
                })?;
            }
        } else {
            fs::copy(entry.path(), &destination).map_err(|error| {
                format!(
                    "cannot copy {}: {error}",
                    paths::path_display(&entry.path())
                )
            })?;
        }
    }
    Ok(())
}

fn prune_releases() -> Result<(), String> {
    let releases = paths::releases_dir();
    let mut entries = fs::read_dir(&releases)
        .map_err(|error| format!("cannot read releases: {error}"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok());
    entries.reverse();
    for path in entries.into_iter().skip(KEEP_RELEASES) {
        fs::remove_dir_all(path).ok();
    }
    Ok(())
}

fn wait_for_health() -> Result<(), String> {
    for attempt in 1..=20 {
        if health_check().is_ok() {
            return Ok(());
        }
        println!("Waiting for health endpoint: {attempt}/20");
        thread::sleep(Duration::from_secs(3));
    }
    Err("health check failed".into())
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

#[cfg(unix)]
const SERVICE_UNIT: &str = include_str!("../debian/konnector.service");
#[cfg(unix)]
const ENV_EXAMPLE: &str = include_str!("../debian/konnector.env.example");

#[cfg(unix)]
pub fn is_elevated() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(unix)]
pub fn require_elevated(action: &str) -> Result<(), String> {
    if !is_elevated() {
        return Err(format!("run as root: sudo konnector {action}"));
    }
    Ok(())
}

#[cfg(unix)]
pub fn service_installed() -> bool {
    paths::service_unit_path().is_file()
}

#[cfg(unix)]
pub fn require_service() -> Result<(), String> {
    if service_installed() {
        Ok(())
    } else {
        Err("konnector is not installed; run: sudo konnector install".into())
    }
}

#[cfg(unix)]
pub fn ensure_layout() -> Result<(), String> {
    if run_output("id", &["konnector"]).is_err() {
        run_command(
            "adduser",
            &[
                "--system",
                "--group",
                "--home",
                "/var/lib/konnector",
                "--shell",
                "/usr/sbin/nologin",
                "konnector",
            ],
        )?;
    }
    let app = paths::path_display(&paths::app_dir());
    let releases = paths::path_display(&paths::releases_dir());
    let ssl = paths::path_display(&paths::ssl_dir());
    let data = paths::path_display(&paths::data_dir());
    let logs = paths::path_display(&paths::logs_dir());
    for (path, mode) in [
        (app.as_str(), "755"),
        (releases.as_str(), "755"),
        (ssl.as_str(), "750"),
        (data.as_str(), "755"),
        (logs.as_str(), "755"),
    ] {
        run_command(
            "install",
            &["-d", "-o", "konnector", "-g", "konnector", "-m", mode, path],
        )?;
    }
    for sub in ["main", "watchers"] {
        let path = paths::logs_dir().join(sub);
        run_command(
            "install",
            &[
                "-d",
                "-o",
                "konnector",
                "-g",
                "konnector",
                "-m",
                "755",
                &paths::path_display(&path),
            ],
        )?;
    }
    let env_path = paths::env_file();
    if !env_path.is_file() {
        fs::write(&env_path, ENV_EXAMPLE)
            .map_err(|error| format!("cannot write {}: {error}", paths::path_display(&env_path)))?;
        run_command("chmod", &["640", &paths::path_display(&env_path)])?;
        run_command(
            "chown",
            &["root:konnector", &paths::path_display(&env_path)],
        )?;
    }
    ensure_config_dir_env()?;
    ensure_sudoers()
}

#[cfg(unix)]
fn ensure_config_dir_env() -> Result<(), String> {
    let path = paths::env_file();
    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", paths::path_display(&path)))?;
    if contents.lines().any(|line| line.starts_with("CONFIG_DIR=")) {
        return Ok(());
    }
    let mut updated = contents;
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&format!(
        "CONFIG_DIR={}\n",
        paths::path_display(&paths::default_config_dir())
    ));
    fs::write(&path, updated)
        .map_err(|error| format!("cannot update {}: {error}", paths::path_display(&path)))?;
    Ok(())
}

#[cfg(unix)]
fn ensure_sudoers() -> Result<(), String> {
    let service = paths::SERVICE_NAME;
    let body = format!(
        "konnector ALL=(root) NOPASSWD: /usr/bin/konnector\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl start {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl stop {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl restart {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl reload {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl enable {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl disable {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl is-active {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl is-enabled {service}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl status {service}\n"
    );
    fs::write("/etc/sudoers.d/konnector-deploy", body)
        .map_err(|error| format!("cannot write sudoers file: {error}"))?;
    run_command("chmod", &["440", "/etc/sudoers.d/konnector-deploy"])?;
    run_command("visudo", &["-cf", "/etc/sudoers.d/konnector-deploy"])
}

#[cfg(unix)]
pub fn install_service() -> Result<(), String> {
    let unit = paths::service_unit_path();
    fs::write(&unit, SERVICE_UNIT)
        .map_err(|error| format!("cannot write service file: {error}"))?;
    run_command("systemctl", &["daemon-reload"])?;
    Ok(())
}

#[cfg(unix)]
pub fn install_cli_link() -> Result<(), String> {
    let runtime = paths::current_binary();
    if !runtime.is_file() {
        return Ok(());
    }
    grant_bind_capability(&runtime)?;
    let link = paths::cli_link_path();
    fs::remove_file(&link).ok();
    std::os::unix::fs::symlink(&runtime, &link).map_err(|error| {
        format!(
            "cannot link {}: {error}",
            paths::path_display(&link)
        )
    })?;
    Ok(())
}

#[cfg(unix)]
pub fn grant_bind_capability(binary: &Path) -> Result<(), String> {
    let path = binary
        .to_str()
        .ok_or("invalid binary path for capability grant")?;
    if run_command("setcap", &["cap_net_bind_service=+ep", path]).is_ok() {
        return Ok(());
    }
    // Install setcap only when missing; avoid apt-get on every package configure.
    if !Path::new("/sbin/setcap").is_file() && !Path::new("/usr/sbin/setcap").is_file() {
        run_command(
            "apt-get",
            &[
                "install",
                "-y",
                "-o",
                "DEBIAN_FRONTEND=noninteractive",
                "libcap2-bin",
            ],
        )
        .ok();
        if run_command("setcap", &["cap_net_bind_service=+ep", path]).is_ok() {
            return Ok(());
        }
    }
    eprintln!(
        "warning: could not grant port 80/443 binding to {path}; \
         ensure konnector runs under systemd"
    );
    Ok(())
}

#[cfg(unix)]
pub fn set_executable(binary: &Path) -> Result<(), String> {
    let path = binary
        .to_str()
        .ok_or("invalid binary path for chmod")?;
    run_command("chmod", &["755", path])
}

#[cfg(unix)]
pub fn chown_release(release_dir: &Path) -> Result<(), String> {
    run_command(
        "chown",
        &[
            "-R",
            "konnector:konnector",
            &paths::path_display(release_dir),
        ],
    )
}

#[cfg(unix)]
pub fn link_current(release_dir: &Path) -> Result<(), String> {
    let current = paths::current_dir();
    fs::remove_file(&current).ok();
    std::os::unix::fs::symlink(release_dir, &current)
        .map_err(|error| format!("cannot activate release: {error}"))
}

#[cfg(unix)]
pub fn current_release_path() -> Result<PathBuf, String> {
    let current = paths::current_dir();
    current
        .read_link()
        .or_else(|_| current.canonicalize())
        .map_err(|_| "no active release".to_string())
}

#[cfg(unix)]
pub fn activate_release(release_dir: &Path) -> Result<(), String> {
    let current = paths::current_dir();
    let previous_release = current
        .read_link()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    link_current(release_dir)?;
    install_cli_link()?;
    install_service()?;

    let service = paths::SERVICE_NAME;
    if run_output("systemctl", &["is-active", "--quiet", service]).is_ok() {
        run_command("systemctl", &["restart", service])?;
    } else {
        run_command("systemctl", &["start", service])?;
    }

    for attempt in 1..=15 {
        if run_output("systemctl", &["is-active", "--quiet", service]).is_ok() {
            break;
        }
        println!("Waiting for service: {attempt}/15");
        thread::sleep(Duration::from_secs(2));
        if attempt == 15 {
            if let Some(previous) = &previous_release {
                let _ = link_current(previous);
                install_cli_link().ok();
                let _ = run_command("systemctl", &["restart", service]);
            }
            return Err("service failed to start".into());
        }
    }

    match wait_for_health() {
        Ok(()) => {
            println!("Release active: {}", release_dir.display());
            prune_releases()?;
            Ok(())
        }
        Err(error) => {
            if let Some(previous) = previous_release {
                let _ = link_current(&previous);
                install_cli_link().ok();
                let _ = run_command("systemctl", &["restart", service]);
            }
            Err(error)
        }
    }
}

#[cfg(unix)]
pub fn extract_archive(archive: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("cannot create destination: {error}"))?;
    run_command(
        "tar",
        &[
            "-xzf",
            archive.to_str().ok_or("invalid archive path")?,
            "-C",
            &paths::path_display(destination),
        ],
    )
}

#[cfg(unix)]
pub fn package_or_runtime_installed() -> bool {
    let package_ok = run_output("dpkg-query", &["-W", "-f=${Status}", "konnector"])
        .is_ok_and(|status| status.contains("install ok installed"));
    package_ok || paths::current_binary().is_file()
}

#[cfg(unix)]
pub fn installed_package_version() -> Result<String, String> {
    run_output("dpkg-query", &["-W", "-f=${Version}", "konnector"])
}

#[cfg(unix)]
pub fn cmd_start() -> Result<(), String> {
    require_elevated("start")?;
    require_service()?;
    run_command("systemctl", &["start", paths::SERVICE_NAME])
}

#[cfg(unix)]
pub fn cmd_stop() -> Result<(), String> {
    require_elevated("stop")?;
    require_service()?;
    run_command("systemctl", &["stop", paths::SERVICE_NAME])
}

#[cfg(unix)]
pub fn cmd_restart() -> Result<(), String> {
    require_elevated("restart")?;
    require_service()?;
    run_command("systemctl", &["restart", paths::SERVICE_NAME])
}

#[cfg(unix)]
pub fn cmd_reload() -> Result<(), String> {
    require_elevated("reload")?;
    require_service()?;
    if run_command("systemctl", &["reload", paths::SERVICE_NAME]).is_err() {
        run_command("systemctl", &["restart", paths::SERVICE_NAME])?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn cmd_enable() -> Result<(), String> {
    require_elevated("enable")?;
    require_service()?;
    run_command("systemctl", &["enable", paths::SERVICE_NAME])
}

#[cfg(unix)]
pub fn cmd_disable() -> Result<(), String> {
    require_elevated("disable")?;
    require_service()?;
    run_command("systemctl", &["disable", paths::SERVICE_NAME])
}

#[cfg(unix)]
pub fn cmd_status() -> Result<(), String> {
    require_service()?;
    run_command(
        "systemctl",
        &["status", paths::SERVICE_NAME, "--no-pager"],
    )
}

pub fn cmd_logs(follow: bool, lines: &str, target: &str) -> Result<(), String> {
    let log_path = resolve_log_target(target);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    if !log_path.is_file() {
        println!(
            "No log yet at {} (start the service; site access logs appear after proxied traffic).",
            paths::path_display(&log_path)
        );
        return Ok(());
    }

    let line_count: usize = lines.parse().unwrap_or(100);
    let contents = fs::read_to_string(&log_path).unwrap_or_default();
    let all_lines: Vec<&str> = contents.lines().collect();
    let start = all_lines.len().saturating_sub(line_count);
    for line in &all_lines[start..] {
        println!("{line}");
    }

    if !follow {
        return Ok(());
    }

    let mut file = fs::OpenOptions::new()
        .read(true)
        .open(&log_path)
        .map_err(|error| {
            format!(
                "cannot open {}: {error}",
                paths::path_display(&log_path)
            )
        })?;
    file.seek(SeekFrom::End(0))
        .map_err(|error| format!("cannot seek log file: {error}"))?;
    let mut reader = BufReader::new(file);
    loop {
        let mut buffer = String::new();
        match reader.read_line(&mut buffer) {
            Ok(0) => thread::sleep(Duration::from_millis(500)),
            Ok(_) => print!("{buffer}"),
            Err(error) => return Err(format!("cannot read log file: {error}")),
        }
    }
}

fn resolve_log_target(target: &str) -> PathBuf {
    let target = target.trim().trim_matches('/');
    if target.is_empty() || target.eq_ignore_ascii_case("main") {
        return paths::log_file();
    }
    if let Some(watcher) = target
        .strip_prefix("watchers/")
        .or_else(|| target.strip_prefix("watcher/"))
    {
        return paths::logs_dir()
            .join("watchers")
            .join(format!("{watcher}.log"));
    }
    if matches!(target, "config" | "tls") {
        return paths::logs_dir()
            .join("watchers")
            .join(format!("{target}.log"));
    }
    // YAML stem → logs/{stem}/access.log
    let stem = target.trim_end_matches(".yaml").trim_end_matches(".yml");
    paths::logs_dir().join(stem).join("access.log")
}

#[cfg(unix)]
pub fn cmd_remove_service() -> Result<(), String> {
    require_elevated("remove")?;
    run_command("systemctl", &["stop", paths::SERVICE_NAME]).ok();
    run_command("systemctl", &["disable", paths::SERVICE_NAME]).ok();
    fs::remove_file(paths::service_unit_path()).ok();
    run_command("systemctl", &["daemon-reload"]).ok();
    println!(
        "Konnector service removed. Runtime data kept in {}.",
        paths::path_display(&paths::app_dir())
    );
    Ok(())
}

#[cfg(unix)]
pub fn cmd_uninstall_all() -> Result<(), String> {
    require_elevated("uninstall")?;
    run_command("systemctl", &["stop", paths::SERVICE_NAME]).ok();
    run_command("systemctl", &["disable", paths::SERVICE_NAME]).ok();
    let package_ok = run_output("dpkg-query", &["-W", "-f=${Status}", "konnector"])
        .is_ok_and(|status| status.contains("install ok installed"));
    if package_ok {
        run_command(
            "apt-get",
            &[
                "purge",
                "-y",
                "-o",
                "DEBIAN_FRONTEND=noninteractive",
                "konnector",
            ],
        )?;
    } else {
        fs::remove_file(paths::service_unit_path()).ok();
        run_command("systemctl", &["daemon-reload"]).ok();
    }
    fs::remove_dir_all(paths::app_dir()).ok();
    fs::remove_dir_all(paths::data_dir()).ok();
    fs::remove_dir_all(paths::ssl_dir()).ok();
    fs::remove_file(paths::env_file()).ok();
    fs::remove_file("/etc/sudoers.d/konnector-deploy").ok();
    fs::remove_file(paths::cli_link_path()).ok();
    run_command("deluser", &["--system", "konnector"]).ok();
    println!("Konnector uninstalled.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
const ENV_EXAMPLE: &str = include_str!("../windows/konnector.env.example");

#[cfg(windows)]
pub fn is_elevated() -> bool {
    Command::new("net")
        .arg("session")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn require_elevated(action: &str) -> Result<(), String> {
    if !is_elevated() {
        return Err(format!(
            "run as Administrator: konnector {action}"
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub fn service_installed() -> bool {
    use windows_service::{
        service::ServiceAccess,
        service_manager::{ServiceManager, ServiceManagerAccess},
    };
    let Ok(manager) =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    else {
        return false;
    };
    manager
        .open_service(paths::SERVICE_NAME, ServiceAccess::QUERY_STATUS)
        .is_ok()
}

#[cfg(windows)]
pub fn require_service() -> Result<(), String> {
    if service_installed() {
        Ok(())
    } else {
        Err("konnector is not installed; run: konnector install (as Administrator)".into())
    }
}

#[cfg(windows)]
pub fn ensure_layout() -> Result<(), String> {
    for dir in [
        paths::app_dir(),
        paths::releases_dir(),
        paths::data_dir(),
        paths::data_dir().join("configs"),
        paths::ssl_dir(),
        paths::logs_dir(),
        paths::logs_dir().join("main"),
        paths::logs_dir().join("watchers"),
    ] {
        fs::create_dir_all(&dir).map_err(|error| {
            format!(
                "cannot create {}: {error}",
                paths::path_display(&dir)
            )
        })?;
    }
    let env_path = paths::env_file();
    if !env_path.is_file() {
        fs::write(&env_path, ENV_EXAMPLE).map_err(|error| {
            format!(
                "cannot write {}: {error}",
                paths::path_display(&env_path)
            )
        })?;
    }
    // Touch log file so later writers and log viewers have a target.
    if !paths::log_file().is_file() {
        fs::write(paths::log_file(), "").map_err(|error| {
            format!(
                "cannot create log file {}: {error}",
                paths::path_display(&paths::log_file())
            )
        })?;
    }
    Ok(())
}

#[cfg(windows)]
fn service_info(
    start_type: windows_service::service::ServiceStartType,
) -> windows_service::service::ServiceInfo {
    use std::ffi::OsString;
    use windows_service::service::{
        ServiceErrorControl, ServiceInfo, ServiceType,
    };
    ServiceInfo {
        name: OsString::from(paths::SERVICE_NAME),
        display_name: OsString::from("Konnector Reverse Proxy"),
        service_type: ServiceType::OWN_PROCESS,
        start_type,
        error_control: ServiceErrorControl::Normal,
        executable_path: paths::current_binary(),
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    }
}

#[cfg(windows)]
pub fn install_service() -> Result<(), String> {
    use windows_service::{
        service::{ServiceAccess, ServiceStartType},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    require_elevated("install")?;
    ensure_layout()?;

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|error| format!("cannot open service manager: {error}"))?;

    let info = service_info(ServiceStartType::AutoStart);
    match manager.open_service(
        paths::SERVICE_NAME,
        ServiceAccess::QUERY_CONFIG | ServiceAccess::CHANGE_CONFIG,
    ) {
        Ok(service) => {
            service
                .change_config(&info)
                .map_err(|error| format!("cannot update service: {error}"))?;
        }
        Err(_) => {
            manager
                .create_service(&info, ServiceAccess::QUERY_STATUS | ServiceAccess::START)
                .map_err(|error| format!("cannot create service: {error}"))?;
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn install_cli_link() -> Result<(), String> {
    let runtime = paths::current_binary();
    if !runtime.is_file() {
        return Ok(());
    }
    let link = paths::cli_link_path();
    if link == runtime {
        return Ok(());
    }
    fs::remove_file(&link).ok();

    if std::os::windows::fs::symlink_file(&runtime, &link).is_ok() {
        return Ok(());
    }

    fs::copy(&runtime, &link).map_err(|error| {
        format!(
            "cannot install CLI at {}: {error}",
            paths::path_display(&link)
        )
    })?;
    Ok(())
}

#[cfg(windows)]
pub fn grant_bind_capability(_binary: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn set_executable(_binary: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn chown_release(_release_dir: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn remove_path_link(path: &Path) {
    // Junctions look like directories; symlinks may be files or dirs.
    let _ = fs::remove_dir(path);
    let _ = fs::remove_file(path);
}

#[cfg(windows)]
pub fn link_current(release_dir: &Path) -> Result<(), String> {
    use std::os::windows::fs::symlink_dir;

    let current = paths::current_dir();
    remove_path_link(&current);

    if symlink_dir(release_dir, &current).is_ok() {
        return Ok(());
    }

    let current_s = paths::path_display(&current);
    let release_s = paths::path_display(release_dir);
    run_command(
        "cmd",
        &["/C", "mklink", "/J", &current_s, &release_s],
    )
    .map_err(|error| format!("cannot link current release: {error}"))
}

#[cfg(windows)]
pub fn current_release_path() -> Result<PathBuf, String> {
    let current = paths::current_dir();
    fs::read_link(&current)
        .or_else(|_| current.canonicalize())
        .map_err(|_| "no active release".to_string())
}

#[cfg(windows)]
fn open_service_with(
    access: windows_service::service::ServiceAccess,
) -> Result<windows_service::service::Service, String> {
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| format!("cannot open service manager: {error}"))?;
    manager
        .open_service(paths::SERVICE_NAME, access)
        .map_err(|error| format!("cannot open service: {error}"))
}

#[cfg(windows)]
fn service_is_running() -> bool {
    use windows_service::service::{ServiceAccess, ServiceState};
    let Ok(service) = open_service_with(ServiceAccess::QUERY_STATUS) else {
        return false;
    };
    service
        .query_status()
        .map(|status| status.current_state == ServiceState::Running)
        .unwrap_or(false)
}

#[cfg(windows)]
fn wait_for_service_running() -> Result<(), String> {
    for attempt in 1..=15 {
        if service_is_running() {
            return Ok(());
        }
        println!("Waiting for service: {attempt}/15");
        thread::sleep(Duration::from_secs(2));
    }
    Err("service failed to start".into())
}

#[cfg(windows)]
fn start_service_process() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    let service = open_service_with(ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;
    match service.start(&[] as &[&str]) {
        Ok(()) => Ok(()),
        Err(error) => {
            if service_is_running() {
                Ok(())
            } else {
                Err(format!("cannot start service: {error}"))
            }
        }
    }
}

#[cfg(windows)]
fn stop_service_process() -> Result<(), String> {
    use windows_service::service::{ServiceAccess, ServiceState};
    let service = open_service_with(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?;
    match service.stop() {
        Ok(_) | Err(_) => {}
    }
    for _ in 0..30 {
        let state = service
            .query_status()
            .map(|status| status.current_state)
            .unwrap_or(ServiceState::Stopped);
        if state == ServiceState::Stopped {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err("service did not stop in time".into())
}

#[cfg(windows)]
pub fn activate_release(release_dir: &Path) -> Result<(), String> {
    let previous_release = current_release_path().ok();
    link_current(release_dir)?;
    install_cli_link()?;
    install_service()?;

    if service_is_running() {
        stop_service_process().ok();
    }
    start_service_process()?;

    if let Err(error) = wait_for_service_running() {
        if let Some(previous) = &previous_release {
            let _ = link_current(previous);
            install_cli_link().ok();
            install_service().ok();
            let _ = start_service_process();
        }
        return Err(error);
    }

    match wait_for_health() {
        Ok(()) => {
            println!("Release active: {}", release_dir.display());
            prune_releases()?;
            Ok(())
        }
        Err(error) => {
            if let Some(previous) = previous_release {
                let _ = link_current(&previous);
                install_cli_link().ok();
                install_service().ok();
                stop_service_process().ok();
                let _ = start_service_process();
            }
            Err(error)
        }
    }
}

#[cfg(windows)]
pub fn extract_archive(archive: &Path, destination: &Path) -> Result<(), String> {
    let name = archive
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return Err(
            "tar.gz packages are not supported on Windows; use a .zip package".into(),
        );
    }
    if !name.ends_with(".zip") {
        return Err(format!(
            "unsupported archive type on Windows: {}",
            paths::path_display(archive)
        ));
    }
    fs::create_dir_all(destination)
        .map_err(|error| format!("cannot create destination: {error}"))?;
    let archive_s = paths::path_display(archive);
    let dest_s = paths::path_display(destination);
    run_command(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            &format!(
                "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
                archive_s.replace('\'', "''"),
                dest_s.replace('\'', "''"),
            ),
        ],
    )
}

#[cfg(windows)]
pub fn package_or_runtime_installed() -> bool {
    service_installed() || paths::current_binary().is_file()
}

#[cfg(windows)]
pub fn installed_package_version() -> Result<String, String> {
    Err("package version is only available on Linux".into())
}

#[cfg(windows)]
pub fn cmd_start() -> Result<(), String> {
    require_elevated("start")?;
    require_service()?;
    start_service_process()
}

#[cfg(windows)]
pub fn cmd_stop() -> Result<(), String> {
    require_elevated("stop")?;
    require_service()?;
    stop_service_process()
}

#[cfg(windows)]
pub fn cmd_restart() -> Result<(), String> {
    require_elevated("restart")?;
    require_service()?;
    stop_service_process().ok();
    start_service_process()
}

#[cfg(windows)]
pub fn cmd_reload() -> Result<(), String> {
    cmd_restart()
}

#[cfg(windows)]
pub fn cmd_enable() -> Result<(), String> {
    use windows_service::service::{ServiceAccess, ServiceStartType};
    require_elevated("enable")?;
    require_service()?;
    let service = open_service_with(ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_CONFIG)?;
    service
        .change_config(&service_info(ServiceStartType::AutoStart))
        .map_err(|error| format!("cannot enable service: {error}"))?;
    println!("Service enabled (Auto start).");
    Ok(())
}

#[cfg(windows)]
pub fn cmd_disable() -> Result<(), String> {
    use windows_service::service::{ServiceAccess, ServiceStartType};
    require_elevated("disable")?;
    require_service()?;
    stop_service_process().ok();
    let service = open_service_with(ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_CONFIG)?;
    service
        .change_config(&service_info(ServiceStartType::Disabled))
        .map_err(|error| format!("cannot disable service: {error}"))?;
    println!("Service disabled.");
    Ok(())
}

#[cfg(windows)]
pub fn cmd_status() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    require_service()?;
    let service = open_service_with(ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG)?;
    let status = service
        .query_status()
        .map_err(|error| format!("cannot query status: {error}"))?;
    let config = service
        .query_config()
        .map_err(|error| format!("cannot query config: {error}"))?;
    println!("Service: {}", paths::SERVICE_NAME);
    println!("Display name: {}", config.display_name.to_string_lossy());
    println!("State: {:?}", status.current_state);
    println!("Start type: {:?}", config.start_type);
    println!(
        "Binary: {}",
        paths::path_display(&config.executable_path)
    );
    Ok(())
}

#[cfg(windows)]
pub fn cmd_remove_service() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    require_elevated("remove")?;
    if !service_installed() {
        println!("Service not installed.");
        return Ok(());
    }
    stop_service_process().ok();
    let service = open_service_with(ServiceAccess::DELETE | ServiceAccess::STOP)?;
    service
        .delete()
        .map_err(|error| format!("cannot delete service: {error}"))?;
    println!(
        "Konnector service removed. Runtime data kept in {}.",
        paths::path_display(&paths::app_dir())
    );
    Ok(())
}

#[cfg(windows)]
pub fn cmd_uninstall_all() -> Result<(), String> {
    require_elevated("uninstall")?;
    if service_installed() {
        stop_service_process().ok();
        use windows_service::service::ServiceAccess;
        if let Ok(service) = open_service_with(ServiceAccess::DELETE | ServiceAccess::STOP) {
            service.delete().ok();
        }
    }
    fs::remove_dir_all(paths::app_dir()).ok();
    fs::remove_dir_all(paths::data_dir()).ok();
    println!("Konnector uninstalled.");
    Ok(())
}
