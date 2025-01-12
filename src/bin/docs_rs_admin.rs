use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use docs_rs::{
    context::BinContext,
    db::{self, add_path_into_database, CrateId, Overrides},
    utils::{
        get_crate_pattern_and_priority, list_crate_priorities, remove_crate_priority,
        set_crate_priority,
    },
    Context as _,
};
use futures_util::StreamExt;
use humantime::Duration;
use std::{env, path::PathBuf};

fn main() -> Result<()> {
    let _sentry_guard = docs_rs::logging::initialize_logging();
    CommandLine::parse()
        .handle_args()
        .context("error running command")?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    about = env!("CARGO_PKG_DESCRIPTION"),
    version = docs_rs::BUILD_VERSION,
    rename_all = "kebab-case",
)]
enum CommandLine {
    /// Starts the daemon
    /// Database operations
    Database {
        #[command(subcommand)]
        subcommand: DatabaseSubcommand,
    },

    /// Interactions with the build queue
    Queue {
        #[command(subcommand)]
        subcommand: QueueSubcommand,
    },
}

impl CommandLine {
    fn handle_args(self) -> Result<()> {
        let ctx = BinContext::new();

        match self {
            Self::Database { subcommand } => subcommand.handle_args(ctx)?,
            Self::Queue { subcommand } => subcommand.handle_args(ctx)?,
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum QueueSubcommand {
    /// Add a crate to the build queue
    Add {
        /// Name of crate to build
        #[arg(name = "CRATE_NAME")]
        crate_name: String,
        /// Version of crate to build
        #[arg(name = "CRATE_VERSION")]
        crate_version: String,
        /// Priority of build (new crate builds get priority 0)
        #[arg(
            name = "BUILD_PRIORITY",
            short = 'p',
            long = "priority",
            default_value = "5",
            allow_negative_numbers = true
        )]
        build_priority: i32,
    },

    /// Interactions with build queue priorities
    DefaultPriority {
        #[command(subcommand)]
        subcommand: PrioritySubcommand,
    },

    /// Get the registry watcher's last seen reference
    GetLastSeenReference,

    /// Set the registry watcher's last seen reference
    #[command(arg_required_else_help(true))]
    SetLastSeenReference {
        /// The reference to set to, required unless flag used
        #[arg(conflicts_with("head"))]
        reference: Option<crates_index_diff::gix::ObjectId>,

        /// Fetch the current HEAD of the remote index and use it
        #[arg(long, conflicts_with("reference"))]
        head: bool,
    },
}

impl QueueSubcommand {
    fn handle_args(self, ctx: BinContext) -> Result<()> {
        let build_queue = ctx.build_queue()?;
        match self {
            Self::Add {
                crate_name,
                crate_version,
                build_priority,
            } => build_queue.add_crate(
                &crate_name,
                &crate_version,
                build_priority,
                ctx.config()?.registry_url.as_deref(),
            )?,

            Self::GetLastSeenReference => {
                if let Some(reference) = build_queue.last_seen_reference()? {
                    println!("Last seen reference: {reference}");
                } else {
                    println!("No last seen reference available");
                }
            }

            Self::SetLastSeenReference { reference, head } => {
                let reference = match (reference, head) {
                    (Some(reference), false) => reference,
                    (None, true) => {
                        println!("Fetching changes to set reference to HEAD");
                        let (_, oid) = ctx.index()?.diff()?.peek_changes()?;
                        oid
                    }
                    (_, _) => unreachable!(),
                };

                build_queue.set_last_seen_reference(reference)?;
                println!("Set last seen reference: {reference}");
            }

            Self::DefaultPriority { subcommand } => subcommand.handle_args(ctx)?,
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum PrioritySubcommand {
    /// Get priority for a crate
    ///
    /// (returns only the first matching pattern, there may be other matching patterns)
    Get { crate_name: String },

    /// List priorities for all patterns
    List,

    /// Set all crates matching a pattern to a priority level
    Set {
        /// See https://www.postgresql.org/docs/current/functions-matching.html for pattern syntax
        #[arg(name = "PATTERN")]
        pattern: String,
        /// The priority to give crates matching the given `PATTERN`
        #[arg(allow_negative_numbers = true)]
        priority: i32,
    },

    /// Remove the prioritization of crates for a pattern
    Remove {
        /// See https://www.postgresql.org/docs/current/functions-matching.html for pattern syntax
        #[arg(name = "PATTERN")]
        pattern: String,
    },
}

impl PrioritySubcommand {
    fn handle_args(self, ctx: BinContext) -> Result<()> {
        ctx.runtime()?.block_on(async move {
            let mut conn = ctx.pool()?.get_async().await?;
            match self {
                Self::List => {
                    for (pattern, priority) in list_crate_priorities(&mut conn).await? {
                        println!("{pattern:>20} : {priority:>3}");
                    }
                }

                Self::Get { crate_name } => {
                    if let Some((pattern, priority)) =
                        get_crate_pattern_and_priority(&mut conn, &crate_name).await?
                    {
                        println!("{pattern} : {priority}");
                    } else {
                        println!("No priority found for {crate_name}");
                    }
                }

                Self::Set { pattern, priority } => {
                    set_crate_priority(&mut conn, &pattern, priority)
                        .await
                        .context("Could not set pattern's priority")?;
                    println!("Set pattern '{pattern}' to priority {priority}");
                }

                Self::Remove { pattern } => {
                    if let Some(priority) = remove_crate_priority(&mut conn, &pattern)
                        .await
                        .context("Could not remove pattern's priority")?
                    {
                        println!("Removed pattern '{pattern}' with priority {priority}");
                    } else {
                        println!("Pattern '{pattern}' did not exist and so was not removed");
                    }
                }
            }
            Ok(())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum DatabaseSubcommand {
    /// Run database migration
    Migrate {
        /// The database version to migrate to
        #[arg(name = "VERSION")]
        version: Option<i64>,
    },

    /// temporary command to update the `crates.latest_version_id` field
    UpdateLatestVersionId,

    /// Updates GitHub/GitLab stats for crates.
    UpdateRepositoryFields,

    /// Backfill GitHub/GitLab stats for crates.
    BackfillRepositoryStats,

    /// Updates info for a crate from the registry's API
    UpdateCrateRegistryFields {
        #[arg(name = "CRATE")]
        name: String,
    },

    AddDirectory {
        /// Path of file or directory
        #[arg(name = "DIRECTORY")]
        directory: PathBuf,
    },

    /// Remove documentation from the database
    Delete {
        #[command(subcommand)]
        command: DeleteSubcommand,
    },

    /// Blacklist operations
    Blacklist {
        #[command(subcommand)]
        command: BlacklistSubcommand,
    },

    /// Limit overrides operations
    Limits {
        #[command(subcommand)]
        command: LimitsSubcommand,
    },

    /// Compares the database with the index and resolves inconsistencies
    Synchronize {
        /// Don't actually resolve the inconsistencies, just log them
        #[arg(long)]
        dry_run: bool,
    },
}

impl DatabaseSubcommand {
    fn handle_args(self, ctx: BinContext) -> Result<()> {
        match self {
            Self::Migrate { version } => {
                let pool = ctx.pool()?;
                ctx.runtime()?
                    .block_on(async {
                        let mut conn = pool.get_async().await?;
                        db::migrate(&mut conn, version).await
                    })
                    .context("Failed to run database migrations")?
            }

            Self::UpdateLatestVersionId => {
                let pool = ctx.pool()?;
                ctx.runtime()?
                    .block_on(async {
                        let mut list_conn = pool.get_async().await?;
                        let mut update_conn = pool.get_async().await?;

                        let mut result_stream = sqlx::query!(
                            r#"SELECT id as "id: CrateId", name FROM crates ORDER BY name"#
                        )
                        .fetch(&mut *list_conn);

                        while let Some(row) = result_stream.next().await {
                            let row = row?;

                            println!("handling crate {}", row.name);

                            db::update_latest_version_id(&mut update_conn, row.id).await?;
                        }

                        Ok::<(), anyhow::Error>(())
                    })
                    .context("Failed to update latest version id")?
            }

            Self::UpdateRepositoryFields => {
                ctx.runtime()?
                    .block_on(ctx.repository_stats_updater()?.update_all_crates())?;
            }

            Self::BackfillRepositoryStats => {
                ctx.runtime()?
                    .block_on(ctx.repository_stats_updater()?.backfill_repositories())?;
            }

            Self::UpdateCrateRegistryFields { name } => ctx.runtime()?.block_on(async move {
                let mut conn = ctx.pool()?.get_async().await?;
                let registry_data = ctx.registry_api()?.get_crate_data(&name).await?;
                db::update_crate_data_in_database(&mut conn, &name, &registry_data).await
            })?,

            Self::AddDirectory { directory } => {
                ctx.runtime()?
                    .block_on(async {
                        let storage = ctx.async_storage().await?;

                        add_path_into_database(&storage, &ctx.config()?.prefix, directory).await
                    })
                    .context("Failed to add directory into database")?;
            }

            Self::Delete {
                command: DeleteSubcommand::Version { name, version },
            } => ctx
                .runtime()?
                .block_on(async move {
                    let mut conn = ctx.pool()?.get_async().await?;
                    db::delete_version(
                        &mut conn,
                        &*ctx.async_storage().await?,
                        &*ctx.config()?,
                        &name,
                        &version,
                    )
                    .await
                })
                .context("failed to delete the version")?,
            Self::Delete {
                command: DeleteSubcommand::Crate { name },
            } => ctx
                .runtime()?
                .block_on(async move {
                    let mut conn = ctx.pool()?.get_async().await?;
                    db::delete_crate(
                        &mut conn,
                        &*ctx.async_storage().await?,
                        &*ctx.config()?,
                        &name,
                    )
                    .await
                })
                .context("failed to delete the crate")?,
            Self::Blacklist { command } => command.handle_args(ctx)?,

            Self::Limits { command } => command.handle_args(ctx)?,

            Self::Synchronize { dry_run } => {
                docs_rs::utils::consistency::run_check(&ctx, dry_run)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum LimitsSubcommand {
    /// Get sandbox limit overrides for a crate
    Get { crate_name: String },

    /// List sandbox limit overrides for all crates
    List,

    /// Set sandbox limits overrides for a crate
    Set {
        crate_name: String,
        #[arg(long)]
        memory: Option<usize>,
        #[arg(long)]
        targets: Option<usize>,
        #[arg(long)]
        timeout: Option<Duration>,
    },

    /// Remove sandbox limits overrides for a crate
    Remove { crate_name: String },
}

impl LimitsSubcommand {
    fn handle_args(self, ctx: BinContext) -> Result<()> {
        let pool = ctx.pool()?;
        ctx.runtime()?.block_on(async move {
            let mut conn = pool.get_async().await?;

            match self {
                Self::Get { crate_name } => {
                    let overrides = Overrides::for_crate(&mut conn, &crate_name).await?;
                    println!("sandbox limit overrides for {crate_name} = {overrides:?}");
                }

                Self::List => {
                    for (crate_name, overrides) in Overrides::all(&mut conn).await? {
                        println!("sandbox limit overrides for {crate_name} = {overrides:?}");
                    }
                }

                Self::Set {
                    crate_name,
                    memory,
                    targets,
                    timeout,
                } => {
                    let overrides = Overrides::for_crate(&mut conn, &crate_name).await?;
                    println!("previous sandbox limit overrides for {crate_name} = {overrides:?}");
                    let overrides = Overrides {
                        memory,
                        targets,
                        timeout: timeout.map(Into::into),
                    };
                    Overrides::save(&mut conn, &crate_name, overrides).await?;
                    let overrides = Overrides::for_crate(&mut conn, &crate_name).await?;
                    println!("new sandbox limit overrides for {crate_name} = {overrides:?}");
                }

                Self::Remove { crate_name } => {
                    let overrides = Overrides::for_crate(&mut conn, &crate_name).await?;
                    println!("previous overrides for {crate_name} = {overrides:?}");
                    Overrides::remove(&mut conn, &crate_name).await?;
                }
            }
            Ok(())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum BlacklistSubcommand {
    /// List all crates on the blacklist
    List,

    /// Add a crate to the blacklist
    Add {
        /// Crate name
        #[arg(name = "CRATE_NAME")]
        crate_name: String,
    },

    /// Remove a crate from the blacklist
    Remove {
        /// Crate name
        #[arg(name = "CRATE_NAME")]
        crate_name: String,
    },
}

impl BlacklistSubcommand {
    fn handle_args(self, ctx: BinContext) -> Result<()> {
        ctx.runtime()?.block_on(async {
            let conn = &mut *ctx.pool()?.get_async().await?;
            match self {
                Self::List => {
                    let crates = db::blacklist::list_crates(conn)
                        .await
                        .context("failed to list crates on blacklist")?;

                    println!("{}", crates.join("\n"));
                }

                Self::Add { crate_name } => db::blacklist::add_crate(conn, &crate_name)
                    .await
                    .context("failed to add crate to blacklist")?,

                Self::Remove { crate_name } => db::blacklist::remove_crate(conn, &crate_name)
                    .await
                    .context("failed to remove crate from blacklist")?,
            }
            Ok(())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum DeleteSubcommand {
    /// Delete a whole crate
    Crate {
        /// Name of the crate to delete
        #[arg(name = "CRATE_NAME")]
        name: String,
    },
    /// Delete a single version of a crate (which may include multiple builds)
    Version {
        /// Name of the crate to delete
        #[arg(name = "CRATE_NAME")]
        name: String,

        /// The version of the crate to delete
        #[arg(name = "VERSION")]
        version: String,
    },
}
