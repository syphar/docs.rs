use crate::{AsyncPoolClient, Config, Pool, migrations};
use anyhow::{Context as _, Result};
use docs_rs_opentelemetry::AnyMeterProvider;
use futures_util::TryStreamExt as _;
use sqlx::{AssertSqlSafe, Connection as _};
use tokio::{runtime, sync::OnceCell, task::block_in_place};
use tracing::error;
use url::Url;

const TEMPLATE_DATABASE: &str = "docs_rs_test_template";
const TEMPLATE_LOCK: i64 = 0x646f6373_72735f74; // "docs_rs_t"

/// Initialization is also protected by a PostgreSQL advisory lock, because every test binary has
/// its own copy of this static.
static TEMPLATE_READY: OnceCell<()> = OnceCell::const_new();

#[derive(Debug)]
pub struct TestDatabase {
    pool: Pool,
    database: String,
    maintenance_url: String,
    runtime: runtime::Handle,
}

impl TestDatabase {
    pub async fn new(config: &Config, otel_meter_provider: &AnyMeterProvider) -> Result<Self> {
        let maintenance_url = database_url(&config.database_url, "postgres")?;
        TEMPLATE_READY
            .get_or_try_init(|| initialize_template(&maintenance_url, config))
            .await?;

        let database = format!("docs_rs_test_{}", rand::random::<u64>());
        let mut maintenance = sqlx::PgConnection::connect(&maintenance_url).await?;
        sqlx::query(AssertSqlSafe(format!(
            "CREATE DATABASE {database} TEMPLATE {TEMPLATE_DATABASE}"
        )))
        .execute(&mut maintenance)
        .await
        .context("error creating test database from template")?;

        let database_url = database_url(&config.database_url, &database)?;
        let test_config = Config {
            database_url: database_url.clone(),
            max_pool_size: config.max_pool_size,
            min_pool_idle: config.min_pool_idle,
        };
        let pool = Pool::new(&test_config, otel_meter_provider).await?;
        let mut conn = sqlx::PgConnection::connect(&database_url).await?;

        // Move all sequence start positions 10000 apart to avoid overlapping primary keys.
        let sequence_names: Vec<_> = sqlx::query!(
            "SELECT relname
             FROM pg_class
             INNER JOIN pg_namespace ON
                 pg_class.relnamespace = pg_namespace.oid
             WHERE pg_class.relkind = 'S'
                 AND pg_namespace.nspname = $1
            ",
            "public",
        )
        .fetch(&mut conn)
        .map_ok(|row| row.relname)
        .try_collect()
        .await?;

        for (i, sequence) in sequence_names.into_iter().enumerate() {
            let offset = (i + 1) * 10000;
            sqlx::query(AssertSqlSafe(format!(
                r#"ALTER SEQUENCE "{sequence}" RESTART WITH {offset};"#
            )))
            .execute(&mut conn)
            .await
            .context("error resetting test database sequences")?;
        }

        Ok(TestDatabase {
            pool,
            database,
            maintenance_url,
            runtime: runtime::Handle::current(),
        })
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub async fn async_conn(&self) -> Result<AsyncPoolClient> {
        self.pool.get_async().await.map_err(Into::into)
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let database = self.database.clone();
        let maintenance_url = self.maintenance_url.clone();
        let runtime = self.runtime.clone();

        block_in_place(move || {
            runtime.block_on(async move {
                let migration_result = match pool.get_async().await {
                    Ok(mut conn) => migrations::migrate(&mut conn, Some(0)).await,
                    Err(err) => {
                        error!(
                            ?err,
                            "error acquiring test database connection in drop impl"
                        );
                        return;
                    }
                };
                pool.close().await;

                let mut conn = match sqlx::PgConnection::connect(&maintenance_url).await {
                    Ok(conn) => conn,
                    Err(err) => {
                        error!(
                            ?err,
                            "error connecting to maintenance database in drop impl"
                        );
                        return;
                    }
                };
                if let Err(e) = sqlx::query(AssertSqlSafe(format!(
                    "DROP DATABASE {database} WITH (FORCE)"
                )))
                .execute(&mut conn)
                .await
                {
                    error!("failed to drop test database {}: {}", database, e);
                    return;
                }

                if let Err(err) = migration_result {
                    error!(?err, "error reverting migrations");
                }
            })
        });
    }
}

async fn initialize_template(maintenance_url: &str, config: &Config) -> Result<()> {
    let mut conn = sqlx::PgConnection::connect(maintenance_url).await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(TEMPLATE_LOCK)
        .execute(&mut conn)
        .await?;

    let result = async {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
        )
        .bind(TEMPLATE_DATABASE)
        .fetch_one(&mut conn)
        .await?;
        if !exists {
            sqlx::query(AssertSqlSafe(format!(
                "CREATE DATABASE {TEMPLATE_DATABASE}"
            )))
            .execute(&mut conn)
            .await
            .context("error creating test template database")?;
        }

        let template_url = database_url(&config.database_url, TEMPLATE_DATABASE)?;
        let mut template = sqlx::PgConnection::connect(&template_url).await?;
        migrations::migrate(&mut template, None)
            .await
            .context("error running migrations for test template database")?;
        Ok(())
    }
    .await;

    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(TEMPLATE_LOCK)
        .execute(&mut conn)
        .await;
    result
}

fn database_url(base_url: &str, database: &str) -> Result<String> {
    let mut url = Url::parse(base_url).context("invalid database URL")?;
    url.set_path(&format!("/{database}"));
    Ok(url.into())
}
