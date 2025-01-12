use anyhow::{Context as _, Result};
use docs_rs::context::BinContext;
use docs_rs::utils::queue_builder;
use docs_rs::{start_background_metrics_webserver, Context, RustwideBuilder};
use std::env;
use std::net::SocketAddr;
use tracing_log::LogTracer;

fn main() -> Result<()> {
    // set the global log::logger for backwards compatibility
    // through rustwide.
    rustwide::logging::init_with(LogTracer::new());

    let _sentry_guard = docs_rs::logging::initialize_logging();

    let metric_server_socket_addr: SocketAddr = env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:3000".into())
        .parse()
        .context("Failed to parse socket address")?;

    let context = BinContext::new();

    start_background_metrics_webserver(Some(metric_server_socket_addr), &context)?;

    let build_queue = context.build_queue()?;
    let config = context.config()?;
    let rustwide_builder = RustwideBuilder::init(&context)?;
    queue_builder(&context, rustwide_builder, build_queue, config)?;

    Ok(())
}
