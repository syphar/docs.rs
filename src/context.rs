use crate::cdn::CdnBackend;
use crate::db::Pool;
use crate::repositories::RepositoryStatsUpdater;
use crate::utils::opentelemetry::NoopMeterProvider;
use crate::{
    AsyncBuildQueue, AsyncStorage, BuildQueue, Config, InstanceMetrics, RegistryApi,
    ServiceMetrics, Storage,
};
use anyhow::Result;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_otlp::{Protocol, WithExportConfig as _};
use opentelemetry_resource_detectors::{OsResourceDetector, ProcessResourceDetector};
use opentelemetry_sdk::Resource;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime;

// get_resource returns a Resource containing information about the environment
// The Resource is used to provide context to Traces, Metrics and Logs
// It is created by merging the results of multiple ResourceDetectors
// The ResourceDetectors are responsible for detecting information about the environment
fn get_resource() -> Resource {
    Resource::builder()
        .with_detector(Box::new(OsResourceDetector))
        .with_detector(Box::new(ProcessResourceDetector))
        .build()
}

/// opentelemetry metric provider setup,
/// if no endpoint is configured, use a no-op provider
fn get_metric_provider(config: &Config) -> Arc<dyn MeterProvider + Send + Sync> {
    if let Some(ref endpoint) = config.opentelemetry_endpoint {
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.to_string())
            .with_protocol(Protocol::Grpc)
            .with_timeout(Duration::from_secs(3))
            .build()?;

        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_periodic_exporter(exporter)
            .with_resource(get_resource())
            .build();

        Arc::new(provider)
    } else {
        Arc::new(NoopMeterProvider::new())
    }
}

pub struct Context {
    pub config: Arc<Config>,
    pub async_build_queue: Arc<AsyncBuildQueue>,
    pub build_queue: Arc<BuildQueue>,
    pub storage: Arc<Storage>,
    pub async_storage: Arc<AsyncStorage>,
    pub cdn: Arc<CdnBackend>,
    pub pool: Pool,
    pub service_metrics: Arc<ServiceMetrics>,
    pub instance_metrics: Arc<InstanceMetrics>,
    pub registry_api: Arc<RegistryApi>,
    pub repository_stats_updater: Arc<RepositoryStatsUpdater>,
    pub runtime: runtime::Handle,
    metric_provider: Arc<dyn MeterProvider + Send + Sync>,
}

impl Context {
    /// Create a new context environment from the given configuration.
    #[cfg(not(test))]
    pub async fn from_config(config: Config) -> Result<Self> {
        let instance_metrics = Arc::new(InstanceMetrics::new()?);
        let pool = Pool::new(&config, instance_metrics.clone()).await?;
        Self::from_config_with_metrics_and_pool(config, instance_metrics, pool).await
    }

    /// Create a new context environment from the given configuration, for running tests.
    #[cfg(test)]
    pub async fn from_config(
        config: Config,
        instance_metrics: Arc<InstanceMetrics>,
        pool: Pool,
    ) -> Result<Self> {
        Self::from_config_with_metrics_and_pool(config, instance_metrics, pool).await
    }

    /// private function for context environment generation, allows passing in a
    /// preconfigured instance metrics & pool from the database.
    /// Mostly so we can support test environments with their db
    async fn from_config_with_metrics_and_pool(
        config: Config,
        instance_metrics: Arc<InstanceMetrics>,
        pool: Pool,
    ) -> Result<Self> {
        let config = Arc::new(config);

        let metric_provider = get_metric_provider(&config);

        let async_storage = Arc::new(
            AsyncStorage::new(pool.clone(), instance_metrics.clone(), config.clone()).await?,
        );

        let async_build_queue = Arc::new(AsyncBuildQueue::new(
            pool.clone(),
            instance_metrics.clone(),
            config.clone(),
            async_storage.clone(),
            metric_provider.meter("storage"),
        ));

        let cdn = Arc::new(CdnBackend::new(&config).await);

        let runtime = runtime::Handle::current();

        // sync wrappers around build-queue & storage async resources
        let build_queue = Arc::new(BuildQueue::new(runtime.clone(), async_build_queue.clone()));
        let storage = Arc::new(Storage::new(async_storage.clone(), runtime.clone()));

        Ok(Self {
            async_build_queue,
            build_queue,
            storage,
            async_storage,
            cdn,
            pool: pool.clone(),
            service_metrics: Arc::new(ServiceMetrics::new()?),
            instance_metrics,
            registry_api: Arc::new(RegistryApi::new(
                config.registry_api_host.clone(),
                config.crates_io_api_call_retries,
            )?),
            repository_stats_updater: Arc::new(RepositoryStatsUpdater::new(&config, pool)),
            runtime,
            config,
            metric_provider,
        })
    }
}
