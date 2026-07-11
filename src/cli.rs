use serde::Deserialize;
use std::{
    env,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

const APP_DIR: &str = "/opt/konnector";
const DEFAULT_CONFIG_DIR: &str = "/opt/konnector/current/configs";
const SERVICE: &str = "konnector.service";
const PACKAGE: &str = "konnector";
const HEALTH_URL: &str = "http://127.0.0.1/_health";
const KEEP_RELEASES: usize = 5;
const DEFAULT_GITHUB_REPO: &str = "veliuysal/konnector";
const SERVICE_UNIT: &str = include_str!("../debian/konnector.service");
const ENV_EXAMPLE: &str = include_str!("../debian/konnector.env.example");

pub fn is_admin_command(args: &[String]) -> bool {
    let Some(command) = args.first().map(String::as_str) else {
        return false;
    };
    matches!(
        command,
        "start"
            | "stop"
            | "restart"
            | "reload"
            | "enable"
            | "disable"
            | "status"
            | "health"
            | "logs"
            | "install"
            | "update"
            | "upgrade"
            | "remove"
            | "uninstall"
            | "purge"
            | "init"
            | "tags"
            | "releases"
            | "current"
            | "build-deb"
            | "version"
            | "help"
            | "-h"
            | "--help"
            | "-V"
            | "--version"
    )
}

pub fn run(args: &[String]) -> i32 {
    match dispatch(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn dispatch(args: &[String]) -> Result<(), String> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    match command {
        "start" => cmd_start(),
        "stop" => cmd_stop(),
        "restart" => cmd_restart(),
        "reload" => cmd_reload(),
        "enable" => cmd_enable(),
        "disable" => cmd_disable(),
        "status" => cmd_status(),
        "health" => cmd_health(),
        "logs" => cmd_logs(&args[1..]),
        "install" => cmd_install(&args[1..]),
        "update" | "upgrade" => cmd_update(&args[1..]),
        "remove" | "uninstall" => cmd_remove(),
        "purge" => cmd_purge(),
        "init" => cmd_init(),
        "tags" => cmd_tags(),
        "releases" => cmd_releases(),
        "current" => cmd_current(),
        "build-deb" => cmd_build_deb(),
        "version" | "-V" | "--version" => cmd_version(),
        "help" | "-h" | "--help" | "" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("unknown command: {other}")),
    }
}

fn print_usage() {
    println!(
        r#"Konnector server control.

Service:
  konnector start
  konnector stop
  konnector restart
  konnector reload
  konnector enable
  konnector disable
  konnector status
  konnector health
  konnector logs [--follow] [--lines N]

Release:
  konnector install [tag|package.deb|archive.tar.gz|release-url]
  konnector install --tag v0.1.0
  konnector update [tag]
  konnector upgrade [tag]
  konnector tags
  konnector init
  konnector releases
  konnector current
  konnector remove
  konnector purge

Build:
  konnector build-deb

Info:
  konnector version

Run without arguments to start the proxy server.

Examples:
  sudo ./konnector install
  sudo konnector install v0.1.0
  konnector tags
  sudo konnector update
  sudo konnector start
  konnector status
  konnector logs --follow"#
    );
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn require_root(action: &str) -> Result<(), String> {
    if !is_root() {
        return Err(format!("run as root: sudo konnector {action}"));
    }
    Ok(())
}

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

fn service_installed() -> bool {
    Path::new("/lib/systemd/system/konnector.service").is_file()
}

fn require_service() -> Result<(), String> {
    if service_installed() {
        Ok(())
    } else {
        Err("konnector is not installed; run: sudo konnector install".into())
    }
}

fn download_file(source: &str, destination: &Path) -> Result<(), String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        run_command(
            "curl",
            &[
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--output",
                destination.to_str().ok_or("invalid destination path")?,
                source,
            ],
        )
    } else {
        fs::copy(source, destination)
            .map(|_| ())
            .map_err(|error| format!("cannot copy {source}: {error}"))
    }
}

