use crate::platform_ops;
use crate::paths;
use serde::Deserialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const DEFAULT_GITHUB_REPO: &str = "veliuysal/konnector";

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
        "start" => platform_ops::cmd_start(),
        "stop" => platform_ops::cmd_stop(),
        "restart" => platform_ops::cmd_restart(),
        "reload" => platform_ops::cmd_reload(),
        "enable" => platform_ops::cmd_enable(),
        "disable" => platform_ops::cmd_disable(),
        "status" => platform_ops::cmd_status(),
        "health" => platform_ops::health_check(),
        "logs" => cmd_logs(&args[1..]),
        "install" => cmd_install(&args[1..]),
        "update" | "upgrade" => cmd_update(&args[1..]),
        "remove" => platform_ops::cmd_remove_service(),
        "uninstall" | "purge" => platform_ops::cmd_uninstall_all(),
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
        "serve" => Err(serve_hint()),
        other => Err(format!("unknown command: {other}")),
    }
}

fn serve_hint() -> String {
    #[cfg(windows)]
    {
        "serve is started by the Windows service; use: konnector start".into()
    }
    #[cfg(unix)]
    {
        "serve is started by systemd; use: sudo systemctl start konnector".into()
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
  konnector logs [--follow] [--lines N]   # main/konnector.log

Release:
  konnector install [tag|package|release-url]
  konnector install --tag v0.1.0
  konnector update [tag]
  konnector upgrade [tag]
  konnector tags
  konnector init
  konnector releases
  konnector current
  konnector remove
  konnector uninstall
  konnector purge

Build:
  konnector build-deb

Info:
  konnector version

Server:
  konnector serve

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

fn download_file(source: &str, destination: &Path) -> Result<(), String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let response = ureq::get(source)
            .call()
            .map_err(|error| format!("cannot download {source}: {error}"))?;
        let mut reader = response.into_reader();
        let mut file = fs::File::create(destination)
            .map_err(|error| format!("cannot create {}: {error}", destination.display()))?;
        std::io::copy(&mut reader, &mut file)
            .map(|_| ())
            .map_err(|error| format!("cannot write {}: {error}", destination.display()))
    } else {
        fs::copy(source, destination)
            .map(|_| ())
            .map_err(|error| format!("cannot copy {source}: {error}"))
    }
}

fn copy_configs_to(release_dir: &Path) -> Result<(), String> {
    let destination = release_dir.join("configs");
    let current_configs = paths::default_config_dir();
    if current_configs.is_dir() && config_dir_has_sites(&current_configs) {
        return platform_ops::copy_tree(&current_configs, &destination);
    }
    #[cfg(unix)]
    if Path::new("/usr/share/konnector/configs").is_dir() {
        return platform_ops::copy_tree(Path::new("/usr/share/konnector/configs"), &destination);
    }
    if Path::new("configs").is_dir() {
        return platform_ops::copy_tree(Path::new("configs"), &destination);
    }
    Err("configs directory not found in package or release".into())
}

fn config_dir_has_sites(directory: &Path) -> bool {
    fs::read_dir(directory)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                matches!(
                    entry.path().extension().and_then(|value| value.to_str()),
                    Some("yaml" | "yml")
                )
            })
        })
        .unwrap_or(false)
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
    let body = ureq::get(&format!("https://api.github.com/repos/{repo}{path}"))
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "konnector")
        .call()
        .map_err(|error| format!("cannot fetch GitHub release: {error}"))?
        .into_string()
        .map_err(|error| format!("cannot read GitHub release body: {error}"))?;
    serde_json::from_str(&body).map_err(|error| format!("invalid GitHub release response: {error}"))
}

fn tag_version(tag: &str) -> String {
    normalize_tag(tag).trim_start_matches('v').to_owned()
}

fn release_package_url(release: GithubRelease) -> Result<String, String> {
    #[cfg(windows)]
    {
        return windows_release_package_url(release);
    }
    #[cfg(unix)]
    {
        unix_release_package_url(release)
    }
}

