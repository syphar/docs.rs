mod cache;
mod config;
mod error;
mod extractors;
mod handlers;
pub(crate) mod match_release;
mod metrics;
mod page;
mod routes;
mod utils;

pub use docs_rs_utils::{APP_USER_AGENT, BUILD_VERSION, RUSTDOC_STATIC_STORAGE_PREFIX};
