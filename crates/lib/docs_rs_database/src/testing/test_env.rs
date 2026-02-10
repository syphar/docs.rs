use super::template::TemplateDatabase;
use crate::{AsyncPoolClient, Config, Pool};
use anyhow::Result;
use docs_rs_opentelemetry::AnyMeterProvider;
use sqlx::Connection as _;
use std::time::Instant;
use tokio::{runtime, task::block_in_place};
use tracing::{error, info};
use url::Url;

#[derive(Debug)]
pub struct TestDatabase {
    pool: Option<Pool>,
    name: String,
    base_url: Url,
    runtime: runtime::Handle,
}

impl TestDatabase {
    pub async fn new(config: &Config, otel_meter_provider: &AnyMeterProvider) -> Result<Self> {
        let started_at = Instant::now();
        let template = TemplateDatabase::instance(config).await?;
        let name = config.database_name()?;

        info!(
            elapsed_ms = started_at.elapsed().as_millis(),
            "create or fetch template"
        );

        dbg!(&name);

        let started_at = Instant::now();
        {
            let mut conn = sqlx::PgConnection::connect(template.base_url.as_str()).await?;
            sqlx::query(&format!(
                "CREATE DATABASE {name} TEMPLATE {}",
                &template.template_name
            ))
            .execute(&mut conn)
            .await?;
        }

        let pool = Pool::new(&config, otel_meter_provider).await?;

        info!(
            name,
            elapsed_ms = started_at.elapsed().as_millis(),
            "created test database from template"
        );

        Ok(TestDatabase {
            pool: Some(pool),
            base_url: template.base_url.clone(),
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
        // drop the pool so we don't have any connections to the test database any more
        self.pool.take();

        let name = self.name.clone();
        let runtime = self.runtime.clone();
        let base_url = self.base_url.clone();

        block_in_place(move || {
            runtime.block_on(async move {
                let mut conn = sqlx::PgConnection::connect(base_url.as_str())
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
