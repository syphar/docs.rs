use crate::{Config as WebConfig, handlers::build_axum_app, page::TemplateData};
use anyhow::Result;
use axum::Router;
use docs_rs_build_queue::AsyncBuildQueue;
use docs_rs_context::Context;
use docs_rs_database::{AsyncPoolClient, Config as DatabaseConfig, testing::TestDatabase};
use docs_rs_opentelemetry::testing::{CollectedMetrics, TestMetrics};
use docs_rs_storage::{AsyncStorage, Config as StorageConfig, StorageKind, testing::TestStorage};
use docs_rs_test_fakes::FakeRelease;
use std::sync::Arc;

pub(crate) struct TestEnvironment {
    pub(crate) context: Context,
    pub(crate) config: Arc<WebConfig>,
    #[allow(dead_code)] // so we can allow asserting collected metrics later.
    pub(crate) metrics: TestMetrics,
    #[allow(dead_code)] // we need to keep the storage so it can be cleaned up.
    pub(crate) storage: TestStorage,
    #[allow(dead_code)] // we need to keep the storage so it can be cleaned up.
    pub(crate) db: TestDatabase,
}

impl TestEnvironment {
    pub(crate) async fn new() -> Result<Self> {
        Self::with_config(WebConfig::test_config()?).await
    }

    pub(crate) async fn with_config(config: WebConfig) -> Result<Self> {
        docs_rs_logging::testing::init();

        let metrics = TestMetrics::new();

        let db_config = DatabaseConfig::test_config()?;
        let db = TestDatabase::new(&db_config, metrics.provider()).await?;

        let storage_config = Arc::new(StorageConfig::test_config(StorageKind::Memory)?);
        let test_storage =
            TestStorage::from_config(storage_config.clone(), metrics.provider()).await?;

        Ok(Self {
            config: Arc::new(config),
            context: Context::builder()
                .await?
                .pool(db_config.into(), db.pool().clone())
                .storage(storage_config.clone(), test_storage.storage())
                .with_build_queue()
                .await?
                .with_registry_api()
                .await?
                .build()?,
            db,
            storage: test_storage,
            metrics,
        })
    }

    pub(crate) fn config(&self) -> &WebConfig {
        &self.config
    }

    pub(crate) fn build_queue(&self) -> Result<&Arc<AsyncBuildQueue>> {
        self.context.build_queue()
    }

    pub(crate) async fn async_conn(&self) -> Result<AsyncPoolClient> {
        self.context.pool()?.get_async().await.map_err(Into::into)
    }

    pub(crate) fn storage(&self) -> Result<&Arc<AsyncStorage>> {
        self.context.storage()
    }

    pub(crate) async fn web_app(&self) -> Router {
        let template_data = Arc::new(TemplateData::new(1).unwrap());
        build_axum_app(self.config.clone(), &self.context, template_data)
            .await
            .expect("could not build axum app")
    }

    pub async fn fake_release(&self) -> FakeRelease<'_> {
        FakeRelease::new(
            self.context.pool().unwrap().clone(),
            self.context.storage().unwrap().clone(),
        )
    }

    pub fn collected_metrics(&self) -> CollectedMetrics {
        self.metrics.collected_metrics()
    }
}