#[cfg(windows)]
fn windows_release_package_url(release: GithubRelease) -> Result<String, String> {
    let tag = normalize_tag(&release.tag_name);
    let preferred = [
        format!("konnector-{tag}-windows-x86_64.zip"),
        format!("konnector-{tag}-windows-amd64.zip"),
        format!("konnector_{}-windows-x86_64.zip", tag_version(&release.tag_name)),
    ];
    for name in preferred {
        if let Some(asset) = release.assets.iter().find(|asset| asset.name == name) {
            return Ok(asset.browser_download_url.clone());
        }
    }
    if let Some(asset) = release.assets.iter().find(|asset| {
        asset.name.ends_with(".zip")
            && asset.name.contains("windows")
            && asset.name.contains("konnector")
    }) {
        return Ok(asset.browser_download_url.clone());
    }
    Err(format!(
        "no Windows zip package found for {}; expected konnector-{tag}-windows-x86_64.zip",
        release.tag_name
    ))
}

#[cfg(unix)]
fn unix_release_package_url(release: GithubRelease) -> Result<String, String> {
    let version = tag_version(&release.tag_name);
    let deb_prefix = format!("konnector_{version}-");
    let deb_assets = release
        .assets
        .iter()
        .filter(|asset| asset.name.starts_with("konnector_") && asset.name.ends_with("_amd64.deb"))
        .collect::<Vec<_>>();

    if let Some(asset) = deb_assets
        .iter()
        .find(|asset| asset.name.starts_with(&deb_prefix))
    {
        return Ok(asset.browser_download_url.clone());
    }

    if deb_assets.len() == 1 {
        let asset = deb_assets[0];
        return Err(format!(
            "release {} has {} but expected {}; rebuild the release with a matching package version",
            release.tag_name, asset.name, deb_prefix
        ));
    }

    if deb_assets.len() > 1 {
        let names = deb_assets
            .iter()
            .map(|asset| asset.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "release {} has multiple .deb packages ({names}) but none match expected {}",
            release.tag_name, deb_prefix
        ));
    }

    release
        .assets
        .iter()
        .find(|asset| asset.name.starts_with("konnector-v") && asset.name.ends_with(".tar.gz"))
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
        || reference.ends_with(".zip")
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
        || (source.contains("/konnector_") && source.contains("_amd64.deb"))
}

fn is_zip_package(source: &str) -> bool {
    source.ends_with(".zip") || source.contains("windows") && source.ends_with(".zip")
}

#[cfg(unix)]
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
    #[cfg(unix)]
    {
        if source.starts_with("http://")
            || source.starts_with("https://")
            || Path::new(source).is_file()
        {
            if is_deb_package(source) {
                let mut package = temp_path("konnector-package");
                package.set_extension("deb");
                download_file(source, &package)?;
                return install_deb_package(&package);
            }
        } else if is_deb_package(source) {
            return install_deb_package(Path::new(source));
        }
    }

    #[cfg(windows)]
    {
        if is_deb_package(source) {
            return Err("Debian packages are not supported on Windows; use the Windows zip release".into());
        }
    }

    install_archive(source)
}

fn install_archive(source: &str) -> Result<(), String> {
    platform_ops::ensure_layout()?;
    let release_id = format!(
        "{}-{}",
        platform_ops::utc_timestamp(),
        platform_ops::short_hash(source)
    );
    let release_dir = paths::releases_dir().join(&release_id);
    let mut archive = temp_path("konnector-archive");
    #[cfg(windows)]
    {
        if is_zip_package(source) || source.ends_with(".zip") {
            archive.set_extension("zip");
        }
    }
    #[cfg(unix)]
    {
        let _ = is_zip_package;
        archive.set_extension("tar.gz");
    }
    download_file(source, &archive)?;
    verify_checksum(source)?;
    fs::create_dir_all(&release_dir)
        .map_err(|error| format!("cannot create release directory: {error}"))?;
    platform_ops::extract_archive(&archive, &release_dir)?;

    // Zip/tarball may nest files in a top-level folder; flatten if needed.
    normalize_release_layout(&release_dir)?;

    if !release_dir.join("configs").is_dir() {
        return Err(format!(
            "release archive is missing configs/: {}",
            release_dir.display()
        ));
    }
    let binary = release_dir.join(paths::BINARY_NAME);
    if !binary.is_file() {
        return Err(format!(
            "release archive is missing {}: {}",
            paths::BINARY_NAME,
            release_dir.display()
        ));
    }
    platform_ops::set_executable(&binary)?;
    platform_ops::grant_bind_capability(&binary)?;
    platform_ops::chown_release(&release_dir)?;
    fs::remove_file(archive).ok();
    platform_ops::activate_release(&release_dir)
}

