use anyhow::Result;
use docs_rs_config::AppConfig;
use docs_rs_env_vars::maybe_env;

#[derive(Debug, bon::Builder)]
#[builder(on(_, overwritable))]
pub struct Config {
    // Github authentication
    pub(crate) github_accesstoken: Option<String>,

    #[builder(default = 2500)]
    pub(crate) github_updater_min_rate_limit: u32,

    // GitLab authentication
    pub(crate) gitlab_accesstoken: Option<String>,
}

use config_builder::State;

impl<S: State> ConfigBuilder<S> {
    pub(crate) fn load_environment(self) -> Result<ConfigBuilder<S>> {
        Ok(self
            .maybe_github_accesstoken(maybe_env("DOCSRS_GITHUB_ACCESSTOKEN")?)
            .maybe_github_updater_min_rate_limit(maybe_env("DOCSRS_GITHUB_UPDATER_MIN_RATE_LIMIT")?)
            .maybe_gitlab_accesstoken(maybe_env("DOCSRS_GITLAB_ACCESSTOKEN")?))
    }

    #[cfg(test)]
    pub(crate) fn test_config(self) -> Result<ConfigBuilder<S>> {
        self.load_environment()
    }
}

impl AppConfig for Config {
    fn from_environment() -> Result<Self> {
        Ok(Self::builder().load_environment()?.build())
    }

    #[cfg(test)]
    fn test_config() -> Result<Self> {
        Ok(Self::builder().test_config()?.build())
    }
}
