use crate::configs::{ForwardingConfig, HttpsConfig, SiteConfig, TlsProviderConfig, TlsProviderKind};
use openssl::{
    hash::MessageDigest,
    pkey::PKey,
    rsa::Rsa,
    x509::{X509NameBuilder, X509ReqBuilder, X509},
};
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
    proxied_tls_domains_with(sites, false)
}

/// Domains for certificate issuance. Cloudflare Origin CA can cover wildcards.
pub fn certificate_domains(sites: &[SiteConfig], kind: TlsProviderKind) -> Vec<String> {
    let allow_wildcards = kind == TlsProviderKind::Cloudflare;
    proxied_tls_domains_with(sites, allow_wildcards)
}

fn proxied_tls_domains_with(sites: &[SiteConfig], allow_wildcards: bool) -> Vec<String> {
    let mut domains = HashSet::new();
    for site in sites {
        if !site.listen.https {
            continue;
        }
        for domain in &site.domains {
            let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
            if crate::domain_routing::is_wildcard(&normalized) {
                if !allow_wildcards {
                    // Let's Encrypt HTTP-01 cannot issue wildcard certs; skip for ACME SAN list.
                    // Routing still matches *.example.com. Use Cloudflare Origin CA for *. certs,
                    // or list each subdomain explicitly for ACME.
                    log::warn!(
                        "skipping wildcard {normalized} for certificate issuance; \
                         list concrete hostnames for Let's Encrypt, or use Cloudflare for *. certs"
                    );
                    continue;
                }
            }
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
        let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        if normalized.is_empty() || crate::domain_routing::is_wildcard(&normalized) {
            if crate::domain_routing::is_wildcard(&normalized) {
                hostnames.insert(normalized);
            }
            continue;
        }
        // Only names the site configures — do not invent a parent apex like `kon.ag`
        // for `reg.kon.ag` (CF zone may be the subdomain; inventing apex causes error 1010).
        hostnames.insert(normalized.clone());
        // Cover first-level children of this hostname when it is itself a zone apex.
        hostnames.insert(format!("*.{normalized}"));
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
    let kind = provider.resolve(sites);
    let domains = certificate_domains(sites, kind);
    if domains.is_empty() {
        log::warn!(
            "HTTPS is enabled but no site domains are ready yet; \
             serving a temporary self-signed certificate until sites are configured"
        );
        let placeholder = vec!["localhost".to_owned()];
        let _ = crate::acme::prepare_for_startup(https, &placeholder, provider)?;
        return Ok(());
    }
    if kind == TlsProviderKind::Acme {
        // ACME needs HTTP-01 on :80; placeholder certs may be used until issuance finishes.
        let _ = crate::acme::prepare_for_startup(https, &domains, provider)?;
        return Ok(());
    }
    // Cloudflare / command: issue or renew like certbot when missing, mismatched, or expiring.
    match validate_certificate_files(https, &domains) {
        Ok(()) => {
            if crate::acme::certificate_expires_within(&https.certificate_path, 30) {
                log::info!("TLS certificate expires within 30 days; renewing");
                match refresh_certificate(https, sites, provider) {
                    Ok(()) => {
                        let domains = certificate_domains(sites, kind);
                        if let Err(error) = validate_certificate_files(https, &domains) {
                            log::warn!(
                                "renewed certificate still invalid ({error}); keeping previous files"
                            );
                        }
                    }
                    Err(error) => {
                        // Keep serving the existing (even short-lived) cert — do not disable HTTPS.
                        log::warn!(
                            "certificate renewal failed ({error}); continuing with existing certificate"
                        );
                    }
                }
            }
            Ok(())
        }
        Err(error) => {
            log::warn!("TLS certificate needs issuance/renewal: {error}");
            match refresh_certificate(https, sites, provider) {
                Ok(()) => {
                    let domains = certificate_domains(sites, kind);
                    validate_certificate_files(https, &domains)
                }
                Err(refresh_error) => {
                    log::warn!(
                        "auto certificate fetch failed ({refresh_error}); \
                         installing temporary self-signed cert so HTTPS can still listen"
                    );
                    match crate::acme::prepare_for_startup(https, &domains, provider) {
                        Ok(_) => Ok(()),
                        Err(placeholder_error) => Err(format!(
                            "cannot install temporary certificate under {}: {placeholder_error} \
                             (ensure TLS_DIR is writable by the konnector user)",
                            provider.tls_dir.as_deref().unwrap_or("/etc/ssl/konnector")
                        )),
                    }
                }
            }
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
    let kind = provider.resolve(sites);
    let domains = certificate_domains(sites, kind);
    if domains.is_empty() {
        return Err("no proxied DNS domains require TLS coverage".into());
    }
    match kind {
        TlsProviderKind::Acme => {
            log::info!(
                "requesting Let's Encrypt certificate for {}",
                domains.join(", ")
            );
            crate::acme::issue_certificate(https, &domains, provider)
        }
        TlsProviderKind::Cloudflare => {
            let token = provider
                .cloudflare_api_token
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or("CLOUDFLARE_API_TOKEN is required for Cloudflare Origin CA")?;
            let hostnames = cloudflare_hostnames(&domains);
            log::info!(
                "requesting Cloudflare origin certificate for {}",
                hostnames.join(", ")
            );
            let (certificate, private_key) = fetch_cloudflare_origin_certificate(token, &hostnames)?;
            write_certificate_files(https, &certificate, &private_key)?;
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
            "TLS certificate mismatch and no TLS provider is configured; \
             set CLOUDFLARE_API_TOKEN or TLS_PROVIDER=acme"
                .into(),
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
    let (csr, private_key) = generate_origin_csr(hostnames)?;
    let body = serde_json::json!({
        "csr": csr,
        "hostnames": hostnames,
        "requested_validity": DEFAULT_ORIGIN_VALIDITY_DAYS,
        "request_type": "origin-rsa",
    });
    let response = match ureq::post(CLOUDFLARE_ORIGIN_CERT_URL)
        .set("Authorization", &format!("Bearer {api_token}"))
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let detail = response
                .into_string()
                .unwrap_or_else(|_| "(empty body)".to_owned());
            return Err(format!(
                "Cloudflare Origin CA HTTP {status}: {detail} \
                 (token needs Zone → SSL and Certificates → Edit on the Cloudflare zone \
                 that owns these hostnames; for a subdomain zone list that hostname, \
                 e.g. reg.kon.ag — do not require the parent apex)"
            ));
        }
        Err(error) => {
            return Err(format!("Cloudflare certificate request failed: {error}"));
        }
    };
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
    Ok((certificate.to_owned(), private_key))
}

fn generate_origin_csr(hostnames: &[String]) -> Result<(String, String), String> {
    let rsa = Rsa::generate(2048).map_err(|error| format!("RSA key: {error}"))?;
    let key = PKey::from_rsa(rsa).map_err(|error| format!("pkey: {error}"))?;
    let cn = hostnames
        .iter()
        .find(|name| !name.starts_with("*."))
        .map(String::as_str)
        .or_else(|| hostnames.first().map(String::as_str))
        .unwrap_or("konnector");

    let mut name = X509NameBuilder::new().map_err(|error| format!("X509 name: {error}"))?;
    name.append_entry_by_text("CN", cn)
        .map_err(|error| format!("X509 CN: {error}"))?;
    let name = name.build();

    let mut req = X509ReqBuilder::new().map_err(|error| format!("CSR builder: {error}"))?;
    req.set_subject_name(&name)
        .map_err(|error| format!("CSR subject: {error}"))?;
    req.set_pubkey(&key)
        .map_err(|error| format!("CSR pubkey: {error}"))?;
    req.sign(&key, MessageDigest::sha256())
        .map_err(|error| format!("CSR sign: {error}"))?;
    let csr = req.build();
    let csr_pem = String::from_utf8(
        csr.to_pem()
            .map_err(|error| format!("CSR pem: {error}"))?,
    )
    .map_err(|error| format!("CSR utf8: {error}"))?;
    let private_key = String::from_utf8(
        key.private_key_to_pem_pkcs8()
            .map_err(|error| format!("key pem: {error}"))?,
    )
    .map_err(|error| format!("key utf8: {error}"))?;
    Ok((csr_pem, private_key))
}

fn run_fetch_command(command: &str, https: &HttpsConfig) -> Result<(), String> {
    #[cfg(unix)]
    let mut process = Command::new("sh");
    #[cfg(unix)]
    {
        process.arg("-c").arg(command);
    }
    #[cfg(windows)]
    let mut process = Command::new("cmd");
    #[cfg(windows)]
    {
        process.args(["/C", command]);
    }
    let status = process
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

pub fn write_certificate_files(
    https: &HttpsConfig,
    certificate: &str,
    private_key: &str,
) -> Result<(), String> {
    write_atomic(&https.certificate_path, certificate)?;
    write_atomic(&https.private_key_path, private_key)?;
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
    // Bundled example.yaml placeholders must not enter ACME / cert checks.
    if matches!(
        domain,
        "example.com"
            | "www.example.com"
            | "example.org"
            | "example.net"
            | "www.example.org"
            | "www.example.net"
    ) {
        return false;
    }
    domain.contains('.')
}

impl TlsProviderConfig {
    pub fn resolve(&self, sites: &[SiteConfig]) -> TlsProviderKind {
        match self.provider {
            TlsProviderKind::Acme => TlsProviderKind::Acme,
            TlsProviderKind::Cloudflare => TlsProviderKind::Cloudflare,
            TlsProviderKind::Command => TlsProviderKind::Command,
            TlsProviderKind::None => {
                // Token alone selects Cloudflare Origin CA (no TLS_PROVIDER / forwarding required).
                if self
                    .cloudflare_api_token
                    .as_deref()
                    .is_some_and(|token| !token.trim().is_empty())
                {
                    TlsProviderKind::Cloudflare
                } else if self.fetch_command.is_some() {
                    TlsProviderKind::Command
                } else if sites
                    .iter()
                    .any(|site| matches!(site.forwarding, ForwardingConfig::Cloudflare))
                {
                    // Legacy hint: cloudflare forwarding without token still does not issue.
                    TlsProviderKind::None
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
                    "myapp.com".to_owned(),
                ],
                target: crate::configs::ProxyTarget::Direct {
                    upstream: crate::configs::UpstreamConfig {
                        address: "127.0.0.1:8080".to_owned(),
                        tls: false,
                        sni: String::new(),
                        host: None,
                        ca_path: None,
                        base_path: String::new(),
                        http_version: crate::configs::HttpVersion::Auto,
                    },
                },
                internal_routes: Vec::new(),
                redirects: Vec::new(),
                access: crate::configs::AccessPolicy::All,
                cache: crate::configs::CacheConfig::default(),
                forwarding: crate::configs::ForwardingConfig::Cloudflare,
                logging: None,
                http: crate::configs::HttpSettings::default(),
                listen: crate::configs::ListenMode::both(),
                traffic: crate::configs::TrafficMode::default(),
                redirect_https: false,
                enabled: true,
                source_file: "example".to_owned(),
            },
        ];
        assert_eq!(proxied_tls_domains(&sites), vec!["myapp.com".to_owned()]);
        assert_eq!(
            cloudflare_hostnames(&["example.com".to_owned(), "app.example.com".to_owned()]),
            vec![
                "*.app.example.com".to_owned(),
                "*.example.com".to_owned(),
                "app.example.com".to_owned(),
                "example.com".to_owned(),
            ]
        );
    }

    #[test]
    fn cloudflare_hostnames_follow_configured_names_not_parent_apex() {
        // Subdomain zone `reg.kon.ag` must not invent `kon.ag` / `*.kon.ag`.
        assert_eq!(
            cloudflare_hostnames(&["reg.kon.ag".to_owned()]),
            vec!["*.reg.kon.ag".to_owned(), "reg.kon.ag".to_owned()]
        );
        assert_eq!(
            cloudflare_hostnames(&["reg.kon.ag".to_owned(), "www.reg.kon.ag".to_owned()]),
            vec![
                "*.reg.kon.ag".to_owned(),
                "*.www.reg.kon.ag".to_owned(),
                "reg.kon.ag".to_owned(),
                "www.reg.kon.ag".to_owned(),
            ]
        );
    }
}
