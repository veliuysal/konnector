use crate::configs::{ForwardingConfig, HttpsConfig, SiteConfig, TlsProviderConfig, TlsProviderKind};
use openssl::x509::X509;
use std::{
    cmp::Ordering,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const CLOUDFLARE_ORIGIN_CERT_URL: &str = "https://api.cloudflare.com/client/v4/certificates";
const DEFAULT_ORIGIN_VALIDITY_DAYS: u32 = 5475;

pub fn proxied_tls_domains(sites: &[SiteConfig]) -> Vec<String> {
    let mut domains = HashSet::new();
    for site in sites {
        for domain in &site.domains {
            let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
            if is_tls_dns_name(&normalized) {
                domains.insert(normalized);
            }
        }
    }
    let mut list: Vec<_> = domains.into_iter().collect();
    list.sort();
    list
}

pub fn cloudflare_hostnames(domains: &[String]) -> Vec<String> {
    let mut hostnames = HashSet::new();
    for domain in domains {
        hostnames.insert(domain.clone());
        let labels: Vec<_> = domain.split('.').collect();
        if labels.len() >= 2 {
            let apex = labels[labels.len() - 2..].join(".");
            hostnames.insert(format!("*.{apex}"));
        }
    }
    let mut list: Vec<_> = hostnames.into_iter().collect();
    list.sort();
    list
}

pub fn ensure_valid_certificate(
    https: &HttpsConfig,
    sites: &[SiteConfig],
    provider: &TlsProviderConfig,
) -> Result<(), String> {
    let domains = proxied_tls_domains(sites);
    if domains.is_empty() {
        return validate_certificate_files(https, &[]);
    }
    match validate_certificate_files(https, &domains) {
        Ok(()) => Ok(()),
        Err(error) => {
            log::warn!("TLS certificate mismatch: {error}");
            refresh_certificate(https, sites, provider)?;
            validate_certificate_files(https, &domains)
        }
    }
}

pub fn validate_certificate_files(https: &HttpsConfig, domains: &[String]) -> Result<(), String> {
    let identities = certificate_identities(&https.certificate_path)?;
    validate_key_pair(&https.certificate_path, &https.private_key_path)?;
    if identities.is_empty() {
        return Err("certificate contains no host identities".into());
    }
    if let Some(expiry_error) = certificate_expiry_error(&https.certificate_path) {
        return Err(expiry_error);
    }
    for domain in domains {
        if !domain_covered_by_cert(domain, &identities) {
            return Err(format!(
                "certificate does not cover proxied domain {domain}; identities: {}",
                identities.join(", ")
            ));
        }
    }
    Ok(())
}

pub fn refresh_certificate(
    https: &HttpsConfig,
    sites: &[SiteConfig],
    provider: &TlsProviderConfig,
) -> Result<(), String> {
    let domains = proxied_tls_domains(sites);
    if domains.is_empty() {
        return Err("no proxied DNS domains require TLS coverage".into());
    }
    let kind = provider.resolve(sites);
    match kind {
        TlsProviderKind::Cloudflare => {
            let token = provider
                .cloudflare_api_token
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or("CLOUDFLARE_API_TOKEN is required for TLS_PROVIDER=cloudflare")?;
            let hostnames = cloudflare_hostnames(&domains);
            log::info!(
                "requesting Cloudflare origin certificate for {}",
                hostnames.join(", ")
            );
            let (certificate, private_key) = fetch_cloudflare_origin_certificate(token, &hostnames)?;
            write_atomic(&https.certificate_path, &certificate)?;
            write_atomic(&https.private_key_path, &private_key)?;
            Ok(())
        }
        TlsProviderKind::Command => {
            let command = provider
                .fetch_command
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or("TLS_FETCH_COMMAND is required for TLS_PROVIDER=command")?;
            log::info!("refreshing TLS certificate with configured fetch command");
            run_fetch_command(command, https)?;
            Ok(())
        }
        TlsProviderKind::None => Err(
            "TLS certificate mismatch and no TLS provider is configured; set TLS_PROVIDER".into(),
        ),
    }
}

pub fn watch_paths(https: &HttpsConfig) -> Vec<PathBuf> {
    let mut paths = HashSet::new();
    if let Some(parent) = Path::new(&https.certificate_path).parent() {
        paths.insert(parent.to_path_buf());
    }
    if let Some(parent) = Path::new(&https.private_key_path).parent() {
        paths.insert(parent.to_path_buf());
    }
    paths.into_iter().collect()
}

fn fetch_cloudflare_origin_certificate(
    api_token: &str,
    hostnames: &[String],
) -> Result<(String, String), String> {
    let body = serde_json::json!({
        "hostnames": hostnames,
        "requested_validity": DEFAULT_ORIGIN_VALIDITY_DAYS,
        "request_type": "origin-rsa",
    });
    let response = ureq::post(CLOUDFLARE_ORIGIN_CERT_URL)
        .set("Authorization", &format!("Bearer {api_token}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|error| format!("Cloudflare certificate request failed: {error}"))?;
    let status = response.status();
    let payload: serde_json::Value = response
        .into_json()
        .map_err(|error| format!("Cloudflare certificate response is not JSON: {error}"))?;
    if !payload
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let errors = payload
            .get("errors")
            .map(ToString::to_string)
            .unwrap_or_else(|| "unknown Cloudflare API error".to_owned());
        return Err(format!(
            "Cloudflare certificate request returned HTTP {status}: {errors}"
        ));
    }
    let result = payload
        .get("result")
        .ok_or("Cloudflare certificate response is missing result")?;
    let certificate = result
        .get("certificate")
        .and_then(serde_json::Value::as_str)
        .ok_or("Cloudflare certificate response is missing certificate")?;
    let private_key = result
        .get("private_key")
        .and_then(serde_json::Value::as_str)
        .ok_or("Cloudflare certificate response is missing private_key")?;
    Ok((certificate.to_owned(), private_key.to_owned()))
}

fn run_fetch_command(command: &str, https: &HttpsConfig) -> Result<(), String> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("TLS_CERT_PATH", &https.certificate_path)
        .env("TLS_KEY_PATH", &https.private_key_path)
        .status()
        .map_err(|error| format!("cannot run TLS_FETCH_COMMAND: {error}"))?;
    if !status.success() {
        return Err(format!(
            "TLS_FETCH_COMMAND exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

fn write_atomic(path: &str, content: &str) -> Result<(), String> {
    let path = Path::new(path);
    let parent = path
        .parent()
        .ok_or_else(|| format!("cannot resolve parent directory for {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("cannot resolve file name for {}", path.display()))?
        .to_string_lossy();
    let tmp_path = parent.join(format!(
        ".{file_name}.{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    fs::write(&tmp_path, content)
        .map_err(|error| format!("cannot write {}: {error}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .map_err(|error| format!("cannot install {}: {error}", path.display()))?;
    Ok(())
}

fn certificate_identities(cert_path: &str) -> Result<Vec<String>, String> {
    let pem = fs::read(cert_path)
        .map_err(|error| format!("cannot read certificate {}: {error}", cert_path))?;
    let cert = X509::from_pem(&pem)
        .map_err(|error| format!("invalid certificate {}: {error}", cert_path))?;
    let mut identities = HashSet::new();
    if let Some(common_name) = cert
        .subject_name()
        .entries_by_nid(openssl::nid::Nid::COMMONNAME)
        .next()
        .and_then(|entry| entry.data().to_string().ok())
    {
        identities.insert(common_name.to_ascii_lowercase());
    }
    if let Some(names) = cert.subject_alt_names() {
        for index in 0..names.len() {
            if let Some(dns) = names.get(index).and_then(|name| name.dnsname()) {
                identities.insert(dns.to_ascii_lowercase());
            }
        }
    }
    let mut list: Vec<_> = identities.into_iter().collect();
    list.sort();
    Ok(list)
}

fn validate_key_pair(cert_path: &str, key_path: &str) -> Result<(), String> {
    let cert_pem = fs::read(cert_path)
        .map_err(|error| format!("cannot read certificate {}: {error}", cert_path))?;
    let key_pem = fs::read(key_path)
        .map_err(|error| format!("cannot read private key {}: {error}", key_path))?;
    let cert = X509::from_pem(&cert_pem)
        .map_err(|error| format!("invalid certificate {}: {error}", cert_path))?;
    let key = openssl::pkey::PKey::private_key_from_pem(&key_pem)
        .map_err(|error| format!("invalid private key {}: {error}", key_path))?;
    let public_key = cert
        .public_key()
        .map_err(|error| format!("cannot read certificate public key: {error}"))?;
    if !public_key.public_eq(&key) {
        return Err("certificate and private key do not match".into());
    }
    Ok(())
}

fn certificate_expiry_error(cert_path: &str) -> Option<String> {
    let pem = fs::read(cert_path).ok()?;
    let cert = X509::from_pem(&pem).ok()?;
    let not_after = cert.not_after();
    let now = openssl::asn1::Asn1Time::days_from_now(0).ok()?;
    if matches!(not_after.compare(&now).ok()?, Ordering::Less) {
        return Some(format!("certificate expired on {not_after}"));
    }
    None
}

pub fn domain_covered_by_cert(domain: &str, identities: &[String]) -> bool {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    identities.iter().any(|identity| identity_matches(identity, &domain))
}

fn identity_matches(identity: &str, domain: &str) -> bool {
    let identity = identity.trim().trim_end_matches('.').to_ascii_lowercase();
    if identity == domain {
        return true;
    }
    if let Some(suffix) = identity.strip_prefix("*.") {
        let suffix = format!(".{suffix}");
        if !domain.ends_with(&suffix) {
            return false;
        }
        let left = &domain[..domain.len() - suffix.len()];
        return !left.is_empty() && !left.contains('.');
    }
    false
}

fn is_tls_dns_name(domain: &str) -> bool {
    if domain.is_empty() || domain.eq_ignore_ascii_case("localhost") {
        return false;
    }
    if (domain.starts_with('[') && domain.ends_with(']')) || domain.parse::<std::net::IpAddr>().is_ok()
    {
        return false;
    }
    domain.contains('.')
}

impl TlsProviderConfig {
    pub fn resolve(&self, sites: &[SiteConfig]) -> TlsProviderKind {
        match self.provider {
            TlsProviderKind::Cloudflare => TlsProviderKind::Cloudflare,
            TlsProviderKind::Command => TlsProviderKind::Command,
            TlsProviderKind::None => {
                if self.cloudflare_api_token.is_some()
                    && sites
                        .iter()
                        .any(|site| matches!(site.forwarding, ForwardingConfig::Cloudflare))
                {
                    TlsProviderKind::Cloudflare
                } else if self.fetch_command.is_some() {
                    TlsProviderKind::Command
                } else {
                    TlsProviderKind::None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_identity_covers_subdomain() {
        let identities = vec!["*.example.com".to_owned()];
        assert!(domain_covered_by_cert("app.example.com", &identities));
        assert!(!domain_covered_by_cert("example.com", &identities));
    }

    #[test]
    fn skips_localhost_and_ip_domains() {
        let sites = vec![
            SiteConfig {
                domains: vec![
                    "localhost".to_owned(),
                    "127.0.0.1".to_owned(),
                    "::1".to_owned(),
                    "example.com".to_owned(),
                ],
                target: crate::configs::ProxyTarget::Direct {
                    upstream: crate::configs::UpstreamConfig {
                        address: "127.0.0.1:8080".to_owned(),
                        tls: false,
                        sni: String::new(),
                        host: None,
                        ca_path: None,
                        base_path: String::new(),
                    },
                },
                internal_routes: Vec::new(),
                redirects: Vec::new(),
                access: crate::configs::AccessPolicy::All,
                cache: crate::configs::CacheConfig::default(),
                forwarding: crate::configs::ForwardingConfig::Cloudflare,
                logging: None,
            },
        ];
        assert_eq!(proxied_tls_domains(&sites), vec!["example.com".to_owned()]);
        assert_eq!(
            cloudflare_hostnames(&["example.com".to_owned(), "app.example.com".to_owned()]),
            vec![
                "*.example.com".to_owned(),
                "app.example.com".to_owned(),
                "example.com".to_owned()
            ]
        );
    }
}
