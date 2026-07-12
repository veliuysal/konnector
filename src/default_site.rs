use bytes::Bytes;
use pingora::prelude::*;

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Konnector</title></head>
<body style="font-family:sans-serif;max-width:720px;margin:5rem auto;padding:0 1rem;text-align:center">
<h1>Server is working</h1>
<p>Konnector is running. Add a site config to serve your application.</p>
</body></html>"#;

pub async fn respond(session: &mut Session) -> Result<bool> {
    let body = Bytes::from_static(PAGE.as_bytes());
    let mut header = ResponseHeader::build(200, Some(4))?;
    header.insert_header("content-type", "text/html; charset=utf-8")?;
    header.insert_header("cache-control", "no-store")?;
    header.insert_header("content-length", body.len().to_string())?;
    session
        .write_response_header(Box::new(header), false)
        .await?;
    session.write_response_body(Some(body), true).await?;
    Ok(true)
}
