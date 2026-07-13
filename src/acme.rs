use crate::configs::{HttpsConfig, TlsProviderConfig};
use instant_acme::{
    Account, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder, RetryPolicy,
};
use once_cell::sync::Lazy;
use openssl::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    hash::MessageDigest,
    pkey::PKey,
    rsa::Rsa,
    x509::{
        extension::{BasicConstraints, KeyUsage, SubjectAlternativeName},
        X509NameBuilder, X509,
    },
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";
const RENEW_WITHIN_DAYS: u32 = 30;
const BOOTSTRAP_DELAY: Duration = Duration::from_secs(3);
const RESTART_EXIT_CODE: i32 = 75;

static CHALLENGES: Lazy<Arc<RwLock<HashMap<String, String>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

pub fn challenge_response(token: &str) -> Option<String> {
    CHALLENGES
        .read()
        .ok()?
        .get(token)
        .cloned()
}

pub fn challenge_token_from_path(path: &str) -> Option<&str> {
    path.strip_prefix(ACME_CHALLENGE_PREFIX)
        .filter(|token| !token.is_empty() && !token.contains('/'))
}

fn set_challenge(token: &str, key_auth: &str) {
    if let Ok(mut guard) = CHALLENGES.write() {
        guard.insert(token.to_owned(), key_auth.to_owned());
    }
}

fn clear_challenge(token: &str) {
    if let Ok(mut guard) = CHALLENGES.write() {
        guard.remove(token);
    }
}

pub fn account_dir(provider: &TlsProviderConfig) -> PathBuf {
    let root = provider
        .tls_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(crate::paths::ssl_dir);
    root.join("acme")
}

fn account_path(provider: &TlsProviderConfig) -> PathBuf {
    account_dir(provider).join("account.json")
}

/// Ensure certificates exist for ACME. Writes a temporary self-signed cert if needed
/// so HTTPS can bind, then a background task obtains a real Let's Encrypt certificate.
pub fn prepare_for_startup(
    https: &HttpsConfig,
    domains: &[String],
    _provider: &TlsProviderConfig,
) -> Result<bool, String> {
    if domains.is_empty() {
        return Err("ACME requires at least one DNS domain in site configs".into());
    }
    match validate_or_renewal_needed(https, domains) {
        Ok(false) => {
            log::info!(
                "ACME certificate is valid for {}",
                domains.join(", ")
            );
            Ok(false)
        }
        Ok(true) | Err(_) => {
            if validate_files_exist(https).is_err() {
                log::warn!(
                    "no usable TLS certificate yet; installing temporary self-signed cert for {}",
                    domains.join(", ")
                );
                install_self_signed(https, domains)?;
            }
            Ok(true)
        }
    }
}

/// Returns Ok(true) when the cert should be renewed soon, Ok(false) when fine.
fn validate_or_renewal_needed(https: &HttpsConfig, domains: &[String]) -> Result<bool, String> {
    crate::ssl::validate_certificate_files(https, domains)?;
    if certificate_expires_within(&https.certificate_path, RENEW_WITHIN_DAYS) {
        Ok(true)
    } else {
        Ok(false)
    }
}

fn validate_files_exist(https: &HttpsConfig) -> Result<(), String> {
    if !Path::new(&https.certificate_path).is_file() {
        return Err("certificate file missing".into());
    }
    if !Path::new(&https.private_key_path).is_file() {
        return Err("private key file missing".into());
    }
    Ok(())
}

pub fn certificate_expires_within(cert_path: &str, days: u32) -> bool {
    let Ok(pem) = fs::read(cert_path) else {
        return true;
    };
    let Ok(cert) = X509::from_pem(&pem) else {
        return true;
    };
    let Ok(threshold) = Asn1Time::days_from_now(days) else {
        return true;
    };
    matches!(
        cert.not_after().compare(&threshold).ok(),
        Some(std::cmp::Ordering::Less)
    )
}

