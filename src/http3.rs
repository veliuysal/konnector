use crate::{
    configs::{HttpVersion, LogLevel, UpstreamConfig},
    error_pages,
    forwarding,
    path_rewrite,
    proxy::{RequestContext, SiteRuntime},
    request_logging,
    upstreams,
};
use bytes::Buf;
use http::{header, Uri};
use once_cell::sync::OnceCell;
use pingora::{http::ResponseHeader, prelude::*};
use quinn::{ClientConfig, Endpoint};
use rustls::pki_types::ServerName;
use std::{fs::File, io::BufReader, net::SocketAddr, sync::Arc};

static H3_ENDPOINT: OnceCell<Endpoint> = OnceCell::new();

pub async fn proxy_if_needed(
    session: &mut Session,
    ctx: &mut RequestContext,
    sites: &[SiteRuntime],
    default_logging: LogLevel,
    root_site: Option<usize>,
) -> Result<bool> {
    let Some(site_index) = ctx.site else {
        return Ok(false);
    };
    let Some(site) = sites.get(site_index) else {
        return Ok(false);
    };

    let upstream =
        upstreams::resolve_upstream_selection(site, session.req_header().uri.path(), ctx)?;
    if upstream.http_version != HttpVersion::Http3 {
        return Ok(false);
    }

    ctx.mark_proxied();
    request_logging::log_proxy_started(session, ctx, sites, default_logging, root_site);

    let mut request = session.req_header().clone();
    forwarding::apply(session, &mut request, ctx, sites).await?;
    upstreams::apply_request_transform(sites, &mut request, ctx).await?;
    path_rewrite::prepare(session, ctx, sites).await;

    if let Err(error) = proxy_request(session, &request, upstream, ctx).await {
        log::error!("http/3 upstream error: {error}");
        error_pages::respond(session, 502).await?;
    }
    Ok(true)
}

async fn proxy_request(
    session: &mut Session,
    request: &RequestHeader,
    upstream: &UpstreamConfig,
    ctx: &mut RequestContext,
) -> Result<()> {
    let addr = resolve_upstream_addr(&upstream.address).await?;
    let tls_config = build_tls_config(upstream)?;
    let quic_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls_config).map_err(|error| {
            Error::explain(
                ErrorType::InternalError,
                format!("invalid http/3 tls config: {error}"),
            )
        })?,
    ));
    let endpoint = h3_endpoint()?;
    let connection = endpoint
        .connect_with(quic_config, addr, &upstream.sni)
        .map_err(|error| {
            Error::explain(
                ErrorType::ConnectError,
                format!("http/3 connect failed: {error}"),
            )
        })?
        .await
        .map_err(|error| {
            Error::explain(
                ErrorType::ConnectError,
                format!("http/3 handshake failed: {error}"),
            )
        })?;

    let quinn_conn = h3_quinn::Connection::new(connection);
    let (mut driver, mut send_request) = h3::client::new(quinn_conn)
        .await
        .map_err(|error| {
            Error::explain(
                ErrorType::InternalError,
                format!("http/3 client init failed: {error}"),
            )
        })?;

    let drive = tokio::spawn(async move {
        let _ = futures::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    let http_request = build_http_request(request, upstream)?;
    let mut stream = send_request
        .send_request(http_request)
        .await
        .map_err(|error| {
            Error::explain(
                ErrorType::WriteError,
                format!("http/3 request failed: {error}"),
            )
        })?;

    loop {
        match session.read_request_body().await? {
            Some(chunk) => {
                stream.send_data(chunk).await.map_err(|error| {
                    Error::explain(
                        ErrorType::WriteError,
                        format!("http/3 request body failed: {error}"),
                    )
                })?;
            }
            None => break,
        }
    }
    stream.finish().await.map_err(|error| {
        Error::explain(
            ErrorType::WriteError,
            format!("http/3 request finish failed: {error}"),
        )
    })?;

    let response = stream.recv_response().await.map_err(|error| {
            Error::explain(
                ErrorType::ReadError,
                format!("http/3 response headers failed: {error}"),
            )
        })?;

    let mut response_header = to_response_header(&response)?;
    path_rewrite::upstream_response_filter(&mut response_header, ctx).await?;

    session
        .write_response_header(Box::new(response_header), false)
        .await?;

    loop {
        match stream.recv_data().await.map_err(|error| {
            Error::explain(
                ErrorType::ReadError,
                format!("http/3 response body failed: {error}"),
            )
        })? {
            Some(mut chunk) => {
                let bytes = chunk.copy_to_bytes(chunk.remaining());
                let mut body = Some(bytes);
                let _ = path_rewrite::upstream_response_body_filter(&mut body, false, ctx)?;
                session.write_response_body(body, false).await?;
            }
            None => {
                let mut body = None;
                let _ = path_rewrite::upstream_response_body_filter(&mut body, true, ctx)?;
                session.write_response_body(body, true).await?;
                break;
            }
        }
    }

    drop(stream);
    drop(send_request);
    drive.abort();
    Ok(())
}

fn build_http_request(request: &RequestHeader, upstream: &UpstreamConfig) -> Result<http::Request<()>> {
    let authority = path_rewrite::upstream_host_header(upstream);
    let path = request
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let uri: Uri = path.parse().map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("invalid http/3 request uri: {error}"),
        )
    })?;

    let mut builder = http::Request::builder()
        .method(request.method.clone())
        .uri(uri)
        .version(http::Version::HTTP_3)
        .header(header::HOST, authority);

    for (name, value) in request.headers.iter() {
        if is_hop_by_hop(name.as_str()) || name == header::HOST {
            continue;
        }
        builder = builder.header(name, value.as_bytes());
    }

    builder.body(()).map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("invalid http/3 request: {error}"),
        )
    })
}

