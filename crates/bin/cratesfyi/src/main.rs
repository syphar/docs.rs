use anyhow::Result;
use clap::Parser;
use cratesfyi::daemon::start_daemon;
use docs_rs_context::Context;
use std::env;
use tokio::runtime;

fn main() {
    let runtime = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("error creating application runtime");

    let guard = runtime
        .block_on(docs_rs_logging::init_from_environment())
        .expect("error initializing logging");

    if let Err(err) = CommandLine::parse().handle_args(&runtime) {
        eprintln!("error running watcher: {:?}", err);
        drop(guard);
        std::process::exit(1);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    about = env!("CARGO_PKG_DESCRIPTION"),
    version = docs_rs_utils::BUILD_VERSION,
    rename_all = "kebab-case",
)]
enum CommandLine {
    /// Starts the daemon
    Daemon,
}

impl CommandLine {
    fn handle_args(self, runtime: &runtime::Runtime) -> Result<()> {
        let ctx = runtime.block_on(async {
            Context::builder()
                .with_runtime()
                .await?
                .with_meter_provider()
                .await?
                .with_pool()
                .await?
                .with_storage()
                .await?
                .with_maybe_cdn()?
                .with_build_queue()?
                .with_registry_api()
                .await?
                .with_std_replacements()
                .await?
                .with_rustsec()
                .await?
                .with_repository_stats()?
                .with_build_limits()?
                .build()
        })?;

        match self {
            Self::Daemon => start_daemon(ctx)?,
        }

        Ok(())
    }
}
