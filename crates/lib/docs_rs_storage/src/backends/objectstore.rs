use crate::{Config, backends::StorageBackendMethods};
use anyhow::Result;
use object_store::{ObjectStoreExt as _, RetryConfig, aws::AmazonS3Builder, path::Path};

pub(crate) struct ObjectStoreBackend {
    store: object_store::aws::AmazonS3,
}

impl ObjectStoreBackend {
    pub(crate) fn new(config: &Config) -> Result<Self> {
        let mut builder = AmazonS3Builder::from_env()
            .with_retry(RetryConfig {
                max_retries: config.aws_sdk_max_retries as usize,
                ..Default::default()
            })
            .with_region(config.s3_region.clone())
            .with_bucket_name(config.s3_bucket.clone());

        if let Some(ref endpoint) = config.s3_endpoint {
            builder = builder
                .with_virtual_hosted_style_request(false)
                .with_url(endpoint);
        }

        // FIXME: create bucket

        Ok(Self {
            store: builder.build()?,
        })
    }
}

impl StorageBackendMethods for ObjectStoreBackend {
    async fn exists(&self, path: &str) -> Result<bool> {
        let path = Path::from(path);
        match self.store.head(&path).await {
            Ok(_) => Ok(true),
            Err(err) if matches!(err, object_store::Error::NotFound { path: _, source: _ }) => {
                Ok(false)
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn get_stream(
        &self,
        path: &str,
        range: Option<crate::types::FileRange>,
    ) -> anyhow::Result<crate::StreamingBlob> {
        todo!()
    }

    async fn upload_stream(&self, upload: crate::blob::StreamUpload) -> anyhow::Result<()> {
        todo!()
    }

    async fn list_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> futures_util::stream::BoxStream<'a, anyhow::Result<String>> {
        todo!()
    }

    async fn delete_prefix(&self, prefix: &str) -> anyhow::Result<()> {
        todo!()
    }
}