fn ensure_layout() -> Result<(), String> {
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
    for (path, mode) in [
        (APP_DIR, "755"),
        (&format!("{APP_DIR}/releases"), "755"),
        ("/etc/ssl/konnector", "750"),
        ("/etc/konnector", "755"),
    ] {
        run_command(
            "install",
            &["-d", "-o", "konnector", "-g", "konnector", "-m", mode, path],
        )?;
    }
    if !Path::new("/etc/konnector.env").is_file() {
        fs::write("/etc/konnector.env", ENV_EXAMPLE)
            .map_err(|error| format!("cannot write /etc/konnector.env: {error}"))?;
        run_command("chmod", &["640", "/etc/konnector.env"])?;
        run_command("chown", &["root:konnector", "/etc/konnector.env"])?;
    }
    ensure_config_dir_env()?;
    ensure_sudoers()
}

fn ensure_config_dir_env() -> Result<(), String> {
    let path = Path::new("/etc/konnector.env");
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("cannot read /etc/konnector.env: {error}"))?;
    if contents.lines().any(|line| line.starts_with("CONFIG_DIR=")) {
        return Ok(());
    }
    let mut updated = contents;
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&format!("CONFIG_DIR={DEFAULT_CONFIG_DIR}\n"));
    fs::write(path, updated)
        .map_err(|error| format!("cannot update /etc/konnector.env: {error}"))?;
    Ok(())
}

fn copy_configs_to(release_dir: &Path) -> Result<(), String> {
    let destination = release_dir.join("configs");
    if Path::new("/usr/share/konnector/configs").is_dir() {
        run_command(
            "cp",
            &[
                "-a",
                "/usr/share/konnector/configs/.",
                &destination.display().to_string(),
            ],
        )?;
        return Ok(());
    }
    if Path::new("configs").is_dir() {
        run_command(
            "cp",
            &["-a", "configs/.", &destination.display().to_string()],
        )?;
        return Ok(());
    }
    Err("configs directory not found in package or release".into())
}

fn install_service_file() -> Result<(), String> {
    fs::write("/lib/systemd/system/konnector.service", SERVICE_UNIT)
        .map_err(|error| format!("cannot write service file: {error}"))?;
    run_command("systemctl", &["daemon-reload"])?;
    Ok(())
}

fn install_cli_symlink() -> Result<(), String> {
    let runtime = PathBuf::from(format!("{APP_DIR}/current/{PACKAGE}"));
    if !runtime.is_file() {
        return Ok(());
    }
    fs::remove_file("/usr/bin/konnector").ok();
    std::os::unix::fs::symlink(&runtime, "/usr/bin/konnector")
        .map_err(|error| format!("cannot link /usr/bin/konnector: {error}"))?;
    Ok(())
}

fn ensure_sudoers() -> Result<(), String> {
    let body = format!(
        "konnector ALL=(root) NOPASSWD: /usr/bin/konnector\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl start {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl stop {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl restart {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl reload {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl enable {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl disable {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl is-active {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl is-enabled {SERVICE}\n\
         konnector ALL=(root) NOPASSWD: /usr/bin/systemctl status {SERVICE}\n"
    );
    fs::write("/etc/sudoers.d/konnector-deploy", body)
        .map_err(|error| format!("cannot write sudoers file: {error}"))?;
    run_command("chmod", &["440", "/etc/sudoers.d/konnector-deploy"])?;
    run_command("visudo", &["-cf", "/etc/sudoers.d/konnector-deploy"])
}

fn activate_release(release_dir: &Path) -> Result<(), String> {
    let current = PathBuf::from(format!("{APP_DIR}/current"));
    let previous_release = current.read_link().ok().and_then(|path| path.canonicalize().ok());
    fs::remove_file(&current).ok();
    std::os::unix::fs::symlink(release_dir, &current)
        .map_err(|error| format!("cannot activate release: {error}"))?;
    install_cli_symlink()?;

    if run_output("systemctl", &["is-active", "--quiet", SERVICE]).is_ok() {
        run_command("systemctl", &["restart", SERVICE])?;
    } else {
        run_command("systemctl", &["start", SERVICE])?;
    }

    for attempt in 1..=15 {
        if run_output("systemctl", &["is-active", "--quiet", SERVICE]).is_ok() {
            break;
        }
        println!("Waiting for service: {attempt}/15");
        thread::sleep(Duration::from_secs(2));
        if attempt == 15 {
            if let Some(previous) = previous_release {
                fs::remove_file(&current).ok();
                std::os::unix::fs::symlink(&previous, &current).ok();
                install_cli_symlink().ok();
                let _ = run_command("systemctl", &["restart", SERVICE]);
            }
            return Err("service failed to start".into());
        }
    }

    for attempt in 1..=20 {
        if run_command("curl", &["--fail", "--silent", "--show-error", "--max-time", "5", HEALTH_URL])
            .is_ok()
        {
            println!("Release active: {}", release_dir.display());
            prune_releases()?;
            return Ok(());
        }
        println!("Waiting for health endpoint: {attempt}/20");
        thread::sleep(Duration::from_secs(3));
    }

    if let Some(previous) = previous_release {
        fs::remove_file(&current).ok();
        std::os::unix::fs::symlink(&previous, &current).ok();
        install_cli_symlink().ok();
        let _ = run_command("systemctl", &["restart", SERVICE]);
    }
    Err("health check failed".into())
}

