use anyhow::Result;
use docs_rs_config::AppConfig;
use docs_rs_env_vars::maybe_env;
use std::time::Duration;

#[derive(Debug, bon::Builder, Default)]
#[builder(on(_, overwritable))]
pub struct Config {
    #[builder(default = 5)]
    pub build_attempts: u16,

    #[builder(
        with = |secs: u64| Duration::from_secs(secs),
        default = Duration::from_secs(60),
    )]
    pub delay_between_build_attempts: Duration,
}

use config_builder::State;

impl<S: State> ConfigBuilder<S> {
    pub(crate) fn load_environment(self) -> Result<ConfigBuilder<S>> {
        Ok(self
            .maybe_build_attempts(maybe_env("DOCSRS_BUILD_ATTEMPTS")?)
            .maybe_delay_between_build_attempts(maybe_env("DOCSRS_DELAY_BETWEEN_BUILD_ATTEMPTS")?))
    }
}

impl AppConfig for Config {
    fn from_environment() -> Result<Self> {
        Ok(Self::builder().load_environment()?.build())
    }
}
