use bytes::Bytes;
use pingora::{prelude::*, proxy::FailToProxy};

pub async fn respond_to_proxy_failure(session: &mut Session, error: &Error) -> FailToProxy {
    let status = status_for(error);
    if status > 0 {
        if let Err(write_error) = respond(session, status).await {
            log::warn!("failed to send error page: {write_error}");
        }
    }
    FailToProxy {
        error_code: status,
        can_reuse_downstream: false,
    }
}

pub async fn respond(session: &mut Session, status: u16) -> Result<()> {
    let (title, message) = copy_for(status);
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title></head><body style=\"font-family:sans-serif;max-width:720px;\
         margin:5rem auto;padding:0 1rem\"><h1>{title}</h1><p>{message}</p>\
         <small>Error {status}</small></body></html>"
    );
    let body = Bytes::from(html);
    let mut header = ResponseHeader::build(status, Some(4))?;
    header.insert_header("content-type", "text/html; charset=utf-8")?;
    header.insert_header("cache-control", "no-store")?;
    header.insert_header("content-length", body.len().to_string())?;
    session
        .write_response_header(Box::new(header), false)
        .await?;
    session.write_response_body(Some(body), true).await
}

fn status_for(error: &Error) -> u16 {
    match error.etype() {
        ErrorType::HTTPStatus(status) => *status,
        _ => match error.esource() {
            ErrorSource::Upstream => 502,
            ErrorSource::Internal | ErrorSource::Unset => 500,
            // A closed downstream connection cannot receive an error page.
            ErrorSource::Downstream => 0,
        },
    }
}

fn copy_for(status: u16) -> (&'static str, &'static str) {
    match status {
        404 => ("Page not found", "The requested page could not be found."),
        502..=504 => (
            "Service temporarily unavailable",
            "Please try again in a few moments.",
        ),
        _ => ("Something went wrong", "Please try again later."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_safe_public_messages() {
        assert_eq!(copy_for(404).0, "Page not found");
        assert_eq!(copy_for(502).0, "Service temporarily unavailable");
        assert_eq!(copy_for(500).0, "Something went wrong");
    }
}
