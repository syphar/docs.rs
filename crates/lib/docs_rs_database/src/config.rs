use anyhow::{Result, anyhow};
use docs_rs_config::AppConfig;
use docs_rs_env_vars::{env, require_env};
use url::Url;

#[derive(Debug)]
pub struct Config {
    pub database_url: Url,
    pub max_pool_size: u32,
    pub min_pool_idle: u32,
    #[cfg(any(feature = "testing", test))]
    pub(crate) original_db_name: Option<String>,
}

impl Config {
    #[cfg(any(feature = "testing", test))]
    pub(crate) fn database_name(&self) -> Result<String> {
        let database_name = self
            .database_url
            .path()
            .strip_prefix('/')
            .ok_or_else(|| anyhow!("failed to parse database name from url"))?
            .to_string();

        Ok(database_name)
    }
}

impl AppConfig for Config {
    fn from_environment() -> Result<Self> {
        Ok(Self {
            database_url: require_env("DOCSRS_DATABASE_URL")?,
            max_pool_size: env("DOCSRS_MAX_POOL_SIZE", 90u32)?,
            min_pool_idle: env("DOCSRS_MIN_POOL_IDLE", 10u32)?,
            #[cfg(any(feature = "testing", test))]
            original_db_name: None,
        })
    }

    #[cfg(any(feature = "testing", test))]
    fn test_config() -> Result<Self> {
        let mut config = Self::from_environment()?;

        let mut database_url = config.database_url.clone();

        let original_db_name = config.database_name()?;

        // generate a random test db name
        let test_db_name = format!("docs_rs_test_db_{}", rand::random::<u64>());
        database_url.set_path(&format!("/{test_db_name}"));

        config.database_url = database_url;

        // original db name is needed to generate a nice db name for the template database
        config.original_db_name = Some(original_db_name);

        // Use less connections for each test compared to production.
        config.max_pool_size = 8;
        config.min_pool_idle = 2;

        Ok(config)
    }
}
