use crate::types::StorageKind;
use anyhow::Result;
use docs_rs_config::AppConfig;
use docs_rs_env_vars::{maybe_env, require_env};
use std::{
    io,
    path::{self, Path, PathBuf},
};

fn ensure_absolute_path(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(path::absolute(&path)?)
    }
}

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

    // Storage params
    #[builder(default)]
    pub storage_backend: StorageKind,

    // AWS SDK configuration
    #[builder(default = 6)]
    pub aws_sdk_max_retries: u32,

    // S3 params
    #[builder(default = "rust-docs-rs".to_string())]
    pub s3_bucket: String,
    #[builder(default = "us-west-1".to_string())]
    pub s3_region: String,
    pub s3_endpoint: Option<String>,

    // DO NOT CONFIGURE THIS THROUGH AN ENVIRONMENT VARIABLE!
    // Accidentally turning this on outside of the test suite might cause data loss in the
    // production environment.
    #[cfg(any(test, feature = "testing"))]
    #[builder(default = false)]
    pub s3_bucket_is_temporary: bool,

    // Max size of the files served by the docs.rs frontend
    #[builder(default = 50 * 1024 * 1024)]
    pub max_file_size: usize,
    #[builder(default = 50 * 1024 * 1024)]
    pub max_file_size_html: usize,

    // where do we want to store the locally cached index files
    // for the remote archives?
    #[builder(
        with = |path: PathBuf| -> io::Result<_> {
            ensure_absolute_path(path)
        },
        default = prefix.join("tmp")
    )]
    pub local_archive_cache_path: PathBuf,

    // expected number of entries in the local archive cache.
    // Makes server restarts faster by preallocating some data structures.
    // General numbers (as of 2025-12):
    // * we have ~1.5 mio releases with archive storage (and 400k without)
    // * each release has on average 2 archive files (rustdoc, source)
    // so, over all, 3 mio archive index files in S3.
    //
    // While due to crawlers we will download _all_ of them over time, the old
    // metric "releases accessed in the last 10 minutes" was around 50k, if I
    // recall correctly.
    // We're using a local DashMap to store some locks for these indexes,
    // and we already know in advance we need these 50k entries.
    // So we can preallocate the DashMap with this number to avoid resizes.
    #[builder(default = 100_000)]
    pub local_archive_cache_expected_count: usize,
}

use config_builder::State;

impl Config {
    pub fn builder() -> Result<ConfigBuilder> {
        let prefix: PathBuf = require_env("DOCSRS_PREFIX")?;
        Ok(Config::builder_internal(prefix))
    }

    pub fn max_file_size_for(&self, path: impl AsRef<Path>) -> usize {
        static HTML: &str = "html";

        if let Some(ext) = path.as_ref().extension()
            && ext == HTML
        {
            self.max_file_size_html
        } else {
            self.max_file_size
        }
    }
}

impl<S: State> ConfigBuilder<S> {
    pub fn load_environment(self) -> Result<ConfigBuilder<S>> {
        Ok(self
            .maybe_storage_backend(maybe_env("DOCSRS_STORAGE_BACKEND")?)
            .maybe_aws_sdk_max_retries(maybe_env("DOCSRS_AWS_SDK_MAX_RETRIES")?)
            .maybe_s3_bucket(maybe_env("DOCSRS_S3_BUCKET")?)
            .maybe_s3_region(maybe_env("S3_REGION")?)
            .maybe_s3_endpoint(maybe_env("S3_ENDPOINT")?)
            .maybe_local_archive_cache_path(maybe_env("DOCSRS_ARCHIVE_INDEX_CACHE_PATH")?)?
            .maybe_local_archive_cache_expected_count(maybe_env(
                "DOCSRS_ARCHIVE_INDEX_EXPECTED_COUNT",
            )?)
            .maybe_max_file_size(maybe_env("DOCSRS_MAX_FILE_SIZE")?)
            .maybe_max_file_size_html(maybe_env("DOCSRS_MAX_FILE_SIZE_HTML")?))
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn test_config(self) -> Result<ConfigBuilder<S>> {
        Ok(self
            .load_environment()?
            .storage_backend(StorageKind::Memory)
            .local_archive_cache_path(
                std::env::temp_dir().join(format!("docsrs-test-index-{}", rand::random::<u64>())),
            )?
            // Use a temporary S3 bucket, only used when storage_kind is set to S3 in env or later.
            .s3_bucket(format!("docsrs-test-bucket-{}", rand::random::<u64>()))
            .s3_bucket_is_temporary(true))
    }
}

impl AppConfig for Config {
    fn from_environment() -> Result<Self> {
        Ok(Self::builder()?.load_environment()?.build())
    }

    #[cfg(any(test, feature = "testing"))]
    fn test_config() -> Result<Self> {
        Ok(Self::builder()?.test_config()?.build())
    }
}