fn prune_releases() -> Result<(), String> {
    let releases = PathBuf::from(format!("{APP_DIR}/releases"));
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

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn github_repo() -> String {
    let value = env::var("KONNECTOR_GITHUB_REPO").unwrap_or_else(|_| DEFAULT_GITHUB_REPO.to_owned());
    normalize_github_repo(&value)
}

fn normalize_github_repo(value: &str) -> String {
    let value = value.trim().trim_end_matches('/');
    if let Some(rest) = value.strip_prefix("https://github.com/") {
        return rest.trim_end_matches(".git").to_owned();
    }
    if let Some(rest) = value.strip_prefix("http://github.com/") {
        return rest.trim_end_matches(".git").to_owned();
    }
    if let Some(rest) = value.strip_prefix("github.com/") {
        return rest.trim_end_matches(".git").to_owned();
    }
    value.trim_end_matches(".git").to_owned()
}

fn github_api_release(path: &str) -> Result<GithubRelease, String> {
    let repo = github_repo();
    let body = run_output(
        "curl",
        &[
            "--fail",
            "--silent",
            "--show-error",
            "-H",
            "Accept: application/vnd.github+json",
            &format!("https://api.github.com/repos/{repo}{path}"),
        ],
    )?;
    serde_json::from_str(&body).map_err(|error| format!("invalid GitHub release response: {error}"))
}

fn release_package_url(release: GithubRelease) -> Result<String, String> {
    if let Some(url) = release
        .assets
        .iter()
        .find(|asset| asset.name.starts_with("konnector-v") && asset.name.ends_with(".tar.gz"))
        .map(|asset| asset.browser_download_url.clone())
    {
        return Ok(url);
    }
    release
        .assets
        .iter()
        .find(|asset| asset.name.starts_with("konnector_") && asset.name.ends_with("_amd64.deb"))
        .map(|asset| asset.browser_download_url.clone())
        .ok_or_else(|| format!("no release package found for {}", release.tag_name))
}

fn latest_release_package_url() -> Result<String, String> {
    release_package_url(github_api_release("/releases/latest")?)
}

fn release_package_url_for_tag(tag: &str) -> Result<String, String> {
    release_package_url(github_api_release(&format!(
        "/releases/tags/{}",
        normalize_tag(tag)
    ))?)
}

fn normalize_tag(tag: &str) -> String {
    let tag = tag.trim();
    if tag.starts_with('v') {
        tag.to_owned()
    } else {
        format!("v{tag}")
    }
}

fn is_local_package(reference: &str) -> bool {
    reference.ends_with(".tar.gz")
        || reference.ends_with(".tgz")
        || reference.ends_with(".deb")
        || Path::new(reference).exists()
}

fn resolve_release_source(reference: Option<&str>) -> Result<String, String> {
    let Some(reference) = reference else {
        return latest_release_package_url();
    };
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return Ok(reference.to_owned());
    }
    if is_local_package(reference) {
        return Ok(reference.to_owned());
    }
    release_package_url_for_tag(reference)
}

fn is_deb_package(source: &str) -> bool {
    source.ends_with(".deb")
        || source.contains("/konnector_")
            && source.contains("_amd64.deb")
}

fn install_deb_package(path: &Path) -> Result<(), String> {
    run_command("apt-get", &["update"])?;
    run_command(
        "apt-get",
        &[
            "install",
            "-y",
            "-o",
            "DEBIAN_FRONTEND=noninteractive",
            path.to_str().ok_or("invalid deb path")?,
        ],
    )
}

