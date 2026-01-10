use anyhow::Result;
use docs_rs_config::AppConfig;
use docs_rs_env_vars::maybe_env;
use url::Url;

#[derive(Debug, bon::Builder)]
#[builder(on(_, overwritable))]
pub struct Config {
    /// Fastly API host, typically only overwritten for testing
    #[builder(default =  "https://api.fastly.com".parse().unwrap())]
    pub api_host: Url,

    /// Fastly API token for purging the services below.
    pub api_token: Option<String>,

    /// fastly service SID for the main domain
    pub service_sid: Option<String>,
}

use config_builder::State;

impl<S: State> ConfigBuilder<S> {
    pub(crate) fn load_environment(self) -> Result<ConfigBuilder<S>> {
        Ok(self
            .maybe_api_host(maybe_env("DOCSRS_FASTLY_API_HOST")?)
            .maybe_api_token(maybe_env("DOCSRS_FASTLY_API_TOKEN")?)
            .maybe_service_sid(maybe_env("DOCSRS_FASTLY_SERVICE_SID_WEB")?))
    }

    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn test_config(self) -> ConfigBuilder<S> {
        self.api_token("some_token".into())
            .service_sid("some_sid".into())
    }
}

impl AppConfig for Config {
    fn from_environment() -> Result<Self> {
        Ok(Self::builder().load_environment()?.build())
    }

    #[cfg(any(test, feature = "testing"))]
    fn test_config() -> Result<Self> {
        let cfg = Self::builder().test_config().build();
        debug_assert!(cfg.is_valid());

        Ok(cfg)
    }
}

impl Config {
    pub fn is_valid(&self) -> bool {
        self.api_token.is_some() && self.service_sid.is_some()
    }
}
