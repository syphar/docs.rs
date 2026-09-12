use crate::{AsyncPoolClient, Config, Pool, migrations};
use anyhow::{Context as _, Result};
use docs_rs_opentelemetry::AnyMeterProvider;
use sqlx::{AssertSqlSafe, Connection as _};
use std::{env, fs, io::Write as _, path::PathBuf, process::Command};
use tempfile::NamedTempFile;
use tokio::{runtime, sync::OnceCell, task::block_in_place};
use tracing::{debug, error, warn};

const TEST_SCHEMA_PREFIX: &str = "docs_rs_test_schema_";
const TEMPLATE_SCHEMA: &str = "docs_rs_test_template";
pub const TEMPLATE_DDL_ENV: &str = "DOCSRS_TEST_DATABASE_DDL_PATH";

static TEMPLATE_DDL: OnceCell<String> = OnceCell::const_new();

/// An isolated test schema cloned from a shared, fully migrated template.
///
/// The template is prepared once, then each test replays its schema-only DDL
/// into a fresh schema that is dropped when this value is dropped.
#[derive(Debug)]
pub struct TestDatabase {
    pool: Pool,
    schema: String,
    runtime: runtime::Handle,
}

impl TestDatabase {
    pub async fn new(config: &Config, otel_meter_provider: &AnyMeterProvider) -> Result<Self> {
        let template_ddl = template_ddl(&config.database_url).await?;
        let schema = format!("{TEST_SCHEMA_PREFIX}{}", rand::random::<u64>());

        let mut conn = sqlx::PgConnection::connect(&config.database_url).await?;

        // run the prepared DDL to fill the database schema into the new schema.
        //
        // The DDL is produced by pg_dump from a schema we own. The only substitution is a
        // generated schema name, so it is safe to send as raw SQL.
        sqlx::raw_sql(AssertSqlSafe(
            template_ddl.replace(TEMPLATE_SCHEMA, &schema),
        ))
        .execute(&mut conn)
        .await
        .context("error cloning test database schema")?;

        let pool = Pool::new_with_schema(config, &schema, otel_meter_provider).await?;

        Ok(TestDatabase {
            pool,
            schema,
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
        let schema = self.schema.clone();
        let runtime = self.runtime.clone();

        block_in_place(move || {
            runtime.block_on(async move {
                let Ok(mut conn) = pool.get_async().await else {
                    error!("error in drop impl");
                    return;
                };

                // NOTE: we run all reverse-migrations after the tests.
                // With that we ensure that even with data, the rollback will work.
                // This only costs little performance at the moment, we could make
                // this optional at some point.
                let migration_error = migrations::migrate(&mut conn, Some(0)).await.err();

                if let Err(e) = sqlx::query(AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE;")))
                    .execute(&mut *conn)
                    .await
                {
                    panic!("failed to drop test schema {schema}: {e}");
                }

                if let Some(err) = migration_error {
                    panic!("failed to revert migrations for test schema {schema}: {err:?}");
                }
            })
        });
    }
}

/// Creates or updates the migrated template schema, dumps its DDL, and keeps
/// that dump in a persistent temporary file. The nextest setup script exposes
/// this path to every test process, avoiding one migration run per process.
pub async fn prepare_template_db(database_url: &str) -> Result<PathBuf> {
    let template_ddl = prepare_template_ddl(database_url).await?;

    let mut file = NamedTempFile::new().context("error creating template DDL file")?;
    file.write_all(template_ddl.as_bytes())
        .context("error writing template DDL file")?;

    let (_, path) = file.keep().context("error preserving template DDL file")?;
    Ok(path)
}

async fn template_ddl(database_url: &str) -> Result<&'static String> {
    TEMPLATE_DDL
        .get_or_try_init(|| async {
            if let Some(path) = env::var_os(TEMPLATE_DDL_ENV) {
                return fs::read_to_string(path).context("error reading template DDL file");
            }

            warn!("fall back to generating template DDL ourselves, prepare went wrong?");
            prepare_template_ddl(database_url).await
        })
        .await
}

async fn prepare_template_ddl(database_url: &str) -> Result<String> {
    let mut conn = sqlx::PgConnection::connect(database_url).await?;

    // Cargo test can start several test binaries at once. Serializing this work keeps them from
    // racing while applying migrations to the one shared template schema.
    sqlx::query("SELECT pg_advisory_lock(hashtext($1))")
        .bind(TEMPLATE_SCHEMA)
        .execute(&mut conn)
        .await?;

    let result = async {
        cleanup_leftover_schemas(&mut conn).await?;

        sqlx::query(AssertSqlSafe(format!(
            "CREATE SCHEMA IF NOT EXISTS {TEMPLATE_SCHEMA}"
        )))
        .execute(&mut conn)
        .await?;

        sqlx::query(AssertSqlSafe(format!(
            "SET search_path TO {TEMPLATE_SCHEMA}, public"
        )))
        .execute(&mut conn)
        .await?;

        migrations::migrate(&mut conn, None).await?;

        dump_schema(database_url)
    }
    .await;

    sqlx::query("SELECT pg_advisory_unlock(hashtext($1))")
        .bind(TEMPLATE_SCHEMA)
        .execute(&mut conn)
        .await?;

    result
}

async fn cleanup_leftover_schemas(conn: &mut sqlx::PgConnection) -> Result<()> {
    let schemas: Vec<String> = sqlx::query_scalar(
        "SELECT schema_name FROM information_schema.schemata \
         WHERE schema_name ~ '^docs_rs_test_schema_[0-9]+$'",
    )
    .fetch_all(&mut *conn)
    .await?;

    for schema in schemas {
        debug!(%schema, "dropping leftover test schema");
        sqlx::query(AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

fn dump_schema(database_url: &str) -> Result<String> {
    let output = Command::new("pg_dump")
        .args(["--schema-only", "--no-owner", "--schema", TEMPLATE_SCHEMA])
        .arg(database_url)
        .output()
        .context("error running pg_dump for test template")?;

    if !output.status.success() {
        anyhow::bail!(
            "pg_dump for test template failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let ddl = String::from_utf8(output.stdout).context("pg_dump output was not UTF-8")?;
    // PostgreSQL 17+ emits psql-only \restrict directives. SQLx sends DDL directly to
    // PostgreSQL, where those directives are invalid SQL.
    Ok(ddl
        .lines()
        .filter(|line| !line.starts_with("\\restrict ") && !line.starts_with("\\unrestrict "))
        .collect::<Vec<_>>()
        .join("\n"))
}
