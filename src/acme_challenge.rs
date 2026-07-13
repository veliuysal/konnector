use bytes::Bytes;
use pingora::prelude::*;

pub async fn respond(session: &mut Session) -> Result<bool> {
    let path = session.req_header().uri.path();
    let Some(token) = crate::acme::challenge_token_from_path(path) else {
        return Ok(false);
    };
    let Some(body) = crate::acme::challenge_response(token) else {
        let mut header = ResponseHeader::build(404, Some(2))?;
        header.insert_header("content-type", "text/plain; charset=utf-8")?;
        header.insert_header("content-length", "0")?;
        session
            .write_response_header(Box::new(header), true)
            .await?;
        return Ok(true);
    };
    let body = Bytes::from(body);
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
