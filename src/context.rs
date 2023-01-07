use crate::cdn::CdnBackend;
use crate::db::Pool;
use crate::repositories::RepositoryStatsUpdater;
use crate::{BuildQueue, Config, Index, Metrics, Storage};
use std::ops::Deref;
use std::sync::Arc;
use tokio::runtime::Runtime;

pub trait Context {
    fn config(&self) -> Arc<Config>;
    fn build_queue(&self) -> Arc<BuildQueue>;
    fn storage(&self) -> Arc<Storage>;
    fn cdn(&self) -> Arc<CdnBackend>;
    fn pool(&self) -> Pool;
    fn metrics(&self) -> Arc<Metrics>;
    fn index(&self) -> Arc<Index>;
    fn repository_stats_updater(&self) -> Arc<RepositoryStatsUpdater>;
    fn runtime(&self) -> Arc<Runtime>;
}

pub type AppContext = Arc<dyn Context + Send + Sync + 'static>;

// FIXME: why do we need this? can we prevent this?
impl Context for AppContext {
    fn config(&self) -> Arc<Config> {
        Deref::deref(self).config()
    }

    fn build_queue(&self) -> Arc<BuildQueue> {
        Deref::deref(self).build_queue()
    }

    fn storage(&self) -> Arc<Storage> {
        Deref::deref(self).storage()
    }

    fn cdn(&self) -> Arc<CdnBackend> {
        Deref::deref(self).cdn()
    }

    fn pool(&self) -> Pool {
        Deref::deref(self).pool()
    }

    fn metrics(&self) -> Arc<Metrics> {
        Deref::deref(self).metrics()
    }

    fn index(&self) -> Arc<Index> {
        Deref::deref(self).index()
    }

    fn repository_stats_updater(&self) -> Arc<RepositoryStatsUpdater> {
        Deref::deref(self).repository_stats_updater()
    }

    fn runtime(&self) -> Arc<Runtime> {
        Deref::deref(self).runtime()
    }
}
