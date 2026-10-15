use std::sync::Arc;

use sqlx::PgPool;

use crate::{config::Config, rate_limit::RateLimiter};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub cfg: Arc<Config>,
    pub limiter: Arc<RateLimiter>,
}

impl AppState {
    pub fn new(pool: PgPool, cfg: Config) -> Self {
        Self {
            pool,
            cfg: Arc::new(cfg),
            limiter: Arc::new(RateLimiter::per_minute()),
        }
    }
}