fn to_response_header(response: &http::Response<()>) -> Result<ResponseHeader> {
    let mut header = ResponseHeader::build(response.status().as_u16(), Some(response.headers().len()))?;
    for (name, value) in response.headers() {
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        header.insert_header(name.as_str().to_owned(), value.as_bytes().to_vec())?;
    }
    Ok(header)
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

async fn resolve_upstream_addr(address: &str) -> Result<SocketAddr> {
    let mut addresses = tokio::net::lookup_host(address).await.map_err(|error| {
        Error::explain(
            ErrorType::ConnectError,
            format!("cannot resolve http/3 upstream {address}: {error}"),
        )
    })?;
    addresses.next().ok_or_else(|| {
        Error::explain(
            ErrorType::ConnectError,
            format!("no addresses for http/3 upstream {address}"),
        )
    })
}

fn build_tls_config(upstream: &UpstreamConfig) -> Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = &upstream.ca_path {
        let file = File::open(path).map_err(|error| {
            Error::explain(
                ErrorType::InternalError,
                format!("cannot read upstream CA {path}: {error}"),
            )
        })?;
        let mut reader = BufReader::new(file);
        for cert in rustls_pemfile::certs(&mut reader) {
            let cert = cert.map_err(|error| {
                Error::explain(
                    ErrorType::InternalError,
                    format!("invalid upstream CA {path}: {error}"),
                )
            })?;
            roots.add(cert).map_err(|error| {
                Error::explain(
                    ErrorType::InternalError,
                    format!("invalid upstream CA {path}: {error}"),
                )
            })?;
        }
    }
    let _ = ServerName::try_from(upstream.sni.as_str()).map_err(|error| {
        Error::explain(
            ErrorType::InternalError,
            format!("invalid http/3 sni: {error}"),
        )
    })?;
    Ok(rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

fn h3_endpoint() -> Result<&'static Endpoint> {
    H3_ENDPOINT.get_or_try_init(|| {
        let endpoint = Endpoint::client("[::]:0".parse().unwrap()).map_err(|error| {
            Error::explain(
                ErrorType::InternalError,
                format!("cannot create http/3 client endpoint: {error}"),
            )
        })?;
        Ok(endpoint)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configs::HttpVersion;
    use http::Method;

    #[test]
    fn hop_by_hop_headers_are_filtered() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(!is_hop_by_hop("content-type"));
    }

    #[test]
    fn http3_request_uses_authority_and_path() {
        let upstream = UpstreamConfig {
            address: "backend.example.com:443".into(),
            tls: true,
            sni: "backend.example.com".into(),
            host: Some("backend.example.com".into()),
            ca_path: None,
            base_path: String::new(),
            http_version: HttpVersion::Http3,
        };
        let mut request = RequestHeader::build(Method::GET, b"/api/health?x=1", None).unwrap();
        request.insert_header("host", "public.example.com").unwrap();
        request
            .insert_header("accept", "application/json")
            .unwrap();
        let http_request = build_http_request(&request, &upstream).unwrap();
        assert_eq!(http_request.uri().path(), "/api/health");
        assert_eq!(
            http_request
                .headers()
                .get(header::HOST)
                .and_then(|value| value.to_str().ok()),
            Some("backend.example.com")
        );
        assert_eq!(http_request.version(), http::Version::HTTP_3);
    }
}
