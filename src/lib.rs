pub mod client;
pub mod config;
pub mod error;
pub mod extract;
pub mod openapi;
pub mod rate_limit;
pub mod resource;
pub mod routes;
pub mod state;
pub mod summary;
pub mod topics;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();
