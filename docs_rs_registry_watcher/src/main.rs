use clap::Parser;
use docs_rs::utils::logging;
use std::net::SocketAddr;

// Self::StartRegistryWatcher {
//     metric_server_socket_addr,
//     repository_stats_updater,
//     cdn_invalidator,
// } => {
//     if repository_stats_updater == Toggle::Enabled {
//         docs_rs::utils::daemon::start_background_repository_stats_updater(&ctx)?;
//     }
//     if cdn_invalidator == Toggle::Enabled {
//         docs_rs::utils::daemon::start_background_cdn_invalidator(&ctx)?;
//     }

//     start_background_metrics_webserver(Some(metric_server_socket_addr), &ctx)?;

//     docs_rs::utils::watch_registry(ctx.build_queue()?, ctx.config()?, ctx.index()?)?;
// }
/// Simple program to greet a person

#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    #[arg(name = "SOCKET_ADDR", default_value = "0.0.0.0:3000")]
    metric_server_socket_addr: SocketAddr,

    #[arg(long = "repository-stats-updater")]
    repository_stats_updater: bool,

    #[arg(long = "cdn-invalidator")]
    cdn_invalidator: bool,
}

#[tokio::main]
async fn main() {
    let _sentry = logging::initialize();

    println!("Hello, world!");
}