fn install_package(source: &str) -> Result<(), String> {
    if source.starts_with("http://") || source.starts_with("https://") || Path::new(source).is_file() {
        if is_deb_package(source) {
            let package = temp_path("konnector-package");
            download_file(source, &package)?;
            return install_deb_package(&package);
        }
    } else if is_deb_package(source) {
        return install_deb_package(Path::new(source));
    }
    install_tarball(source)
}

fn install_tarball(source: &str) -> Result<(), String> {
    ensure_layout()?;
    install_service_file()?;
    let release_id = format!("{}-{}", utc_timestamp(), short_hash(source));
    let release_dir = PathBuf::from(format!("{APP_DIR}/releases/{release_id}"));
    let archive = temp_path("konnector-archive");
    download_file(source, &archive)?;
    verify_checksum(source)?;
    fs::create_dir_all(&release_dir)
        .map_err(|error| format!("cannot create release directory: {error}"))?;
    run_command(
        "tar",
        &[
            "-xzf",
            archive.to_str().ok_or("invalid archive path")?,
            "-C",
            &release_dir.display().to_string(),
        ],
    )?;
    if !release_dir.join("configs").is_dir() {
        return Err(format!(
            "release archive is missing configs/: {}",
            release_dir.display()
        ));
    }
    run_command(
        "chmod",
        &[
            "755",
            release_dir
                .join(PACKAGE)
                .to_str()
                .ok_or("invalid binary path")?,
        ],
    )?;
    run_command(
        "chown",
        &[
            "-R",
            "konnector:konnector",
            &release_dir.display().to_string(),
        ],
    )?;
    fs::remove_file(archive).ok();
    activate_release(&release_dir)
}

fn parse_release_reference(args: &[String]) -> Result<Option<String>, String> {
    let mut reference = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--tag" | "-t" => {
                index += 1;
                reference = Some(
                    args.get(index)
                        .ok_or("missing value after --tag")?
                        .clone(),
                );
            }
            value => {
                if reference.is_some() {
                    return Err(format!("unexpected argument: {value}"));
                }
                reference = Some(value.to_owned());
            }
        }
        index += 1;
    }
    Ok(reference)
}

fn verify_checksum(source: &str) -> Result<(), String> {
    let checksum_path = format!("{source}.sha256");
    let checksum = Path::new(&checksum_path);
    if !checksum.is_file() {
        return Ok(());
    }
    let parent = Path::new(source)
        .parent()
        .and_then(|path| path.to_str())
        .unwrap_or(".");
    let file_name = checksum
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid checksum file name")?;
    run_command(
        "bash",
        &[
            "-c",
            &format!("cd {parent} && sha256sum --check {file_name}"),
        ],
    )
}

fn cmd_start() -> Result<(), String> {
    require_root("start")?;
    require_service()?;
    run_command("systemctl", &["start", SERVICE])
}

fn cmd_stop() -> Result<(), String> {
    require_root("stop")?;
    require_service()?;
    run_command("systemctl", &["stop", SERVICE])
}

fn cmd_restart() -> Result<(), String> {
    require_root("restart")?;
    require_service()?;
    run_command("systemctl", &["restart", SERVICE])
}

fn cmd_reload() -> Result<(), String> {
    require_root("reload")?;
    require_service()?;
    if run_command("systemctl", &["reload", SERVICE]).is_err() {
        run_command("systemctl", &["restart", SERVICE])?;
    }
    Ok(())
}

fn cmd_enable() -> Result<(), String> {
    require_root("enable")?;
    require_service()?;
    run_command("systemctl", &["enable", SERVICE])
}

fn cmd_disable() -> Result<(), String> {
    require_root("disable")?;
    require_service()?;
    run_command("systemctl", &["disable", SERVICE])
}

fn cmd_status() -> Result<(), String> {
    require_service()?;
    run_command("systemctl", &["status", SERVICE, "--no-pager"])
}

fn cmd_health() -> Result<(), String> {
    run_command("curl", &["--fail", "--silent", "--show-error", "--max-time", "5", HEALTH_URL])
}

