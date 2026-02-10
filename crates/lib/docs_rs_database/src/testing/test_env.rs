use super::template::TemplateDatabase;
use crate::{AsyncPoolClient, Config, Pool, migrations};
use anyhow::{Context as _, Result};
use docs_rs_opentelemetry::AnyMeterProvider;
use futures_util::TryStreamExt as _;
use sqlx::Connection as _;
use std::time::Instant;
use tokio::{runtime, task::block_in_place};
use tracing::{debug, error, info};
use url::Url;

#[derive(Debug)]
pub struct TestDatabase {
    pool: Option<Pool>,
    name: String,
    database_url: Url,
    runtime: runtime::Handle,
}

impl TestDatabase {
    pub async fn new(config: &Config, otel_meter_provider: &AnyMeterProvider) -> Result<Self> {
        let started_at = Instant::now();
        let template = TemplateDatabase::instance(config).await?;

        info!(
            elapsed_ms = started_at.elapsed().as_millis(),
            "create or fetch template"
        );

        let started_at = Instant::now();
        let name = format!("docs_rs_test_db_{}", rand::random::<u64>());
        {
            let mut conn = sqlx::PgConnection::connect(&config.database_url).await?;
            sqlx::query(&format!(
                "CREATE DATABASE {name} TEMPLATE {}",
                &template.template_name
            ))
            .execute(&mut conn)
            .await?;
        }

        let mut database_url = template.base_url.clone();
        database_url.set_path(&format!("/{}", name));

        // FIXME: can I get around changing the config object here?
        // Perhaps geneerating the test-db name before we come here?
        let config = Config {
            database_url: database_url.to_string(),
            min_pool_idle: config.min_pool_idle,
            max_pool_size: config.max_pool_size,
        };

        dbg!(&config);

        let pool = Pool::new(&config, otel_meter_provider).await?;

        info!(
            name,
            elapsed_ms = started_at.elapsed().as_millis(),
            "created test database from template"
        );

        Ok(TestDatabase {
            pool: Some(pool),
            database_url,
            name,
            runtime: runtime::Handle::current(),
        })
    }

    pub fn pool(&self) -> &Pool {
        self.pool.as_ref().expect("pool should exist")
    }

    pub async fn async_conn(&self) -> Result<AsyncPoolClient> {
        self.pool().get_async().await.map_err(Into::into)
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        self.pool.take();
        let name = self.name.clone();
        let runtime = self.runtime.clone();
        let mut database_url = self.database_url.clone();

        block_in_place(move || {
            runtime.block_on(async move {
                // remove the database (path) from the URL,
                // so connect to the plain server.
                database_url.set_path("/");

                let mut conn = sqlx::PgConnection::connect(database_url.as_str())
                    .await
                    .unwrap();

                let started_at = Instant::now();

                if let Err(e) =
                    sqlx::query(format!("DROP DATABASE {} WITH (FORCE);", name).as_str())
                        .execute(&mut conn)
                        .await
                {
                    error!("failed to drop test database {}: {}", name, e);
                    return;
                }

                info!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "deleted test database"
                );
            })
        });
    }
}
