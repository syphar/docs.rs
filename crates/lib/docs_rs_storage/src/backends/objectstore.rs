use crate::{Config, backends::StorageBackendMethods};
use anyhow::Result;
use object_store::{RetryConfig, aws::AmazonS3Builder};

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
    async fn exists(&self, path: &str) -> anyhow::Result<bool> {
        todo!()
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
