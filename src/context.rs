use crate::cdn::CdnBackend;
use crate::db::Pool;
use crate::error::Result;
use crate::repositories::RepositoryStatsUpdater;
use crate::{BuildQueue, Config, Index, Metrics, Storage};
use std::ops::Deref;
use std::sync::Arc;
use tokio::runtime::Runtime;

pub trait Context {
    fn config(&self) -> Result<Arc<Config>>;
    fn build_queue(&self) -> Result<Arc<BuildQueue>>;
    fn storage(&self) -> Result<Arc<Storage>>;
    fn cdn(&self) -> Result<Arc<CdnBackend>>;
    fn pool(&self) -> Result<Pool>;
    fn metrics(&self) -> Result<Arc<Metrics>>;
    fn index(&self) -> Result<Arc<Index>>;
    fn repository_stats_updater(&self) -> Result<Arc<RepositoryStatsUpdater>>;
    fn runtime(&self) -> Result<Arc<Runtime>>;
}

pub type AppContext = Arc<dyn Context + Send + Sync + 'static>;

// FIXME: why do we need this? can we prevent this?
impl Context for AppContext {
    fn config(&self) -> Result<Arc<Config>> {
        Deref::deref(self).config()
    }

    fn build_queue(&self) -> Result<Arc<BuildQueue>> {
        Deref::deref(self).build_queue()
    }

    fn storage(&self) -> Result<Arc<Storage>> {
        Deref::deref(self).storage()
    }

    fn cdn(&self) -> Result<Arc<CdnBackend>> {
        Deref::deref(self).cdn()
    }

    fn pool(&self) -> Result<Pool> {
        Deref::deref(self).pool()
    }

    fn metrics(&self) -> Result<Arc<Metrics>> {
        Deref::deref(self).metrics()
    }

    fn index(&self) -> Result<Arc<Index>> {
        Deref::deref(self).index()
    }

    fn repository_stats_updater(&self) -> Result<Arc<RepositoryStatsUpdater>> {
        Deref::deref(self).repository_stats_updater()
    }

    fn runtime(&self) -> Result<Arc<Runtime>> {
        Deref::deref(self).runtime()
    }
}
