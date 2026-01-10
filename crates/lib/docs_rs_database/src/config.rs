use anyhow::Result;
use docs_rs_config::{BuildConfig, EnvConfigBuilder, HasBuilder};
use docs_rs_env_vars::{maybe_env, require_env};
use url::Url;

#[derive(Debug, bon::Builder)]
#[builder(on(_, overwritable))]
pub struct Config {
    pub database_url: Url,

    #[builder(default = 90)]
    pub max_pool_size: u32,

    #[builder(default = 10)]
    pub min_pool_idle: u32,
}

use config_builder::{SetDatabaseUrl, State};

impl<S: State> EnvConfigBuilder for ConfigBuilder<S> {
    type Config = Config;
    type Loaded = ConfigBuilder<SetDatabaseUrl<S>>;

    fn load_environment(self) -> Result<Self::Loaded> {
        Ok(self
            .database_url(require_env("DOCSRS_DATABASE_URL")?)
            .maybe_min_pool_idle(maybe_env("DOCSRS_MIN_POOL_IDLE")?)
            .maybe_max_pool_size(maybe_env("DOCSRS_MAX_POOL_SIZE")?))
    }

    #[cfg(feature = "testing")]
    fn test_config(self) -> Result<Self::Loaded> {
        Ok(self
            .load_environment()?
            // Use less connections for each test compared to production.
            .max_pool_size(8)
            .min_pool_idle(2))
    }
}

impl<S: State> BuildConfig for ConfigBuilder<SetDatabaseUrl<S>> {
    type Config = Config;

    fn build(self) -> Result<Self::Config> {
        Ok(self.build())
    }
}

impl HasBuilder for Config {
    type Builder = ConfigBuilder;

    fn builder() -> Result<Self::Builder> {
        Ok(Self::builder())
    }
}
