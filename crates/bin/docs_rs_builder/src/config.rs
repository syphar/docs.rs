use anyhow::Result;
use docs_rs_config::{AppConfig, AppConfigBuilder};
use docs_rs_env_vars::{maybe_env, prefix};
use std::{path::PathBuf, time::Duration};

#[derive(Debug, bon::Builder)]
#[builder(
    start_fn(name = builder_internal, vis = ""),
    on(_, overwritable)
)]
pub struct Config {
    #[builder(start_fn)]
    pub prefix: PathBuf,

    #[builder(
        default = prefix.join("tmp")
    )]
    pub temp_dir: PathBuf,

    // Where to collect metrics for the metrics initiative.
    // When empty, we won't collect metrics.
    pub compiler_metrics_collection_path: Option<PathBuf>,

    #[builder(
        with = |secs: u64| Duration::from_secs(secs),
        default = Duration::from_secs(86400),
    )]
    pub build_workspace_reinitialization_interval: Duration,

    // Build params
    #[builder(
        default = prefix.join(".workspace")
    )]
    pub rustwide_workspace: PathBuf,
    #[builder(default = false)]
    pub inside_docker: bool,
    pub docker_image: Option<String>,
    pub build_cpu_limit: Option<u32>,
    #[builder(default = true)]
    pub include_default_targets: bool,
    #[builder(default = false)]
    pub disable_memory_limit: bool,
}

use config_builder::State;

impl Config {
    pub fn builder() -> Result<ConfigBuilder> {
        Ok(Config::builder_internal(prefix()?))
    }
}

impl<S: State> AppConfigBuilder for ConfigBuilder<S> {
    type Config = Config;
    type Loaded = ConfigBuilder<S>;

    fn load_environment(self) -> Result<Self::Loaded> {
        Ok(self
            .maybe_rustwide_workspace(maybe_env("DOCSRS_RUSTWIDE_WORKSPACE")?)
            .maybe_inside_docker(maybe_env("DOCSRS_DOCKER")?)
            .maybe_docker_image(
                maybe_env("DOCSRS_LOCAL_DOCKER_IMAGE")?.or(maybe_env("DOCSRS_DOCKER_IMAGE")?),
            )
            .maybe_build_cpu_limit(maybe_env("DOCSRS_BUILD_CPU_LIMIT")?)
            .maybe_include_default_targets(maybe_env("DOCSRS_INCLUDE_DEFAULT_TARGETS")?)
            .maybe_disable_memory_limit(maybe_env("DOCSRS_DISABLE_MEMORY_LIMIT")?)
            .maybe_build_workspace_reinitialization_interval(maybe_env(
                "DOCSRS_BUILD_WORKSPACE_REINITIALIZATION_INTERVAL",
            )?)
            .maybe_compiler_metrics_collection_path(maybe_env("DOCSRS_COMPILER_METRICS_PATH")?))
    }

    #[cfg(test)]
    fn test_config(self) -> Result<Self::Loaded> {
        Ok(self.load_environment()?.include_default_targets(true))
    }

    fn build(self) -> Self::Config {
        self.build()
    }
}

impl AppConfig for Config {
    fn from_environment() -> Result<Self> {
        Ok(Self::builder()?.load_environment()?.build())
    }

    #[cfg(test)]
    fn test_config() -> Result<Self> {
        Ok(Self::builder()?.test_config()?.build())
    }
}
