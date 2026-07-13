use serde::{de, Deserialize, Deserializer};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub http_listen: String,
    pub https_listen: String,
    pub threads: usize,
    pub https: Option<HttpsConfig>,
    pub tls_provider: TlsProviderConfig,
    pub root_proxy: Option<ProxyTarget>,
    pub logging: LogLevel,
}

#[derive(Clone, Debug)]
pub struct HttpsConfig {
    pub certificate_path: String,
    pub private_key_path: String,
}

#[derive(Clone, Debug, Default)]
pub struct TlsProviderConfig {
    pub provider: TlsProviderKind,
    pub cloudflare_api_token: Option<String>,
    pub fetch_command: Option<String>,
    pub check_interval_seconds: u64,
    pub acme_staging: bool,
    /// Root TLS directory from env `TLS_DIR` (holds fullchain.pem, privkey.pem, acme/).
    pub tls_dir: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TlsProviderKind {
    #[default]
    None,
    Acme,
    Cloudflare,
    Command,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub domains: Vec<String>,
    #[serde(rename = "proxy")]
    pub target: ProxyTarget,
    #[serde(default)]
    pub internal_routes: Vec<InternalRouteConfig>,
    #[serde(default)]
    pub redirects: Vec<RedirectRule>,
    #[serde(default)]
    pub access: AccessPolicy,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub forwarding: ForwardingConfig,
    #[serde(default)]
    pub logging: Option<LoggingConfig>,
    #[serde(default)]
    pub http: HttpSettings,
    /// Config file stem (`example` from `example.yaml`). Not part of YAML.
    #[serde(skip)]
    pub source_file: String,
}

impl SiteConfig {
    pub fn primary_domain(&self) -> &str {
        self.domains
            .first()
            .map(String::as_str)
            .unwrap_or("<no-domain>")
    }

    pub fn resolved_logging(&self, default: LogLevel) -> LogLevel {
        self.logging
            .as_ref()
            .map(|logging| logging.level)
            .unwrap_or(default)
    }

