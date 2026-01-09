use anyhow::Result;
use docs_rs_config::AppConfig;
use docs_rs_env_vars::{env, maybe_env, require_env};
use std::{path::PathBuf, time::Duration};

#[derive(Debug, bon::Builder)]
#[builder(on(_, overwritable))]
pub struct Config {
    pub registry_index_path: PathBuf,
    pub registry_url: Option<String>,

    /// How long to wait between registry checks
    #[builder(
        with = |secs: u64| Duration::from_secs(secs),
        default = Duration::from_secs(60),
    )]
    pub delay_between_registry_fetches: Duration,

    // Time between 'git gc --auto' calls
    #[builder(
        with = |secs: u64| Duration::from_secs(secs),
        default = Duration::from_secs(60 * 60),
    )]
    pub registry_gc_interval: Duration,

    // automatic rebuild configuration
    pub max_queued_rebuilds: Option<u16>,
}

use config_builder::{SetRegistryIndexPath, State};

impl<S: State> ConfigBuilder<S> {
    pub(crate) fn load_environment(self) -> Result<ConfigBuilder<SetRegistryIndexPath<S>>> {
        let prefix: PathBuf = require_env("DOCSRS_PREFIX")?;

        Ok(self
            .registry_index_path(env("REGISTRY_INDEX_PATH", prefix.join("crates.io-index"))?)
            .maybe_registry_url(maybe_env("REGISTRY_URL")?)
            .maybe_registry_gc_interval(maybe_env("DOCSRS_REGISTRY_GC_INTERVAL")?)
            .maybe_max_queued_rebuilds(maybe_env("DOCSRS_MAX_QUEUED_REBUILDS")?))
    }

    #[cfg(test)]
    pub(crate) fn test_config(self) -> Result<ConfigBuilder<SetRegistryIndexPath<S>>> {
        self.load_environment()
    }
}

impl AppConfig for Config {
    fn from_environment() -> Result<Self> {
        Ok(Self::builder().load_environment()?.build())
    }
}