fn cmd_logs(args: &[String]) -> Result<(), String> {
    require_service()?;
    let mut follow = false;
    let mut lines = "100".to_owned();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--follow" | "-f" => follow = true,
            "--lines" | "-n" => {
                index += 1;
                lines = args.get(index).cloned().unwrap_or_else(|| "100".to_owned());
            }
            value => return Err(format!("unknown logs option: {value}")),
        }
        index += 1;
    }
    let mut command = vec!["-u", SERVICE, "-n", lines.as_str(), "--no-pager"];
    if follow {
        command.push("--follow");
    }
    run_command("journalctl", &command)
}

fn cmd_install(args: &[String]) -> Result<(), String> {
    require_root("install")?;
    let reference = parse_release_reference(args)?;
    let source = resolve_release_source(reference.as_deref())?;
    println!("Installing from: {source}");
    install_package(&source)?;
    if service_installed() {
        run_command("systemctl", &["enable", SERVICE]).ok();
    }
    println!("Konnector installed.");
    Ok(())
}

fn cmd_update(args: &[String]) -> Result<(), String> {
    require_root("update")?;
    let reference = parse_release_reference(args)?;
    let source = resolve_release_source(reference.as_deref())?;
    println!("Updating from: {source}");
    install_package(&source)?;
    println!("Konnector updated.");
    Ok(())
}

fn cmd_tags() -> Result<(), String> {
    let repo = github_repo();
    let body = run_output(
        "curl",
        &[
            "--fail",
            "--silent",
            "--show-error",
            "-H",
            "Accept: application/vnd.github+json",
            &format!("https://api.github.com/repos/{repo}/releases?per_page=50"),
        ],
    )?;
    let releases: Vec<GithubRelease> = serde_json::from_str(&body)
        .map_err(|error| format!("invalid GitHub releases response: {error}"))?;
    if releases.is_empty() {
        return Err("no GitHub releases found".into());
    }
    for release in releases {
        println!("{}", release.tag_name);
    }
    Ok(())
}

fn cmd_remove() -> Result<(), String> {
    require_root("remove")?;
    run_command("systemctl", &["stop", SERVICE]).ok();
    run_command("systemctl", &["disable", SERVICE]).ok();
    fs::remove_file("/lib/systemd/system/konnector.service").ok();
    run_command("systemctl", &["daemon-reload"]).ok();
    println!("Konnector service removed. Runtime data kept in {APP_DIR}.");
    Ok(())
}

fn cmd_purge() -> Result<(), String> {
    require_root("purge")?;
    cmd_remove()?;
    fs::remove_dir_all(APP_DIR).ok();
    fs::remove_file("/etc/konnector.env").ok();
    fs::remove_file("/etc/sudoers.d/konnector-deploy").ok();
    fs::remove_file("/usr/bin/konnector").ok();
    println!("Konnector purged.");
    Ok(())
}

fn cmd_init() -> Result<(), String> {
    require_root("init")?;
    ensure_layout()?;
    install_service_file()?;
    let binary = current_binary();
    if binary.is_file() {
        let version = env::var("KONNECTOR_VERSION").unwrap_or_else(|_| "manual".to_owned());
        let release_dir = PathBuf::from(format!("{APP_DIR}/releases/pkg-{version}"));
        fs::create_dir_all(&release_dir)
            .map_err(|error| format!("cannot create release directory: {error}"))?;
        fs::copy(&binary, release_dir.join(PACKAGE))
            .map_err(|error| format!("cannot install runtime binary: {error}"))?;
        copy_configs_to(&release_dir)?;
        run_command(
            "chown",
            &[
                "-R",
                "konnector:konnector",
                &release_dir.display().to_string(),
            ],
        )?;
        fs::remove_file(format!("{APP_DIR}/current")).ok();
        std::os::unix::fs::symlink(&release_dir, format!("{APP_DIR}/current"))
            .map_err(|error| format!("cannot link current release: {error}"))?;
        install_cli_symlink()?;
    }
    require_service()?;
    run_command("systemctl", &["enable", SERVICE])?;
    if run_command("systemctl", &["restart", SERVICE]).is_err() {
        run_command("systemctl", &["start", SERVICE])?;
    }
    println!("Konnector initialized.");
    Ok(())
}

fn current_binary() -> PathBuf {
    env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/bin/konnector"))
}

