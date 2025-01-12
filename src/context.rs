use crate::cdn::CdnBackend;
use crate::db::Pool;
use crate::repositories::RepositoryStatsUpdater;
use crate::{
    AsyncBuildQueue, AsyncStorage, BuildQueue, Config, Index, InstanceMetrics, RegistryApi,
    ServiceMetrics, Storage,
};
use anyhow::{Error, Result};
use once_cell::sync::OnceCell;
use std::{future::Future, sync::Arc};
use tokio::runtime::{Builder, Runtime};

pub trait Context {
    fn config(&self) -> Result<Arc<Config>>;
    fn async_build_queue(&self) -> impl Future<Output = Result<Arc<AsyncBuildQueue>>> + Send;
    fn build_queue(&self) -> Result<Arc<BuildQueue>>;
    fn storage(&self) -> Result<Arc<Storage>>;
    fn async_storage(&self) -> impl Future<Output = Result<Arc<AsyncStorage>>> + Send;
    fn cdn(&self) -> impl Future<Output = Result<Arc<CdnBackend>>> + Send;
    fn pool(&self) -> Result<Pool>;
    fn async_pool(&self) -> impl Future<Output = Result<Pool>> + Send;
    fn service_metrics(&self) -> Result<Arc<ServiceMetrics>>;
    fn instance_metrics(&self) -> Result<Arc<InstanceMetrics>>;
    fn index(&self) -> Result<Arc<Index>>;
    fn registry_api(&self) -> Result<Arc<RegistryApi>>;
    fn repository_stats_updater(&self) -> Result<Arc<RepositoryStatsUpdater>>;
    fn runtime(&self) -> Result<Arc<Runtime>>;
}

pub struct BinContext {
    build_queue: OnceCell<Arc<BuildQueue>>,
    async_build_queue: tokio::sync::OnceCell<Arc<AsyncBuildQueue>>,
    storage: OnceCell<Arc<Storage>>,
    cdn: tokio::sync::OnceCell<Arc<CdnBackend>>,
    config: OnceCell<Arc<Config>>,
    pool: OnceCell<Pool>,
    service_metrics: OnceCell<Arc<ServiceMetrics>>,
    instance_metrics: OnceCell<Arc<InstanceMetrics>>,
    index: OnceCell<Arc<Index>>,
    registry_api: OnceCell<Arc<RegistryApi>>,
    repository_stats_updater: OnceCell<Arc<RepositoryStatsUpdater>>,
    runtime: OnceCell<Arc<Runtime>>,
}

impl BinContext {
    pub fn new() -> Self {
        Self {
            build_queue: OnceCell::new(),
            async_build_queue: tokio::sync::OnceCell::new(),
            storage: OnceCell::new(),
            cdn: tokio::sync::OnceCell::new(),
            config: OnceCell::new(),
            pool: OnceCell::new(),
            service_metrics: OnceCell::new(),
            instance_metrics: OnceCell::new(),
            index: OnceCell::new(),
            registry_api: OnceCell::new(),
            repository_stats_updater: OnceCell::new(),
            runtime: OnceCell::new(),
        }
    }
}

macro_rules! lazy {
    ( $(fn $name:ident($self:ident) -> $type:ty = $init:expr);+ $(;)? ) => {
        $(fn $name(&$self) -> Result<Arc<$type>> {
            Ok($self
                .$name
                .get_or_try_init::<_, Error>(|| Ok(Arc::new($init)))?
                .clone())
        })*
    }
}

impl Context for BinContext {
    lazy! {
        fn build_queue(self) -> BuildQueue = {
            let runtime = self.runtime()?;
            BuildQueue::new(
                runtime.clone(),
                runtime.block_on(self.async_build_queue())?
            )
        };
        fn storage(self) -> Storage = {
            let runtime = self.runtime()?;
            Storage::new(
                runtime.block_on(self.async_storage())?,
                runtime
           )
        };
        fn config(self) -> Config = Config::from_env()?;
        fn service_metrics(self) -> ServiceMetrics = {
            ServiceMetrics::new()?
        };
        fn instance_metrics(self) -> InstanceMetrics = InstanceMetrics::new()?;
        fn runtime(self) -> Runtime = {
            Builder::new_multi_thread()
                .enable_all()
                .build()?
        };
        fn index(self) -> Index = {
            let config = self.config()?;
            let path = config.registry_index_path.clone();
            if let Some(registry_url) = config.registry_url.clone() {
                Index::from_url(path, registry_url)
            } else {
                Index::new(path)
            }?
        };
        fn registry_api(self) -> RegistryApi = {
            let config = self.config()?;
            RegistryApi::new(config.registry_api_host.clone(), config.crates_io_api_call_retries)?
        };
        fn repository_stats_updater(self) -> RepositoryStatsUpdater = {
            let config = self.config()?;
            let pool = self.pool()?;
            RepositoryStatsUpdater::new(&config, pool)
        };
    }

    async fn async_pool(&self) -> Result<Pool> {
        self.pool()
    }

    fn pool(&self) -> Result<Pool> {
        Ok(self
            .pool
            .get_or_try_init::<_, Error>(|| {
                Ok(Pool::new(
                    &*self.config()?,
                    self.runtime()?,
                    self.instance_metrics()?,
                )?)
            })?
            .clone())
    }

    async fn async_storage(&self) -> Result<Arc<AsyncStorage>> {
        Ok(Arc::new(
            AsyncStorage::new(self.pool()?, self.instance_metrics()?, self.config()?).await?,
        ))
    }

    async fn async_build_queue(&self) -> Result<Arc<AsyncBuildQueue>> {
        Ok(self
            .async_build_queue
            .get_or_try_init(|| async {
                Ok::<_, Error>(Arc::new(AsyncBuildQueue::new(
                    self.pool()?,
                    self.instance_metrics()?,
                    self.config()?,
                    self.async_storage().await?,
                )))
            })
            .await?
            .clone())
    }

    async fn cdn(&self) -> Result<Arc<CdnBackend>> {
        let config = self.config()?;
        Ok(self
            .cdn
            .get_or_init(|| async { Arc::new(CdnBackend::new(&config).await) })
            .await
            .clone())
    }
}
