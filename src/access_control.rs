use crate::{
    configs::AccessPolicy,
    domain_routing, error_pages,
    proxy::{RequestContext, SiteRuntime},
};
use pingora::prelude::*;

pub async fn reject_disallowed(
    session: &mut Session,
    ctx: &mut RequestContext,
    sites: &[SiteRuntime],
) -> Result<bool> {
    let Some(site_index) = domain_routing::site_for(session, sites) else {
        return Ok(false);
    };
    ctx.site = Some(site_index);
    if is_allowed(session.req_header().uri.path(), &sites[site_index].access) {
        return Ok(false);
    }

    // Hide closed upstream routes instead of advertising them with 403.
    error_pages::respond(session, 404).await?;
    Ok(true)
}

fn is_allowed(path: &str, policy: &AccessPolicy) -> bool {
    match policy {
        AccessPolicy::All => true,
        AccessPolicy::OnlyPrefixes { prefixes } => prefixes.iter().any(|prefix| {
            path == prefix.trim_end_matches('/')
                || path == prefix
                || (prefix.ends_with('/') && path.starts_with(prefix))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_does_not_open_similar_route() {
        let policy = AccessPolicy::OnlyPrefixes {
            prefixes: vec!["/api/".to_owned()],
        };
        assert!(is_allowed("/api/items", &policy));
        assert!(is_allowed("/api", &policy));
        assert!(!is_allowed("/api-admin", &policy));
    }
}