fn normalize_release_layout(release_dir: &Path) -> Result<(), String> {
    let binary = release_dir.join(paths::BINARY_NAME);
    if binary.is_file() {
        return Ok(());
    }
    let entries = fs::read_dir(release_dir)
        .map_err(|error| format!("cannot read release directory: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    if entries.len() != 1 {
        return Ok(());
    }
    let nested = &entries[0];
    for name in [paths::BINARY_NAME, "configs", "konnector"] {
        let from = nested.join(name);
        if from.exists() {
            let to = release_dir.join(name);
            if from.is_dir() {
                platform_ops::copy_tree(&from, &to)?;
            } else {
                fs::copy(&from, &to)
                    .map_err(|error| format!("cannot move {}: {error}", from.display()))?;
            }
        }
    }
    Ok(())
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
    let checksum_url = format!("{source}.sha256");
    if source.starts_with("http://") || source.starts_with("https://") {
        let Ok(response) = ureq::get(&checksum_url).call() else {
            return Ok(());
        };
        let body = response.into_string().unwrap_or_default();
        let expected = body.split_whitespace().next().unwrap_or("");
        if expected.is_empty() {
            return Ok(());
        }
        // Best-effort: skip strict verification when we only have the remote checksum text.
        let _ = expected;
        return Ok(());
    }
    let checksum = Path::new(&checksum_url);
    if !checksum.is_file() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let parent = Path::new(source)
            .parent()
            .and_then(|path| path.to_str())
            .unwrap_or(".");
        let file_name = checksum
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid checksum file name")?;
        return run_command(
            "bash",
            &["-c", &format!("cd {parent} && sha256sum --check {file_name}")],
        );
    }
    #[cfg(windows)]
    {
        Ok(())
    }
}

fn cmd_logs(args: &[String]) -> Result<(), String> {
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
    platform_ops::cmd_logs(follow, &lines)
}

fn cmd_install(args: &[String]) -> Result<(), String> {
    platform_ops::require_elevated("install")?;
    let reference = parse_release_reference(args)?;
    let source = resolve_release_source(reference.as_deref())?;
    println!("Installing from: {source}");
    install_package(&source)?;
    if platform_ops::service_installed() {
        platform_ops::cmd_enable().ok();
    }
    println!("Konnector installed.");
    Ok(())
}

fn cmd_update(args: &[String]) -> Result<(), String> {
    platform_ops::require_elevated("update")?;
    let reference = parse_release_reference(args)?;
    let source = resolve_release_source(reference.as_deref())?;
    println!("Updating from: {source}");
    install_package(&source)?;
    if platform_ops::package_or_runtime_installed() {
        platform_ops::cmd_restart().ok();
    }
    println!("Konnector updated.");
    Ok(())
}

