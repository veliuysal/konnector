use crate::{
    configs::{
        AccessPolicy, CacheConfig, ForwardingConfig, InternalRouteConfig, LogLevel, RedirectRule,
        UpstreamConfig,
    },
    path_rewrite::PathRewriteState,
};
use pingora::{
    cache::MemCache,
    prelude::{LoadBalancer, RoundRobin},
};
use std::sync::Arc;

pub struct SiteRuntime {
    pub domains: Vec<String>,
    pub target: UpstreamRuntime,
    pub internal_routes: Vec<InternalRouteConfig>,
    pub redirects: Vec<RedirectRule>,
    pub access: AccessPolicy,
    pub cache: CacheConfig,
    pub cache_storage: &'static MemCache,
    pub forwarding: ForwardingConfig,
    pub logging: LogLevel,
}

impl SiteRuntime {
    pub fn primary_domain(&self) -> &str {
        self.domains
            .first()
            .map(String::as_str)
            .unwrap_or("<no-domain>")
    }
}

pub enum UpstreamRuntime {
    Direct(UpstreamConfig),
    LoadBalanced {
        upstreams: Vec<UpstreamConfig>,
        load_balancer: Arc<LoadBalancer<RoundRobin>>,
    },
}

impl UpstreamRuntime {
    pub fn get(&self, index: usize) -> Option<&UpstreamConfig> {
        match self {
            Self::Direct(upstream) => (index == 0).then_some(upstream),
            Self::LoadBalanced { upstreams, .. } => upstreams.get(index),
        }
    }
}

#[derive(Default)]
pub struct RequestContext {
    pub site: Option<usize>,
    pub upstream: Option<usize>,
    pub internal_route: Option<usize>,
    pub path_rewrite: Option<PathRewriteState>,
    pub skip_access_log: bool,
    pub started_at: Option<std::time::Instant>,
}