fn cmd_releases() -> Result<(), String> {
    let releases = PathBuf::from(format!("{APP_DIR}/releases"));
    if !releases.is_dir() {
        return Err("no releases directory".into());
    }
    let mut entries = fs::read_dir(releases)
        .map_err(|error| format!("cannot read releases: {error}"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    entries.sort();
    entries.reverse();
    for entry in entries {
        println!("{entry}");
    }
    Ok(())
}

fn cmd_current() -> Result<(), String> {
    let current = PathBuf::from(format!("{APP_DIR}/current"));
    let path = current
        .read_link()
        .or_else(|_| current.canonicalize())
        .map_err(|_| "no active release".to_string())?;
    println!("{}", path.display());
    Ok(())
}

fn project_root() -> Result<PathBuf, String> {
    if let Ok(dir) = env::var("CARGO_MANIFEST_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let cwd = env::current_dir().map_err(|error| format!("cannot read working directory: {error}"))?;
    if cwd.join("debian/control").is_file() {
        return Ok(cwd);
    }
    Err("run konnector build-deb from the source checkout".into())
}

fn cmd_build_deb() -> Result<(), String> {
    require_root("build-deb")?;
    run_command("apt-get", &["update"])?;
    run_command(
        "apt-get",
        &[
            "install",
            "-y",
            "debhelper",
            "cargo",
            "rustc",
            "libssl-dev",
            "pkg-config",
            "clang",
            "cmake",
            "perl",
        ],
    )?;
    let manifest_dir = project_root()?;
    let status = Command::new("dpkg-buildpackage")
        .args(["-us", "-uc", "-b"])
        .current_dir(&manifest_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("cannot run dpkg-buildpackage: {error}"))?;
    if status.success() {
        println!("Debian package built in parent directory.");
        Ok(())
    } else {
        Err(format!(
            "dpkg-buildpackage exited with status {}",
            status.code().unwrap_or(-1)
        ))
    }
}

fn cmd_version() -> Result<(), String> {
    let runtime = PathBuf::from(format!("{APP_DIR}/current/{PACKAGE}"));
    if runtime.is_file() {
        println!("runtime: {}", runtime.display());
    } else {
        println!("runtime: not installed");
    }
    Ok(())
}

fn temp_path(prefix: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!(
        "{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    path
}

fn utc_timestamp() -> String {
    run_output("date", &["-u", "+%Y%m%d%H%M%S"]).unwrap_or_else(|_| "manual".to_owned())
}

fn short_hash(value: &str) -> String {
    let output = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(value.as_bytes())?;
            }
            child.wait_with_output()
        });
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("0000000")
            .chars()
            .take(7)
            .collect(),
        _ => "0000000".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_admin_commands() {
        assert!(is_admin_command(&["status".to_owned()]));
        assert!(is_admin_command(&["install".to_owned()]));
        assert!(is_admin_command(&["tags".to_owned()]));
        assert!(is_admin_command(&["build-deb".to_owned()]));
        assert!(!is_admin_command(&["deploy".to_owned()]));
        assert!(!is_admin_command(&[]));
    }

    #[test]
    fn normalizes_release_tags() {
        assert_eq!(normalize_tag("0.1.0"), "v0.1.0");
        assert_eq!(normalize_tag("v0.1.0"), "v0.1.0");
    }

    #[test]
    fn classifies_local_packages() {
        assert!(is_local_package("konnector-v0.1.0.tar.gz"));
        assert!(is_local_package("konnector_0.1.0-1_amd64.deb"));
        assert!(!is_local_package("v0.1.0"));
    }

    #[test]
    fn selects_deb_when_tarball_missing() {
        let release = GithubRelease {
            tag_name: "v0.1.0".to_owned(),
            assets: vec![GithubAsset {
                name: "konnector_0.1.0-1_amd64.deb".to_owned(),
                browser_download_url: "https://example.com/konnector.deb".to_owned(),
            }],
        };
        assert_eq!(
            release_package_url(release).unwrap(),
            "https://example.com/konnector.deb"
        );
    }

    #[test]
    fn normalizes_github_repo_urls() {
        assert_eq!(
            normalize_github_repo("https://github.com/veliuysal/konnector.git"),
            "veliuysal/konnector"
        );
        assert_eq!(normalize_github_repo("veliuysal/konnector"), "veliuysal/konnector");
    }
}