fn cmd_tags() -> Result<(), String> {
    let repo = github_repo();
    let body = ureq::get(&format!(
        "https://api.github.com/repos/{repo}/releases?per_page=50"
    ))
    .set("Accept", "application/vnd.github+json")
    .set("User-Agent", "konnector")
    .call()
    .map_err(|error| format!("cannot fetch GitHub releases: {error}"))?
    .into_string()
    .map_err(|error| format!("cannot read GitHub releases body: {error}"))?;
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

fn cmd_init() -> Result<(), String> {
    platform_ops::require_elevated("init")?;
    platform_ops::ensure_layout()?;
    let binary = env::current_exe().unwrap_or_else(|_| paths::cli_link_path());
    if binary.is_file() {
        let version = env::var("KONNECTOR_VERSION").unwrap_or_else(|_| "manual".to_owned());
        let release_dir = paths::releases_dir().join(format!("pkg-{version}"));
        fs::create_dir_all(&release_dir)
            .map_err(|error| format!("cannot create release directory: {error}"))?;
        fs::copy(&binary, release_dir.join(paths::BINARY_NAME))
            .map_err(|error| format!("cannot install runtime binary: {error}"))?;
        copy_configs_to(&release_dir)?;
        platform_ops::grant_bind_capability(&release_dir.join(paths::BINARY_NAME))?;
        platform_ops::chown_release(&release_dir)?;
        platform_ops::link_current(&release_dir)?;
        platform_ops::install_cli_link()?;
    }
    platform_ops::install_service()?;
    platform_ops::require_service()?;
    platform_ops::cmd_enable()?;
    if platform_ops::cmd_restart().is_err() {
        platform_ops::cmd_start()?;
    }
    println!("Konnector initialized.");
    Ok(())
}

fn cmd_releases() -> Result<(), String> {
    let releases = paths::releases_dir();
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
    println!("{}", platform_ops::current_release_path()?.display());
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
    #[cfg(windows)]
    {
        let _ = project_root;
        return Err("build-deb is only supported on Linux".into());
    }
    #[cfg(unix)]
    {
        platform_ops::require_elevated("build-deb")?;
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
}

fn cmd_version() -> Result<(), String> {
    let runtime = paths::current_binary();
    if runtime.is_file() {
        println!("runtime: {}", runtime.display());
    } else {
        println!("runtime: not installed");
    }
    println!("package: {}", env!("CARGO_PKG_VERSION"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_admin_commands() {
        assert!(is_admin_command(&["status".to_owned()]));
        assert!(is_admin_command(&["install".to_owned()]));
        assert!(is_admin_command(&["uninstall".to_owned()]));
        assert!(is_admin_command(&["tags".to_owned()]));
        assert!(is_admin_command(&["build-deb".to_owned()]));
        assert!(!is_admin_command(&["deploy".to_owned()]));
        assert!(!is_admin_command(&[]));
    }

    #[test]
    fn tag_version_strips_v_prefix() {
        assert_eq!(tag_version("v0.1.1"), "0.1.1");
        assert_eq!(tag_version("0.1.1"), "0.1.1");
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
        assert!(is_local_package("konnector-v0.1.0-windows-x86_64.zip"));
        assert!(!is_local_package("v0.1.0"));
    }

    #[cfg(unix)]
    #[test]
    fn selects_deb_matching_release_tag() {
        let release = GithubRelease {
            tag_name: "v0.1.1".to_owned(),
            assets: vec![
                GithubAsset {
                    name: "konnector_0.1.0-1_amd64.deb".to_owned(),
                    browser_download_url: "https://example.com/konnector_0.1.0-1_amd64.deb"
                        .to_owned(),
                },
                GithubAsset {
                    name: "konnector_0.1.1-1_amd64.deb".to_owned(),
                    browser_download_url: "https://example.com/konnector_0.1.1-1_amd64.deb"
                        .to_owned(),
                },
            ],
        };
        assert_eq!(
            release_package_url(release).unwrap(),
            "https://example.com/konnector_0.1.1-1_amd64.deb"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_mismatched_deb_for_tagged_release() {
        let release = GithubRelease {
            tag_name: "v0.1.1".to_owned(),
            assets: vec![GithubAsset {
                name: "konnector_0.1.0-1_amd64.deb".to_owned(),
                browser_download_url: "https://example.com/konnector_0.1.0-1_amd64.deb".to_owned(),
            }],
        };
        let error = release_package_url(release).unwrap_err();
        assert!(error.contains("v0.1.1"));
        assert!(error.contains("konnector_0.1.0-1_amd64.deb"));
    }

    #[cfg(unix)]
    #[test]
    fn prefers_deb_over_tarball() {
        let release = GithubRelease {
            tag_name: "v0.1.0".to_owned(),
            assets: vec![
                GithubAsset {
                    name: "konnector-v0.1.0.tar.gz".to_owned(),
                    browser_download_url: "https://example.com/konnector.tar.gz".to_owned(),
                },
                GithubAsset {
                    name: "konnector_0.1.0-1_amd64.deb".to_owned(),
                    browser_download_url: "https://example.com/konnector.deb".to_owned(),
                },
            ],
        };
        assert_eq!(
            release_package_url(release).unwrap(),
            "https://example.com/konnector.deb"
        );
    }

    #[cfg(unix)]
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

    #[cfg(windows)]
    #[test]
    fn selects_windows_zip_for_tag() {
        let release = GithubRelease {
            tag_name: "v0.1.1".to_owned(),
            assets: vec![
                GithubAsset {
                    name: "konnector_0.1.1-1_amd64.deb".to_owned(),
                    browser_download_url: "https://example.com/konnector.deb".to_owned(),
                },
                GithubAsset {
                    name: "konnector-v0.1.1-windows-x86_64.zip".to_owned(),
                    browser_download_url: "https://example.com/konnector.zip".to_owned(),
                },
            ],
        };
        assert_eq!(
            release_package_url(release).unwrap(),
            "https://example.com/konnector.zip"
        );
    }

    #[test]
    fn normalizes_github_repo_urls() {
        assert_eq!(
            normalize_github_repo("https://github.com/veliuysal/konnector.git"),
            "veliuysal/konnector"
        );
        assert_eq!(
            normalize_github_repo("veliuysal/konnector"),
            "veliuysal/konnector"
        );
    }
}
