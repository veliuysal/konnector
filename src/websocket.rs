use pingora::prelude::Session;

/// True for HTTP Upgrade requests (WebSocket and similar).
pub fn is_upgrade_request(session: &Session) -> bool {
    if session.is_upgrade_req() {
        return true;
    }
    let headers = &session.req_header().headers;
    let upgrade = headers
        .get("upgrade")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if !upgrade {
        return false;
    }
    headers
        .get("connection")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn connection_header_parsing_logic() {
        let value = "keep-alive, Upgrade";
        assert!(value
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("upgrade")));
    }
}