pub fn issue_certificate(
    https: &HttpsConfig,
    domains: &[String],
    provider: &TlsProviderConfig,
) -> Result<(), String> {
    if domains.is_empty() {
        return Err("ACME requires at least one DNS domain".into());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("konnector-acme")
        .build()
        .map_err(|error| format!("cannot create ACME runtime: {error}"))?;
    runtime.block_on(issue_certificate_async(https, domains, provider))
}

pub fn start_background_issuer(
    https: HttpsConfig,
    domains: Vec<String>,
    provider: TlsProviderConfig,
) {
    thread::Builder::new()
        .name("acme-issuer".into())
        .spawn(move || {
            thread::sleep(BOOTSTRAP_DELAY);
            log::info!(
                "requesting Let's Encrypt certificate for {}",
                domains.join(", ")
            );
            match issue_certificate(&https, &domains, &provider) {
                Ok(()) => {
                    log::info!("Let's Encrypt certificate installed; restarting to load it");
                    std::process::exit(RESTART_EXIT_CODE);
                }
                Err(error) => log::error!("ACME certificate issuance failed: {error}"),
            }
        })
        .unwrap_or_else(|error| panic!("cannot start ACME issuer: {error}"));
}

async fn issue_certificate_async(
    https: &HttpsConfig,
    domains: &[String],
    provider: &TlsProviderConfig,
) -> Result<(), String> {
    let account = load_or_create_account(provider).await?;
    let identifiers = domains
        .iter()
        .map(|domain| Identifier::Dns(domain.clone()))
        .collect::<Vec<_>>();
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(|error| format!("ACME new_order failed: {error}"))?;

    let mut authorizations = order.authorizations();
    let mut active_tokens = Vec::new();
    while let Some(result) = authorizations.next().await {
        let mut authz = result.map_err(|error| format!("ACME authorization failed: {error}"))?;
        if matches!(
            authz.status,
            instant_acme::AuthorizationStatus::Valid
        ) {
            continue;
        }
        let mut challenge = authz
            .challenge(ChallengeType::Http01)
            .ok_or_else(|| "ACME order has no HTTP-01 challenge".to_string())?;
        let token = challenge.token.clone();
        let key_auth = challenge.key_authorization().as_str().to_owned();
        set_challenge(&token, &key_auth);
        active_tokens.push(token);
        challenge
            .set_ready()
            .await
            .map_err(|error| format!("ACME challenge set_ready failed: {error}"))?;
    }

    let ready = order
        .poll_ready(&RetryPolicy::default())
        .await
        .map_err(|error| format!("ACME poll_ready failed: {error}"))?;
    if ready != instant_acme::OrderStatus::Ready {
        for token in &active_tokens {
            clear_challenge(token);
        }
        return Err(format!("ACME order not ready: {ready:?}"));
    }

    let private_key = order
        .finalize()
        .await
        .map_err(|error| format!("ACME finalize failed: {error}"))?;
    let certificate = order
        .poll_certificate(&RetryPolicy::default())
        .await
        .map_err(|error| format!("ACME certificate download failed: {error}"))?;

    for token in &active_tokens {
        clear_challenge(token);
    }

    crate::ssl::write_certificate_files(https, &certificate, &private_key)?;
    log::info!(
        "stored Let's Encrypt certificate at {}",
        https.certificate_path
    );
    Ok(())
}

async fn load_or_create_account(provider: &TlsProviderConfig) -> Result<Account, String> {
    let dir = account_dir(provider);
    fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create ACME directory {}: {error}", dir.display()))?;
    let path = account_path(provider);
    let directory = if provider.acme_staging {
        LetsEncrypt::Staging.url()
    } else {
        LetsEncrypt::Production.url()
    }
    .to_owned();

    if path.is_file() {
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read ACME account {}: {error}", path.display()))?;
        let credentials = serde_json::from_str(&raw)
            .map_err(|error| format!("invalid ACME account credentials: {error}"))?;
        return Account::builder()
            .map_err(|error| format!("ACME account builder failed: {error}"))?
            .from_credentials(credentials)
            .await
            .map_err(|error| format!("cannot restore ACME account: {error}"));
    }

    let (account, credentials) = Account::builder()
        .map_err(|error| format!("ACME account builder failed: {error}"))?
        .create(
            &NewAccount {
                contact: &[],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory,
            None,
        )
        .await
        .map_err(|error| format!("ACME account create failed: {error}"))?;

    let serialized = serde_json::to_string_pretty(&credentials)
        .map_err(|error| format!("cannot serialize ACME account: {error}"))?;
    fs::write(&path, serialized)
        .map_err(|error| format!("cannot write ACME account {}: {error}", path.display()))?;
    log::info!("created ACME account at {}", path.display());
    Ok(account)
}

fn install_self_signed(https: &HttpsConfig, domains: &[String]) -> Result<(), String> {
    let rsa = Rsa::generate(2048).map_err(|error| format!("cannot generate RSA key: {error}"))?;
    let key = PKey::from_rsa(rsa).map_err(|error| format!("cannot wrap RSA key: {error}"))?;

    let mut name = X509NameBuilder::new().map_err(|error| format!("X509 name: {error}"))?;
    name.append_entry_by_text("CN", domains.first().map(String::as_str).unwrap_or("konnector"))
        .map_err(|error| format!("X509 CN: {error}"))?;
    let name = name.build();

    let mut builder = X509::builder().map_err(|error| format!("X509 builder: {error}"))?;
    builder
        .set_version(2)
        .map_err(|error| format!("X509 version: {error}"))?;
    let mut serial = BigNum::new().map_err(|error| format!("serial: {error}"))?;
    serial
        .rand(159, MsbOption::MAYBE_ZERO, false)
        .map_err(|error| format!("serial rand: {error}"))?;
    let serial = serial
        .to_asn1_integer()
        .map_err(|error| format!("serial asn1: {error}"))?;
    builder
        .set_serial_number(&serial)
        .map_err(|error| format!("set serial: {error}"))?;
    builder
        .set_subject_name(&name)
        .map_err(|error| format!("subject: {error}"))?;
    builder
        .set_issuer_name(&name)
        .map_err(|error| format!("issuer: {error}"))?;
    builder
        .set_pubkey(&key)
        .map_err(|error| format!("pubkey: {error}"))?;
    let not_before =
        Asn1Time::days_from_now(0).map_err(|error| format!("not_before: {error}"))?;
    let not_after =
        Asn1Time::days_from_now(7).map_err(|error| format!("not_after: {error}"))?;
    builder
        .set_not_before(&not_before)
        .map_err(|error| format!("set not_before: {error}"))?;
    builder
        .set_not_after(&not_after)
        .map_err(|error| format!("set not_after: {error}"))?;
    builder
        .append_extension(BasicConstraints::new().build().map_err(|error| {
            format!("basic constraints: {error}")
        })?)
        .map_err(|error| format!("append basic constraints: {error}"))?;
    builder
        .append_extension(
            KeyUsage::new()
                .digital_signature()
                .key_encipherment()
                .build()
                .map_err(|error| format!("key usage: {error}"))?,
        )
        .map_err(|error| format!("append key usage: {error}"))?;

    let mut san = SubjectAlternativeName::new();
    for domain in domains {
        san.dns(domain);
    }
    let san = san
        .build(&builder.x509v3_context(None, None))
        .map_err(|error| format!("SAN: {error}"))?;
    builder
        .append_extension(san)
        .map_err(|error| format!("append SAN: {error}"))?;
    builder
        .sign(&key, MessageDigest::sha256())
        .map_err(|error| format!("sign: {error}"))?;

    let cert = builder.build();
    let certificate = String::from_utf8(cert.to_pem().map_err(|error| format!("cert pem: {error}"))?)
        .map_err(|error| format!("cert utf8: {error}"))?;
    let private_key =
        String::from_utf8(key.private_key_to_pem_pkcs8().map_err(|error| format!("key pem: {error}"))?)
            .map_err(|error| format!("key utf8: {error}"))?;
    crate::ssl::write_certificate_files(https, &certificate, &private_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_challenge_paths() {
        assert_eq!(
            challenge_token_from_path("/.well-known/acme-challenge/abc"),
            Some("abc")
        );
        assert_eq!(
            challenge_token_from_path("/.well-known/acme-challenge/a/b"),
            None
        );
        assert_eq!(challenge_token_from_path("/health"), None);
        assert_eq!(
            challenge_token_from_path("/.well-known/acme-challenge/"),
            None
        );
    }
}
