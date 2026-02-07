use std::sync::Arc;

use anyhow::Result;
use docs_rs_config::AppConfig as _;
use docs_rs_database::Pool;
use docs_rs_opentelemetry::testing::TestMetrics;
use docs_rs_repository_stats::RepositoryStatsUpdater;

#[tokio::main]
async fn main() -> Result<()> {
    let test_metrics = TestMetrics::new();
    let db_config = Arc::new(docs_rs_database::Config::from_environment()?);
    let pool = Pool::new(&db_config, test_metrics.provider()).await?;

    let updater = RepositoryStatsUpdater::from_environment(pool)?;
    let result = updater.update_all_crates().await?;

    dbg!(&result);

    Ok(())
}
