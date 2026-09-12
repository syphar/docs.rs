use anyhow::Context as _;
use docs_rs_config::AppConfig as _;
use docs_rs_database::{Config, testing::prepare_template_db};
use std::{env, fs::OpenOptions, io::Write as _};

/// Prepares the shared test schema once for a nextest run, then publishes the
/// captured DDL location to every test process.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_environment().context("missing database config in env")?;
    let path = prepare_template_db(&config.database_url).await?;

    let env_file = env::var("NEXTEST_ENV")
        .context("NEXTEST_ENV is not set (this binary must be run by nextest)")?;
    let mut file = OpenOptions::new().append(true).open(env_file)?;
    writeln!(file, "DOCSRS_TEST_DATABASE_DDL_PATH={}", path.display())?;
    Ok(())
}
