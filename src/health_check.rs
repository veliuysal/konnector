use bytes::Bytes;
use pingora::prelude::*;

pub async fn respond(session: &mut Session) -> Result<bool> {
    if session.req_header().uri.path() != "/_health" || !is_local(session) {
        return Ok(false);
    }
    let body = Bytes::from_static(b"ok\n");
    let mut header = ResponseHeader::build(200, Some(3))?;
    header.insert_header("content-type", "text/plain; charset=utf-8")?;
    header.insert_header("cache-control", "no-store")?;
    header.insert_header("content-length", body.len().to_string())?;
    session
        .write_response_header(Box::new(header), false)
        .await?;
    session.write_response_body(Some(body), true).await?;
    Ok(true)
}

fn is_local(session: &Session) -> bool {
    session
        .as_downstream()
        .client_addr()
        .and_then(|address| address.as_inet())
        .is_some_and(|address| address.ip().is_loopback())
}
