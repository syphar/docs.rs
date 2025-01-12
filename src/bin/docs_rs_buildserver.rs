use anyhow::{anyhow, Context as _, Result};
use clap::{Parser, Subcommand};
use docs_rs::context::BinContext;
use docs_rs::utils::{get_config, queue_builder, set_config, ConfigName};
use docs_rs::{start_background_metrics_webserver, Context, PackageKind, RustwideBuilder};
use std::{env, net::SocketAddr, path::PathBuf};
use tracing_log::LogTracer;

fn main() -> Result<()> {
    // set the global log::logger for backwards compatibility
    // through rustwide.
    rustwide::logging::init_with(LogTracer::new());

    let _sentry_guard = docs_rs::logging::initialize_logging();
    CommandLine::parse()
        .handle_args()
        .context("error running command")?;

    Ok(())
}

#[derive(Parser, Debug)]
#[command(
    about = env!("CARGO_PKG_DESCRIPTION"),
    version = docs_rs::BUILD_VERSION,
    rename_all = "kebab-case",
)]
enum CommandLine {
    Build {
        #[command(subcommand)]
        subcommand: BuildSubcommand,
    },
    Start {
        #[arg(name = "SOCKET_ADDR", default_value = "0.0.0.0:3000")]
        metric_server_socket_addr: SocketAddr,
    },
}

impl CommandLine {
    fn handle_args(self) -> Result<()> {
        let ctx = BinContext::new();

        match self {
            Self::Build { subcommand } => subcommand.handle_args(ctx)?,
            Self::Start {
                metric_server_socket_addr,
            } => {
                start_background_metrics_webserver(Some(metric_server_socket_addr), &ctx)?;

                let build_queue = ctx.build_queue()?;
                let config = ctx.config()?;
                let rustwide_builder = RustwideBuilder::init(&ctx)?;
                queue_builder(&ctx, rustwide_builder, build_queue, config)?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum BuildSubcommand {
    /// Builds documentation for a crate
    Crate {
        /// Crate name
        #[arg(name = "CRATE_NAME", requires("CRATE_VERSION"))]
        crate_name: Option<String>,

        /// Version of crate
        #[arg(name = "CRATE_VERSION")]
        crate_version: Option<String>,

        /// Build a crate at a specific path
        #[arg(short = 'l', long = "local", conflicts_with_all(&["CRATE_NAME", "CRATE_VERSION"]))]
        local: Option<PathBuf>,
    },

    /// update the currently installed rustup toolchain
    UpdateToolchain {
        /// Update the toolchain only if no toolchain is currently installed
        #[arg(name = "ONLY_FIRST_TIME", long = "only-first-time")]
        only_first_time: bool,
    },

    /// Adds essential files for the installed version of rustc
    AddEssentialFiles,

    SetToolchain {
        toolchain_name: String,
    },

    /// Locks the daemon, preventing it from building new crates
    Lock,

    /// Unlocks the daemon to continue building new crates
    Unlock,
}

impl BuildSubcommand {
    fn handle_args(self, ctx: BinContext) -> Result<()> {
        let build_queue = ctx.build_queue()?;
        let rustwide_builder = || -> Result<RustwideBuilder> { RustwideBuilder::init(&ctx) };

        match self {
            Self::Crate {
                crate_name,
                crate_version,
                local,
            } => {
                let mut builder = rustwide_builder()?;

                if let Some(path) = local {
                    builder
                        .build_local_package(&path)
                        .context("Building documentation failed")?;
                } else {
                    let registry_url = ctx.config()?.registry_url.clone();
                    builder
                        .build_package(
                            &crate_name
                                .with_context(|| anyhow!("must specify name if not local"))?,
                            &crate_version
                                .with_context(|| anyhow!("must specify version if not local"))?,
                            registry_url
                                .as_ref()
                                .map(|s| PackageKind::Registry(s.as_str()))
                                .unwrap_or(PackageKind::CratesIo),
                        )
                        .context("Building documentation failed")?;
                }
            }

            Self::UpdateToolchain { only_first_time } => {
                let rustc_version = ctx.runtime()?.block_on({
                    let pool = ctx.pool()?;
                    async move {
                        let mut conn = pool
                            .get_async()
                            .await
                            .context("failed to get a database connection")?;

                        get_config::<String>(&mut conn, ConfigName::RustcVersion).await
                    }
                })?;
                if only_first_time && rustc_version.is_some() {
                    println!("update-toolchain was already called in the past, exiting");
                    return Ok(());
                }

                rustwide_builder()?
                    .update_toolchain()
                    .context("failed to update toolchain")?;

                rustwide_builder()?
                    .purge_caches()
                    .context("failed to purge caches")?;

                rustwide_builder()?
                    .add_essential_files()
                    .context("failed to add essential files")?;
            }

            Self::AddEssentialFiles => {
                rustwide_builder()?
                    .add_essential_files()
                    .context("failed to add essential files")?;
            }

            Self::SetToolchain { toolchain_name } => {
                ctx.runtime()?.block_on(async move {
                    let mut conn = ctx
                        .pool()?
                        .get_async()
                        .await
                        .context("failed to get a database connection")?;
                    set_config(&mut conn, ConfigName::Toolchain, toolchain_name)
                        .await
                        .context("failed to set toolchain in database")
                })?;
            }

            Self::Lock => build_queue.lock().context("Failed to lock")?,
            Self::Unlock => build_queue.unlock().context("Failed to unlock")?,
        }

        Ok(())
    }
}
