use anyhow::Result;
use clap::Parser;
use docs_rs::context::BinContext;
use docs_rs::start_web_server;
use std::env;
use std::net::SocketAddr;

#[derive(Parser, Debug)]
#[command(
    about = env!("CARGO_PKG_DESCRIPTION"),
    version = docs_rs::BUILD_VERSION,
    rename_all = "kebab-case",
)]
struct Args {
    #[arg(name = "SOCKET_ADDR", default_value = "0.0.0.0:3000")]
    socket_addr: SocketAddr,
}
fn main() -> Result<()> {
    let args = Args::parse();

    let _sentry_guard = docs_rs::logging::initialize_logging();
    let context = BinContext::new();
    start_web_server(Some(args.socket_addr), &context)?;

    Ok(())
}