    pub fn resolve_http_versions(&mut self) {
        let default = self.http.version;
        self.target.apply_http_default(default);
        for route in &mut self.internal_routes {
            route.upstream.apply_http_default(default);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HttpVersion {
    #[default]
    Auto,
    #[serde(rename = "1.1", alias = "1")]
    Http11,
    #[serde(rename = "2")]
    Http2,
    #[serde(rename = "3")]
    Http3,
}

impl HttpVersion {
    pub fn log_label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Http11 => "1.1",
            Self::Http2 => "2",
            Self::Http3 => "3",
        }
    }

    pub fn to_alpn(self, tls: bool) -> pingora::protocols::ALPN {
        use pingora::protocols::ALPN;
        match self {
            Self::Http11 => ALPN::H1,
            Self::Http2 if tls => ALPN::H2,
            Self::Http2 => ALPN::H1,
            Self::Http3 => ALPN::Custom(pingora::protocols::tls::CustomALPN::new(b"h3".to_vec())),
            Self::Auto if tls => ALPN::H2H1,
            Self::Auto => ALPN::H1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpSettings {
    pub version: HttpVersion,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    #[default]
    Off,
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn is_debug(self) -> bool {
        matches!(self, Self::Debug)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: LogLevel,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedirectRule {
    pub from: String,
    pub to: String,
    #[serde(default = "default_redirect_status")]
    pub status: u16,
    #[serde(default, rename = "match")]
    pub match_type: RedirectMatch,
    #[serde(default)]
    pub behavior: RedirectBehavior,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectBehavior {
    #[default]
    Redirect,
    Rewrite,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectMatch {
    #[default]
    Exact,
    Prefix,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternalRouteConfig {
    pub prefix: String,
    pub upstream: UpstreamConfig,
    #[serde(default = "default_true")]
    pub strip_prefix: bool,
}

#[derive(Clone, Debug)]
pub struct UpstreamConfig {
    pub address: String,
    pub tls: bool,
    pub sni: String,
    pub host: Option<String>,
    pub ca_path: Option<String>,
    pub base_path: String,
    pub http_version: HttpVersion,
}

impl UpstreamConfig {
    pub(crate) fn apply_http_default(&mut self, default: HttpVersion) {
        if self.http_version == HttpVersion::Auto {
            self.http_version = default;
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUpstreamConfig {
    instance: Option<String>,
    address: Option<String>,
    url: Option<String>,
    tls: Option<bool>,
    sni: Option<String>,
    host: Option<String>,
    ca_path: Option<String>,
    #[serde(default, rename = "version")]
    http_version: Option<HttpVersion>,
}

impl<'de> Deserialize<'de> for UpstreamConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawUpstreamConfig::deserialize(deserializer)?;
        normalize_upstream(raw).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyTarget {
    Direct {
        upstream: UpstreamConfig,
    },
    LoadBalanced {
        upstreams: Vec<UpstreamConfig>,
        #[serde(default = "default_true")]
        health_check: bool,
        #[serde(default = "default_health_interval")]
        health_check_interval_seconds: u64,
    },
}

impl ProxyTarget {
    pub fn apply_http_default(&mut self, default: HttpVersion) {
        match self {
            Self::Direct { upstream } => upstream.apply_http_default(default),
            Self::LoadBalanced { upstreams, .. } => {
                for upstream in upstreams {
                    upstream.apply_http_default(default);
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccessPolicy {
    #[default]
    All,
    OnlyPrefixes {
        prefixes: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub enabled: bool,
    pub max_file_bytes: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_file_bytes: 10 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardingConfig {
    #[default]
    Direct,
    Cloudflare,
    TrustedProxy,
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .max(4)
}

pub fn server() -> ServerConfig {
    let directory = config_dir();
    let (path, root) = load_root_settings(&directory);
    let (https, tls_provider) = resolve_tls_config(&root.tls);
    ServerConfig {
        http_listen: env_string("HTTP_LISTEN", "0.0.0.0:80"),
        https_listen: env_string("HTTPS_LISTEN", "0.0.0.0:443"),
        threads: env_u64("THREADS", default_threads() as u64).max(1) as usize,
        https,
        tls_provider,
        root_proxy: root_proxy_from_env()
            .or_else(|| root_proxy_from_settings(path.as_deref(), &root)),
        logging: root.logging,
    }
}

#[cfg(test)]
pub fn load_sites() -> Result<Vec<SiteConfig>, String> {
    load_sites_from(&config_dir())
}

pub fn load_sites_lenient() -> Vec<SiteConfig> {
    load_sites_from_lenient(&config_dir())
}

pub(crate) fn warn_root_file(directory: &Path) {
    if let Err(error) = validate_root_file(directory) {
        log::warn!("{error}");
    }
}

pub(crate) fn validate_root_file(directory: &Path) -> Result<(), String> {
    let (path, settings) = load_root_settings(directory);
    if settings.enabled && settings.target.is_none() {
        let label = path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "root.yaml".to_owned());
        return Err(format!("{label} has enabled: true but no upstream"));
    }
    Ok(())
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = env::var("CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let production = crate::paths::production_config_dir();
    if production.is_dir() {
        return production;
    }
    PathBuf::from("configs")
}

pub(crate) fn load_sites_from_lenient(directory: &Path) -> Vec<SiteConfig> {
    let paths = match list_site_config_paths(directory) {
        Ok(paths) => paths,
        Err(error) => {
            log::error!("{error}");
            return Vec::new();
        }
    };
    if paths.is_empty() {
        log::warn!("no YAML site configs found in {}", directory.display());
        return Vec::new();
    }
    let mut sites = Vec::new();
    for path in paths {
        match load_site_file(&path) {
            Ok(mut site) => {
                site.resolve_http_versions();
                sites.push(site);
            }
            Err(error) => log::error!("{error}"),
        }
    }
    sites
}

#[cfg(test)]
pub(crate) fn load_sites_from(directory: &Path) -> Result<Vec<SiteConfig>, String> {
    let paths = list_site_config_paths(directory)?;
    if paths.is_empty() {
        return Err(format!("no YAML configs found in {}", directory.display()));
    }
    paths.into_iter().map(|path| load_site_file(&path)).collect()
}

fn list_site_config_paths(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "cannot read config directory {}: {error}",
                directory.display()
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_site_config(path))
        .collect::<Vec<PathBuf>>();
    paths.sort();
    Ok(paths)
}

fn load_site_file(path: &Path) -> Result<SiteConfig, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut site: SiteConfig = serde_yaml::from_str(&raw)
        .map_err(|error| format!("invalid YAML in {}: {error}", path.display()))?;
    site.source_file = config_stem(path);
    Ok(site)
}

fn config_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_owned()
}

/// TLS flags from `root.yaml`. File paths are never set here — they come from env `TLS_DIR`
/// (default: platform SSL dir), which holds `fullchain.pem`, `privkey.pem`, and `acme/`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootTlsConfig {
    #[serde(default)]
    enabled: bool,
    /// When true, obtain/renew Let's Encrypt certs into `TLS_DIR`.
    /// When false, the server only reads existing files from `TLS_DIR`.
    #[serde(default)]
    auto: bool,
    #[serde(default)]
    staging: bool,
}

#[derive(Clone, Debug, Default)]
struct RootTlsSettings {
    enabled: bool,
    auto: bool,
    staging: bool,
}

impl From<RootTlsConfig> for RootTlsSettings {
    fn from(config: RootTlsConfig) -> Self {
        Self {
            enabled: config.enabled,
            auto: config.auto,
            staging: config.staging,
        }
    }
}

const TLS_CERT_FILE: &str = "fullchain.pem";
const TLS_KEY_FILE: &str = "privkey.pem";

fn resolve_tls_config(yaml: &RootTlsSettings) -> (Option<HttpsConfig>, TlsProviderConfig) {
    let env_enabled = env_bool("TLS_ENABLED", false);
    let enabled = env_enabled || yaml.enabled;
    if !enabled {
        return (None, TlsProviderConfig::default());
    }

    let tls_dir = env_string("TLS_DIR", &crate::paths::path_display(&crate::paths::ssl_dir()));
    let tls_root = PathBuf::from(&tls_dir);
    let certificate_path = tls_root.join(TLS_CERT_FILE).to_string_lossy().into_owned();
    let private_key_path = tls_root.join(TLS_KEY_FILE).to_string_lossy().into_owned();

    let provider = match env_string("TLS_PROVIDER", "").to_ascii_lowercase().as_str() {
        "acme" | "letsencrypt" => TlsProviderKind::Acme,
        "cloudflare" => TlsProviderKind::Cloudflare,
        "command" => TlsProviderKind::Command,
        "none" | "" if yaml.auto => TlsProviderKind::Acme,
        "none" | "" => TlsProviderKind::None,
        other => {
            log::warn!("invalid TLS_PROVIDER value '{other}'; using default");
            if yaml.auto {
                TlsProviderKind::Acme
            } else {
                TlsProviderKind::None
            }
        }
    };

    (
        Some(HttpsConfig {
            certificate_path,
            private_key_path,
        }),
        TlsProviderConfig {
            provider,
            cloudflare_api_token: env_optional("CLOUDFLARE_API_TOKEN"),
            fetch_command: env_optional("TLS_FETCH_COMMAND"),
            check_interval_seconds: env_u64("TLS_CHECK_INTERVAL_SECONDS", 21_600),
            acme_staging: env_bool("ACME_STAGING", yaml.staging),
            tls_dir: Some(tls_dir),
        },
    )
}

fn is_site_config(path: &Path) -> bool {
    if is_tcp_config(path) {
        return false;
    }
    if !matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("yml" | "yaml")
    ) {
        return false;
    }
    !matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some("root.yaml" | "root.yml")
    )
}

pub(crate) fn is_tcp_config(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.ends_with(".tcp.yaml") || name.ends_with(".tcp.yml"))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcpProxyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub name: String,
    pub listen: String,
    pub upstream: TcpUpstreamConfig,
    /// Config file stem (`postgres.tcp` from `postgres.tcp.yaml`). Not part of YAML.
    #[serde(skip)]
    pub source_file: String,
}

impl TcpProxyConfig {
    pub fn listen_address(&self) -> Result<String, String> {
        normalize_listen_address(&self.listen)
    }

    pub fn listen_port(&self) -> Result<u16, String> {
        let address = self.listen_address()?;
        address
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .ok_or_else(|| format!("invalid tcp listen port in {}", self.listen))
    }

    pub fn listen_addresses(&self) -> Result<Vec<String>, String> {
        let primary = self.listen_address()?;
        let mut addresses = vec![primary.clone()];
        if primary.starts_with("0.0.0.0:") {
            if let Some(port) = primary.rsplit_once(':').map(|(_, port)| port) {
                addresses.push(format!("[::]:{port}"));
            }
        }
        Ok(addresses)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcpUpstreamConfig {
    instance: Option<String>,
    address: Option<String>,
}

impl TcpUpstreamConfig {
    pub fn address(&self) -> Result<String, String> {
        let value = self
            .instance
            .as_deref()
            .or(self.address.as_deref())
            .ok_or_else(|| "tcp upstream requires instance or address".to_string())?
            .trim();
        if value.is_empty() {
            return Err("tcp upstream cannot be empty".into());
        }
        normalize_tcp_address(value)
    }
}

pub fn load_tcp_lenient() -> Vec<TcpProxyConfig> {
    load_tcp_from_lenient(&config_dir())
}

pub(crate) fn load_tcp_from_lenient(directory: &Path) -> Vec<TcpProxyConfig> {
    let paths = match list_tcp_config_paths(directory) {
        Ok(paths) => paths,
        Err(error) => {
            log::error!("{error}");
            return Vec::new();
        }
    };
    let mut proxies = Vec::new();
    for path in paths {
        match load_tcp_file(&path) {
            Ok(mut proxy) => {
                proxy.source_file = config_stem(&path);
                if proxy.name.is_empty() {
                    proxy.name = proxy
                        .source_file
                        .trim_end_matches(".tcp")
                        .to_owned();
                }
                proxies.push(proxy);
            }
            Err(error) => log::error!("{error}"),
        }
    }
    proxies
}

fn list_tcp_config_paths(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "cannot read config directory {}: {error}",
                directory.display()
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_tcp_config(path))
        .collect::<Vec<PathBuf>>();
    paths.sort();
    Ok(paths)
}

fn load_tcp_file(path: &Path) -> Result<TcpProxyConfig, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut proxy: TcpProxyConfig = serde_yaml::from_str(&raw)
        .map_err(|error| format!("invalid YAML in {}: {error}", path.display()))?;
    proxy.source_file = config_stem(path);
    Ok(proxy)
}

fn normalize_listen_address(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("tcp listen cannot be empty".into());
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(format!("0.0.0.0:{value}"));
    }
    if let Some(port) = value.strip_prefix(':') {
        if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(format!("0.0.0.0:{port}"));
        }
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        if host.parse::<std::net::IpAddr>().is_ok()
            || host == "0.0.0.0"
            || host == "localhost"
            || host.starts_with('[')
        {
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) {
                return Ok(value.to_owned());
            }
        } else {
            return Err(format!(
                "tcp listen must be an IP address or port, not a domain ({value})"
            ));
        }
    }
    Err(format!("invalid tcp listen address: {value}"))
}

fn normalize_tcp_address(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("tcp upstream cannot be empty".into());
    }
    const DEFAULT_PORT: u16 = 5432;
    if value.starts_with('[') {
        return Ok(if value.contains("]:") {
            value.to_owned()
        } else if value.ends_with(']') {
            format!("{value}:{DEFAULT_PORT}")
        } else {
            return Err("invalid bracketed IPv6 tcp upstream".into());
        });
    }
    let colon_count = value.bytes().filter(|byte| *byte == b':').count();
    if colon_count > 1 {
        return Ok(format!("[{value}]:{DEFAULT_PORT}"));
    }
    if colon_count == 1
        && value.rsplit_once(':').is_some_and(|(_, port)| {
            !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Ok(value.to_owned());
    }
    Ok(format!("{value}:{DEFAULT_PORT}"))
}

#[derive(Deserialize)]
struct RootConfig {
    #[serde(default)]
    enabled: bool,
    /// Shorthand for `proxy: { mode: direct, upstream: ... }`.
    #[serde(default)]
    upstream: Option<UpstreamConfig>,
    /// Full proxy target (`direct` or `load_balanced`), same shape as site configs.
    #[serde(default)]
    proxy: Option<ProxyTarget>,
    #[serde(default)]
    logging: LoggingConfig,
    #[serde(default)]
    http: HttpSettings,
    #[serde(default)]
    tls: RootTlsConfig,
}

#[derive(Clone, Debug, Default)]
struct RootSettings {
    enabled: bool,
    target: Option<ProxyTarget>,
    logging: LogLevel,
    http_version: HttpVersion,
    tls: RootTlsSettings,
}

impl TryFrom<RootConfig> for RootSettings {
    type Error = String;

    fn try_from(config: RootConfig) -> Result<Self, Self::Error> {
        let target = match (config.upstream, config.proxy) {
            (Some(_), Some(_)) => {
                return Err("root.yaml cannot set both upstream and proxy".into());
            }
            (Some(upstream), None) => Some(ProxyTarget::Direct { upstream }),
            (None, Some(proxy)) => Some(proxy),
            (None, None) => None,
        };
        Ok(Self {
            enabled: config.enabled,
            target,
            logging: config.logging.level,
            http_version: config.http.version,
            tls: config.tls.into(),
        })
    }
}

fn root_config_path(directory: &Path) -> Option<PathBuf> {
    ["root.yaml", "root.yml"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
}

fn load_root_settings(directory: &Path) -> (Option<PathBuf>, RootSettings) {
    let Some(path) = root_config_path(directory) else {
        return (None, RootSettings::default());
    };
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            log::error!("cannot read {}: {error}", path.display());
            return (Some(path), RootSettings::default());
        }
    };
    if raw.trim().is_empty() {
        return (Some(path), RootSettings::default());
    }
    match serde_yaml::from_str::<RootConfig>(&raw) {
        Ok(config) => match RootSettings::try_from(config) {
            Ok(settings) => (Some(path), settings),
            Err(error) => {
                log::error!("invalid root config in {}: {error}", path.display());
                (Some(path), RootSettings::default())
            }
        },
        Err(error) => {
            log::error!("invalid YAML in {}: {error}", path.display());
            (Some(path), RootSettings::default())
        }
    }
}

fn root_proxy_from_settings(path: Option<&Path>, settings: &RootSettings) -> Option<ProxyTarget> {
    if !settings.enabled {
        return None;
    }
    match settings.target.clone() {
        Some(mut target) => {
            target.apply_http_default(settings.http_version);
            Some(target)
        }
        None => {
            if let Some(path) = path {
                log::warn!(
                    "{} has enabled: true but no upstream; using working page",
                    path.display()
                );
            }
            None
        }
    }
}

fn root_proxy_from_env() -> Option<ProxyTarget> {
    let address = env::var("ROOT_PROXY").ok()?;
    if address.trim().is_empty() {
        return None;
    }
    Some(ProxyTarget::Direct {
        upstream: UpstreamConfig {
            address,
            tls: env_bool("ROOT_PROXY_TLS", false),
            sni: env::var("ROOT_PROXY_SNI").unwrap_or_default(),
            host: env::var("ROOT_PROXY_HOST").ok(),
            ca_path: env::var("ROOT_PROXY_CA_PATH").ok(),
            base_path: String::new(),
            http_version: HttpVersion::Auto,
        },
    })
}

fn normalize_upstream(raw: RawUpstreamConfig) -> Result<UpstreamConfig, String> {
    let (address, url) = match (raw.instance, raw.address, raw.url) {
        (Some(instance), None, None) if instance.contains("://") => (None, Some(instance)),
        (Some(instance), None, None) => {
            let tls = raw.tls.unwrap_or(false);
            (Some(normalize_instance_address(&instance, tls)?), None)
        }
        (None, address, url) => (address, url),
        _ => return Err("set only one of upstream instance, address, or url".into()),
    };
    match (address, url) {
        (Some(address), None) => Ok(UpstreamConfig {
            address,
            tls: raw.tls.unwrap_or(false),
            sni: raw.sni.unwrap_or_default(),
            host: raw.host,
            ca_path: raw.ca_path,
            base_path: String::new(),
            http_version: raw.http_version.unwrap_or_default(),
        }),
        (None, Some(value)) => {
            if raw.tls.is_some() || raw.sni.is_some() {
                return Err("url derives tls and sni; do not set them separately".into());
            }
            let parsed = url::Url::parse(&value)
                .map_err(|error| format!("invalid upstream url: {error}"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err("upstream url scheme must be http or https".into());
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err("upstream url must not contain credentials".into());
            }
            if parsed.query().is_some() || parsed.fragment().is_some() {
                return Err("upstream url must not contain a query or fragment".into());
            }
            let hostname = parsed.host_str().ok_or("upstream url requires a host")?;
            let port = parsed
                .port_or_known_default()
                .ok_or("upstream url requires a port")?;
            let address_host = if hostname.contains(':') {
                format!("[{hostname}]")
            } else {
                hostname.to_owned()
            };
            let explicit_port = parsed.port();
            let host_header = raw.host.or_else(|| {
                Some(match explicit_port {
                    Some(port) => format!("{address_host}:{port}"),
                    None => address_host.clone(),
                })
            });
            let path = parsed.path().trim_end_matches('/');
            Ok(UpstreamConfig {
                address: format!("{address_host}:{port}"),
                tls: parsed.scheme() == "https",
                sni: hostname.to_owned(),
                host: host_header,
                ca_path: raw.ca_path,
                base_path: path.to_owned(),
                http_version: raw.http_version.unwrap_or_default(),
            })
        }
        (Some(_), Some(_)) => Err("set either upstream address or url, not both".into()),
        (None, None) => Err("upstream requires address or url".into()),
    }
}

fn normalize_instance_address(value: &str, tls: bool) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("upstream instance cannot be empty".into());
    }
    let default_port = if tls { 443 } else { 80 };
    if value.starts_with('[') {
        return Ok(if value.contains("]:") {
            value.to_owned()
        } else if value.ends_with(']') {
            format!("{value}:{default_port}")
        } else {
            return Err("invalid bracketed IPv6 upstream instance".into());
        });
    }
    let colon_count = value.bytes().filter(|byte| *byte == b':').count();
    if colon_count > 1 {
        return Ok(format!("[{value}]:{default_port}"));
    }
    if colon_count == 1
        && value.rsplit_once(':').is_some_and(|(_, port)| {
            !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Ok(value.to_owned());
    }
    Ok(format!("{value}:{default_port}"))
}

fn env_string(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                default.to_owned()
            } else {
                trimmed.to_owned()
            }
        }
        Err(_) => default.to_owned(),
    }
}

fn env_optional(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Err(_) => None,
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    match env::var(name) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(parsed) => parsed,
            Err(_) => {
                log::warn!("invalid {name} value '{value}'; using default {default}");
                default
            }
        },
        Err(_) => default,
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name).as_deref().map(str::trim) {
        Ok("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON") => true,
        Ok("0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF") => false,
        Ok("") | Err(_) => default,
        Ok(value) => {
            log::warn!("invalid {name} value '{value}'; using default {default}");
            default
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_health_interval() -> u64 {
    5
}
fn default_redirect_status() -> u16 {
    308
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_enabled_defaults_to_true() {
        let site: SiteConfig = serde_yaml::from_str(
            "domains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\n",
        )
        .unwrap();
        assert!(site.enabled);
    }

    #[test]
    fn example_configs_load() {
        let sites = load_sites_from(Path::new("configs")).unwrap();
        assert_eq!(sites.len(), 1);
        assert!(sites
            .iter()
            .any(|site| site.domains.iter().any(|domain| domain == "example.com")));
    }

    #[test]
    fn root_config_is_excluded_from_sites() {
        let directory = Path::new("configs");
        assert!(directory.join("root.yaml").is_file());
        let sites = load_sites_from(directory).unwrap();
        assert_eq!(sites.len(), 1);
    }

    #[test]
    fn root_config_is_disabled_by_default() {
        let temp = std::env::temp_dir().join(format!("konnector-root-default-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("root.yaml"),
            "enabled: false\nlogging:\n  level: off\n",
        )
        .unwrap();

        let (path, settings) = load_root_settings(&temp);
        assert!(path.is_some());
        assert!(!settings.enabled);
        assert!(root_proxy_from_settings(path.as_deref(), &settings).is_none());

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn missing_or_empty_root_defaults_to_disabled() {
        let temp = std::env::temp_dir().join(format!("konnector-root-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let (path, settings) = load_root_settings(&temp);
        assert!(path.is_none());
        assert!(!settings.enabled);
        assert!(settings.target.is_none());

        fs::write(temp.join("root.yaml"), "").unwrap();
        let (path, settings) = load_root_settings(&temp);
        assert!(path.is_some());
        assert!(!settings.enabled);

        fs::write(temp.join("root.yaml"), "   \n").unwrap();
        let (_, settings) = load_root_settings(&temp);
        assert!(!settings.enabled);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn enabled_root_without_upstream_is_disabled() {
        let upstream = serde_yaml::from_str::<RootConfig>("enabled: true\n").unwrap();
        assert!(upstream.enabled);
        assert!(upstream.upstream.is_none());
        assert!(upstream.proxy.is_none());
    }

    #[test]
    fn root_tls_defaults_tls_dir_when_env_unset() {
        let root: RootConfig = serde_yaml::from_str(
            r#"
tls:
  enabled: true
  auto: false
"#,
        )
        .unwrap();
        let settings = RootSettings::try_from(root).unwrap();
        env::remove_var("TLS_DIR");
        let (https, provider) = resolve_tls_config(&settings.tls);
        let default_dir = crate::paths::path_display(&crate::paths::ssl_dir());
        let https = https.expect("https enabled");
        assert!(https.certificate_path.starts_with(&default_dir));
        assert!(https.private_key_path.starts_with(&default_dir));
        assert_eq!(provider.tls_dir.as_deref(), Some(default_dir.as_str()));
    }

    #[test]
    fn root_tls_auto_uses_tls_dir_from_env() {
        let root: RootConfig = serde_yaml::from_str(
            r#"
tls:
  enabled: true
  auto: true
  staging: true
"#,
        )
        .unwrap();
        let settings = RootSettings::try_from(root).unwrap();
        assert!(settings.tls.enabled);
        assert!(settings.tls.auto);

        env::set_var("TLS_DIR", "/data/certs");
        let (https, provider) = resolve_tls_config(&settings.tls);
        env::remove_var("TLS_DIR");

        let https = https.expect("https enabled");
        assert_eq!(https.certificate_path, "/data/certs/fullchain.pem");
        assert_eq!(https.private_key_path, "/data/certs/privkey.pem");
        assert_eq!(provider.provider, TlsProviderKind::Acme);
        assert_eq!(provider.tls_dir.as_deref(), Some("/data/certs"));
        assert!(provider.acme_staging);
    }

    #[test]
    fn root_tls_manual_mode_does_not_enable_acme() {
        let root: RootConfig = serde_yaml::from_str(
            r#"
tls:
  enabled: true
  auto: false
"#,
        )
        .unwrap();
        let settings = RootSettings::try_from(root).unwrap();
        env::set_var("TLS_DIR", "/data/certs");
        let (https, provider) = resolve_tls_config(&settings.tls);
        env::remove_var("TLS_DIR");
        assert!(https.is_some());
        assert_eq!(provider.provider, TlsProviderKind::None);
        assert_eq!(provider.tls_dir.as_deref(), Some("/data/certs"));
    }

    #[test]
    fn root_load_balanced_proxy_parses() {
        let root: RootConfig = serde_yaml::from_str(
            r#"
enabled: true
proxy:
  mode: load_balanced
  upstreams:
    - instance: 127.0.0.1:3000
    - instance: 127.0.0.1:3001
  health_check: true
  health_check_interval_seconds: 5
"#,
        )
        .unwrap();
        let settings = RootSettings::try_from(root).unwrap();
        let target = root_proxy_from_settings(None, &settings).unwrap();
        match target {
            ProxyTarget::LoadBalanced { upstreams, health_check, health_check_interval_seconds } => {
                assert_eq!(upstreams.len(), 2);
                assert_eq!(upstreams[0].address, "127.0.0.1:3000");
                assert_eq!(upstreams[1].address, "127.0.0.1:3001");
                assert!(health_check);
                assert_eq!(health_check_interval_seconds, 5);
            }
            ProxyTarget::Direct { .. } => panic!("expected load_balanced root proxy"),
        }
    }

    #[test]
    fn root_rejects_both_upstream_and_proxy() {
        let root: RootConfig = serde_yaml::from_str(
            r#"
enabled: true
upstream:
  instance: 127.0.0.1:3000
proxy:
  mode: direct
  upstream:
    instance: 127.0.0.1:3001
"#,
        )
        .unwrap();
        let error = RootSettings::try_from(root).unwrap_err();
        assert!(error.contains("both upstream and proxy"));
    }

    #[test]
    fn logging_levels_parse_from_yaml() {
        let site: SiteConfig = serde_yaml::from_str(
            "domains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\nlogging:\n  level: debug\n",
        )
        .unwrap();
        assert_eq!(
            site.resolved_logging(LogLevel::Off),
            LogLevel::Debug
        );

        let root: RootConfig = serde_yaml::from_str("logging:\n  level: info\n").unwrap();
        assert_eq!(root.logging.level, LogLevel::Info);
    }

    #[test]
    fn url_upstream_is_normalized() {
        let upstream: UpstreamConfig = serde_yaml::from_str(
            "url: https://service.example.com:8443/base/\nca_path: /tmp/ca.pem\n",
        )
        .unwrap();
        assert_eq!(upstream.address, "service.example.com:8443");
        assert!(upstream.tls);
        assert_eq!(upstream.sni, "service.example.com");
        assert_eq!(upstream.host.as_deref(), Some("service.example.com:8443"));
        assert_eq!(upstream.base_path, "/base");
    }

    #[test]
    fn instance_accepts_localhost_ip_and_ipv6() {
        let localhost: UpstreamConfig = serde_yaml::from_str("instance: localhost\n").unwrap();
        let ip: UpstreamConfig = serde_yaml::from_str("instance: 10.0.0.5:9000\n").unwrap();
        let ipv6: UpstreamConfig = serde_yaml::from_str("instance: ::1\n").unwrap();
        assert_eq!(localhost.address, "localhost:80");
        assert_eq!(ip.address, "10.0.0.5:9000");
        assert_eq!(ipv6.address, "[::1]:80");
    }

    #[test]
    fn tcp_configs_are_excluded_from_sites() {
        let directory = Path::new("configs");
        assert!(directory.join("postgres.tcp.yaml").is_file());
        let sites = load_sites_from(directory).unwrap();
        assert_eq!(sites.len(), 1);
    }

    #[test]
    fn tcp_config_parses_and_normalizes_upstream() {
        let proxy: TcpProxyConfig = serde_yaml::from_str(
            "name: postgres\nlisten: 5432\nupstream:\n  instance: localhost\n",
        )
        .unwrap();
        assert_eq!(proxy.name, "postgres");
        assert_eq!(proxy.listen_address().unwrap(), "0.0.0.0:5432");
        assert_eq!(
            proxy.listen_addresses().unwrap(),
            vec!["0.0.0.0:5432".to_owned(), "[::]:5432".to_owned()]
        );
        assert_eq!(proxy.upstream.address().unwrap(), "localhost:5432");
    }

    #[test]
    fn tcp_upstream_accepts_domain_name() {
        let proxy: TcpProxyConfig = serde_yaml::from_str(
            "listen: 5432\nupstream:\n  instance: postgres.internal\n",
        )
        .unwrap();
        assert_eq!(proxy.upstream.address().unwrap(), "postgres.internal:5432");
    }

    #[test]
    fn http_version_parses_from_yaml() {
        let site: SiteConfig = serde_yaml::from_str(
            "domains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\n    version: \"1.1\"\nhttp:\n  version: \"2\"\n",
        )
        .unwrap();
        assert_eq!(site.http.version, HttpVersion::Http2);
        if let ProxyTarget::Direct { upstream } = &site.target {
            assert_eq!(upstream.http_version, HttpVersion::Http11);
        } else {
            panic!("expected direct proxy target");
        }
    }

    #[test]
    fn site_http_version_defaults_upstream_auto() {
        let mut site: SiteConfig = serde_yaml::from_str(
            "domains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\nhttp:\n  version: \"1.1\"\n",
        )
        .unwrap();
        site.resolve_http_versions();
        if let ProxyTarget::Direct { upstream } = site.target {
            assert_eq!(upstream.http_version, HttpVersion::Http11);
        } else {
            panic!("expected direct proxy target");
        }
    }

    #[test]
    fn http3_version_parses_from_yaml() {
        let site: SiteConfig = serde_yaml::from_str(
            "domains: [example.com]\nproxy:\n  mode: direct\n  upstream:\n    instance: localhost\n    version: \"3\"\n",
        )
        .unwrap();
        if let ProxyTarget::Direct { upstream } = &site.target {
            assert_eq!(upstream.http_version, HttpVersion::Http3);
        } else {
            panic!("expected direct proxy target");
        }
    }

    #[test]
    fn root_http_version_applies_to_upstream() {
        let root: RootConfig = serde_yaml::from_str(
            "enabled: true\nhttp:\n  version: \"1.1\"\nupstream:\n  instance: localhost\n",
        )
        .unwrap();
        let settings = RootSettings::try_from(root).unwrap();
        let target = root_proxy_from_settings(None, &settings).unwrap();
        match target {
            ProxyTarget::Direct { upstream } => {
                assert_eq!(upstream.http_version, HttpVersion::Http11);
            }
            ProxyTarget::LoadBalanced { .. } => panic!("expected direct root proxy"),
        }
    }
}
