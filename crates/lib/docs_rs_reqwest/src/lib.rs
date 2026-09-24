//! Retrying JSON GET requests with a bounded, shared cache.

mod cached_result;
mod client;

pub use cached_result::CachedResult;
pub use client::Client;
