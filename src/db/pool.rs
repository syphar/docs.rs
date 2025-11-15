use crate::Config;
use crate::metrics::InstanceMetrics;
use futures_util::{future::BoxFuture, stream::BoxStream};
use opentelemetry::metrics::{self, Counter, ObservableGauge};
use sqlx::{Executor, postgres::PgPoolOptions};
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};
use tokio::runtime;
use tracing::debug;

const DEFAULT_SCHEMA: &str = "public";

#[derive(Debug)]
struct PoolMetrics {
    failed_connections: Counter<u64>,
    idle_connections: ObservableGauge<u64>,
    used_connections: ObservableGauge<u64>,
    max_connections: ObservableGauge<u64>,
}

impl PoolMetrics {
    fn new(pool: sqlx::PgPool, meter: &metrics::Meter) -> Self {
        const PREFIX: &str = "docsrs.db.pool";
        // FIXME: semantic names?
        // https://github.com/open-telemetry/semantic-conventions/blob/main/docs/database/database-metrics.md#metric-dbclientconnectioncount
        Self {
            failed_connections: meter
                .u64_counter(format!("{PREFIX}.failed_connections"))
                .build(),
            idle_connections: meter
                .u64_observable_gauge(format!("{PREFIX}.idle_connections"))
                .with_callback({
                    let pool = pool.clone();
                    move |observer| {
                        observer.observe(pool.num_idle() as u64, &[]);
                    }
                })
                .build(),
            used_connections: meter
                .u64_observable_gauge(format!("{PREFIX}.used_connections"))
                .with_callback({
                    let pool = pool.clone();
                    move |observer| {
                        let used = pool.size() as u64 - pool.num_idle() as u64;
                        observer.observe(used, &[]);
                    }
                })
                .build(),
            max_connections: meter
                .u64_observable_gauge(format!("{PREFIX}.max_connections"))
                .with_callback({
                    let pool = pool.clone();
                    move |observer| {
                        observer.observe(pool.size() as u64, &[]);
                    }
                })
                .build(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pool {
    async_pool: sqlx::PgPool,
    runtime: runtime::Handle,
    metrics: Arc<InstanceMetrics>,
    max_size: u32,
    otel_metrics: Arc<PoolMetrics>,
}

impl Pool {
    pub async fn new(
        config: &Config,
        metrics: Arc<InstanceMetrics>,
        otel_meter: &metrics::Meter,
    ) -> Result<Pool, PoolError> {
        debug!(
            "creating database pool (if this hangs, consider running `docker-compose up -d db s3`)"
        );
        Self::new_inner(config, metrics, DEFAULT_SCHEMA, otel_meter).await
    }

    #[cfg(test)]
    pub(crate) async fn new_with_schema(
        config: &Config,
        metrics: Arc<InstanceMetrics>,
        schema: &str,
        otel_meter: &metrics::Meter,
    ) -> Result<Pool, PoolError> {
        Self::new_inner(config, metrics, schema, otel_meter).await
    }

    async fn new_inner(
        config: &Config,
        metrics: Arc<InstanceMetrics>,
        schema: &str,
        otel_meter: &metrics::Meter,
    ) -> Result<Pool, PoolError> {
        let acquire_timeout = Duration::from_secs(30);
        let max_lifetime = Duration::from_secs(30 * 60);
        let idle_timeout = Duration::from_secs(10 * 60);

        let async_pool = PgPoolOptions::new()
            .max_connections(config.max_pool_size)
            .min_connections(config.min_pool_idle)
            .max_lifetime(max_lifetime)
            .acquire_timeout(acquire_timeout)
            .idle_timeout(idle_timeout)
            .after_connect({
                let schema = schema.to_owned();
                move |conn, _meta| {
                    Box::pin({
                        let schema = schema.clone();

                        async move {
                            if schema != DEFAULT_SCHEMA {
                                conn.execute(
                                    format!("SET search_path TO {schema}, {DEFAULT_SCHEMA};")
                                        .as_str(),
                                )
                                .await?;
                            }

                            Ok(())
                        }
                    })
                }
            })
            .connect_lazy(&config.database_url)
            .map_err(PoolError::AsyncPoolCreationFailed)?;

        Ok(Pool {
            async_pool: async_pool.clone(),
            metrics,
            runtime: runtime::Handle::current(),
            max_size: config.max_pool_size,
            otel_metrics: Arc::new(PoolMetrics::new(async_pool, otel_meter)),
        })
    }

    pub async fn get_async(&self) -> Result<AsyncPoolClient, PoolError> {
        match self.async_pool.acquire().await {
            Ok(conn) => Ok(AsyncPoolClient {
                inner: Some(conn),
                runtime: self.runtime.clone(),
            }),
            Err(err) => {
                self.metrics.failed_db_connections.inc();
                self.otel_metrics.failed_connections.add(1, &[]);
                Err(PoolError::AsyncClientError(err))
            }
        }
    }

    pub(crate) fn used_connections(&self) -> u32 {
        self.async_pool.size() - self.async_pool.num_idle() as u32
    }

    pub(crate) fn idle_connections(&self) -> u32 {
        self.async_pool.num_idle() as u32
    }

    pub(crate) fn max_size(&self) -> u32 {
        self.max_size
    }
}

/// This impl allows us to use our own pool as an executor for SQLx queries.
impl sqlx::Executor<'_> for &'_ Pool
where
    for<'c> &'c mut <sqlx::Postgres as sqlx::Database>::Connection:
        sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    type Database = sqlx::Postgres;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxStream<
        'e,
        Result<
            sqlx::Either<
                <sqlx::Postgres as sqlx::Database>::QueryResult,
                <sqlx::Postgres as sqlx::Database>::Row,
            >,
            sqlx::Error,
        >,
    >
    where
        E: sqlx::Execute<'q, Self::Database> + 'q,
    {
        self.async_pool.fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<<sqlx::Postgres as sqlx::Database>::Row>, sqlx::Error>>
    where
        E: sqlx::Execute<'q, Self::Database> + 'q,
    {
        self.async_pool.fetch_optional(query)
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [<Self::Database as sqlx::Database>::TypeInfo],
    ) -> BoxFuture<'e, Result<<Self::Database as sqlx::Database>::Statement<'q>, sqlx::Error>> {
        self.async_pool.prepare_with(sql, parameters)
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> BoxFuture<'e, Result<sqlx::Describe<Self::Database>, sqlx::Error>> {
        self.async_pool.describe(sql)
    }
}

/// we wrap `sqlx::PoolConnection` so we can drop it in a sync context
/// and enter the runtime.
/// Otherwise dropping the PoolConnection will panic because it can't spawn a task.
#[derive(Debug)]
pub struct AsyncPoolClient {
    inner: Option<sqlx::pool::PoolConnection<sqlx::postgres::Postgres>>,
    runtime: runtime::Handle,
}

impl Deref for AsyncPoolClient {
    type Target = sqlx::PgConnection;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl DerefMut for AsyncPoolClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl Drop for AsyncPoolClient {
    fn drop(&mut self) {
        let _guard = self.runtime.enter();
        drop(self.inner.take())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("failed to create the database connection pool")]
    AsyncPoolCreationFailed(#[source] sqlx::Error),

    #[error("failed to get a database connection")]
    AsyncClientError(#[source] sqlx::Error),
}
