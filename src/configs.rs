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
    pub root_proxy: Option<UpstreamConfig>,
}

#[derive(Clone, Debug)]
pub struct HttpsConfig {
    pub certificate_path: String,
    pub private_key_path: String,
}

#[derive(Clone, Debug)]
pub struct TlsProviderConfig {
    pub provider: TlsProviderKind,
    pub cloudflare_api_token: Option<String>,
    pub fetch_command: Option<String>,
    pub check_interval_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsProviderKind {
    None,
    Cloudflare,
    Command,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
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
}

impl SiteConfig {
    pub fn primary_domain(&self) -> &str {
        self.domains
            .first()
            .map(String::as_str)
            .unwrap_or("<no-domain>")
    }
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
    ServerConfig {
        http_listen: env::var("HTTP_LISTEN").unwrap_or_else(|_| "0.0.0.0:80".to_owned()),
        https_listen: env::var("HTTPS_LISTEN").unwrap_or_else(|_| "0.0.0.0:443".to_owned()),
        threads: env::var("THREADS")
            .ok()
            .map(|value| value.parse().expect("THREADS must be a number"))
            .unwrap_or_else(default_threads),
        https: https_from_env(),
        root_proxy: root_proxy_from_env().or_else(|| root_proxy_from_file(&config_dir())),
    }
}

pub fn tls_provider() -> TlsProviderConfig {
    tls_provider_from_env()
}

pub fn load_sites() -> Result<Vec<SiteConfig>, String> {
    load_sites_from(&config_dir())
}

pub fn config_dir() -> PathBuf {
    env::var("CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("configs"))
}

pub(crate) fn load_sites_from(directory: &Path) -> Result<Vec<SiteConfig>, String> {
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
    if paths.is_empty() {
        return Err(format!("no YAML configs found in {}", directory.display()));
    }
    paths
        .into_iter()
        .map(|path| {
            let raw = fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            serde_yaml::from_str(&raw)
                .map_err(|error| format!("invalid YAML in {}: {error}", path.display()))
        })
        .collect()
}

fn https_from_env() -> Option<HttpsConfig> {
    env_bool("TLS_ENABLED", false).then(|| HttpsConfig {
        certificate_path: required_env("TLS_CERT_PATH"),
        private_key_path: required_env("TLS_KEY_PATH"),
    })
}

fn tls_provider_from_env() -> TlsProviderConfig {
    let provider = match env::var("TLS_PROVIDER").as_deref() {
        Ok("cloudflare") => TlsProviderKind::Cloudflare,
        Ok("command") => TlsProviderKind::Command,
        Ok("none") | Ok("") => TlsProviderKind::None,
        Err(_) => TlsProviderKind::None,
        Ok(value) => panic!("invalid TLS_PROVIDER value: {value}"),
    };
    TlsProviderConfig {
        provider,
        cloudflare_api_token: env::var("CLOUDFLARE_API_TOKEN").ok(),
        fetch_command: env::var("TLS_FETCH_COMMAND").ok(),
        check_interval_seconds: env::var("TLS_CHECK_INTERVAL_SECONDS")
            .ok()
            .map(|value| value.parse().expect("TLS_CHECK_INTERVAL_SECONDS must be a number"))
            .unwrap_or(21_600),
    }
}

fn is_site_config(path: &Path) -> bool {
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

#[derive(Deserialize)]
struct RootConfig {
    upstream: UpstreamConfig,
}

fn root_proxy_from_env() -> Option<UpstreamConfig> {
    let address = env::var("ROOT_PROXY").ok()?;
    if address.trim().is_empty() {
        return None;
    }
    Some(UpstreamConfig {
        address,
        tls: env_bool("ROOT_PROXY_TLS", false),
        sni: env::var("ROOT_PROXY_SNI").unwrap_or_default(),
        host: env::var("ROOT_PROXY_HOST").ok(),
        ca_path: env::var("ROOT_PROXY_CA_PATH").ok(),
        base_path: String::new(),
    })
}

fn root_proxy_from_file(directory: &Path) -> Option<UpstreamConfig> {
    let path = ["root.yaml", "root.yml"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())?;
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let config: RootConfig = serde_yaml::from_str(&raw)
        .unwrap_or_else(|error| panic!("invalid YAML in {}: {error}", path.display()));
    Some(config.upstream)
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

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name).as_deref() {
        Ok("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON") => true,
        Ok("0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF") => false,
        Err(_) => default,
        Ok(value) => panic!("invalid {name} value: {value}"),
    }
}

fn required_env(name: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} is required when TLS_ENABLED=true"))
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
    fn root_config_loads_upstream() {
        let upstream = root_proxy_from_file(Path::new("configs")).unwrap();
        assert_eq!(upstream.address, "127.0.0.1:3000");
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
}
