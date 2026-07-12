use crate::configs::InternalRouteConfig;

pub fn find(path: &str, routes: &[InternalRouteConfig]) -> Option<usize> {
    routes.iter().position(|route| {
        path == route.prefix.trim_end_matches('/')
            || path == route.prefix
            || (route.prefix.ends_with('/') && path.starts_with(&route.prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configs::{HttpVersion, UpstreamConfig};

    #[test]
    fn finds_only_the_internal_prefix() {
        let routes = vec![InternalRouteConfig {
            prefix: "/api/".to_owned(),
            upstream: UpstreamConfig {
                address: "127.0.0.1:9080".to_owned(),
                tls: false,
                sni: String::new(),
                host: None,
                ca_path: None,
                base_path: String::new(),
                http_version: HttpVersion::Auto,
            },
            strip_prefix: true,
        }];
        assert_eq!(find("/api/users", &routes), Some(0));
        assert_eq!(find("/api", &routes), Some(0));
        assert_eq!(find("/api-admin", &routes), None);
    }
}
