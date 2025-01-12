use anyhow::Result;
use clap::Parser;
use docs_rs::context::BinContext;
use docs_rs::utils::queue_builder;
use docs_rs::{start_background_metrics_webserver, Context, RustwideBuilder};
use std::env;
use std::net::SocketAddr;
use tracing_log::LogTracer;

#[derive(Parser, Debug)]
#[command(
    about = env!("CARGO_PKG_DESCRIPTION"),
    version = docs_rs::BUILD_VERSION,
    rename_all = "kebab-case",
)]
struct Args {
    #[arg(name = "SOCKET_ADDR", default_value = "0.0.0.0:3000")]
    metric_server_socket_addr: SocketAddr,

    #[arg(long = "repository-stats-updater", default_value_t = false)]
    repository_stats_updater: bool,

    #[arg(long = "cdn-invalidator", default_value_t = true)]
    cdn_invalidator: bool,

    #[arg(long = "queue-rebuilds", default_value_t = true)]
    queue_rebuilds: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // set the global log::logger for backwards compatibility
    // through rustwide.
    rustwide::logging::init_with(LogTracer::new());

    let _sentry_guard = docs_rs::logging::initialize_logging();

    let context = BinContext::new();

    start_background_metrics_webserver(Some(args.metric_server_socket_addr), &context)?;

    let build_queue = context.build_queue()?;
    let config = context.config()?;
    let rustwide_builder = RustwideBuilder::init(&context)?;
    queue_builder(&context, rustwide_builder, build_queue, config)?;

    Ok(())
}
