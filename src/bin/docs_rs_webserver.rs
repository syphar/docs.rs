use anyhow::{Context as _, Result};
use docs_rs::context::BinContext;
use docs_rs::start_web_server;
use std::env;
use std::net::SocketAddr;

fn main() -> Result<()> {
    let _sentry_guard = docs_rs::logging::initialize_logging();

    let socket_addr: SocketAddr = env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:3000".into())
        .parse()
        .context("Failed to parse socket address")?;

    let context = BinContext::new();
    start_web_server(Some(socket_addr), &context)?;

    Ok(())
}
