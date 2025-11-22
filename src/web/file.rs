//! Database based file handler

use super::{cache::CachePolicy, headers::IfNoneMatch};
use crate::{
    Config,
    error::Result,
    storage::{AsyncStorage, Blob, StreamingBlob},
};
use axum::{
    body::Body,
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Response as AxumResponse},
};
use axum_extra::{
    TypedHeader,
    headers::{ContentType, LastModified},
};
use std::time::SystemTime;
use tokio_util::io::ReaderStream;

#[derive(Debug)]
pub(crate) struct File(pub(crate) Blob);

impl File {
    /// Gets file from database
    pub(super) async fn from_path(
        storage: &AsyncStorage,
        path: &str,
        config: &Config,
    ) -> Result<File> {
        let max_size = if path.ends_with(".html") {
            config.max_file_size_html
        } else {
            config.max_file_size
        };

        Ok(File(storage.get(path, max_size).await?))
    }
}

impl File {
    pub fn into_response(self, if_none_match: Option<IfNoneMatch>) -> AxumResponse {
        let streaming_blob: StreamingBlob = self.0.into();
        StreamingFile(streaming_blob).into_response(if_none_match)
    }
}

#[derive(Debug)]
pub(crate) struct StreamingFile(pub(crate) StreamingBlob);

impl StreamingFile {
    /// Gets file from database
    pub(super) async fn from_path(storage: &AsyncStorage, path: &str) -> Result<StreamingFile> {
        Ok(StreamingFile(storage.get_stream(path).await?))
    }

    pub fn into_response(self, if_none_match: Option<IfNoneMatch>) -> AxumResponse {
        const CACHE_POLICY: CachePolicy = CachePolicy::ForeverInCdnAndBrowser;

        if let Some(ref if_none_match) = if_none_match
            && let Some(ref etag) = self.0.etag
            && !if_none_match.precondition_passes(etag)
        {
            (
                StatusCode::NOT_MODIFIED,
                TypedHeader(etag.clone()),
                // it's generally a good idea to repeat caching headers on 304 responses
                Extension(CACHE_POLICY),
            )
                .into_response()
        } else {
            // Convert the AsyncBufRead into a Stream of Bytes
            let stream = ReaderStream::new(self.0.content);

            let last_modified: SystemTime = self.0.date_updated.into();
            (
                StatusCode::OK,
                TypedHeader(ContentType::from(self.0.mime)),
                TypedHeader(LastModified::from(last_modified)),
                self.0.etag.map(TypedHeader),
                Extension(CACHE_POLICY),
                Body::from_stream(stream),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::TestEnvironment;
    use chrono::Utc;
    use http::header::{CACHE_CONTROL, LAST_MODIFIED};
    use std::rc::Rc;

    // FIXME: add tests for conditional get in `StreamingFile::into_response`

    #[tokio::test(flavor = "multi_thread")]
    async fn file_roundtrip_axum() -> Result<()> {
        let env = TestEnvironment::new().await?;

        let now = Utc::now();

        env.fake_release().await.create().await?;

        let mut file = File::from_path(
            env.async_storage(),
            "rustdoc/fake-package/1.0.0/fake-package/index.html",
            env.config(),
        )
        .await?;

        file.0.date_updated = now;

        let resp = file.into_response(None);
        assert!(resp.status().is_success());
        assert!(resp.headers().get(CACHE_CONTROL).is_none());
        let cache = resp
            .extensions()
            .get::<CachePolicy>()
            .expect("missing cache response extension");
        assert!(matches!(cache, CachePolicy::ForeverInCdnAndBrowser));
        assert_eq!(
            resp.headers().get(LAST_MODIFIED).unwrap(),
            &now.format("%a, %d %b %Y %T UTC").to_string(),
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_max_size() -> Result<()> {
        const MAX_SIZE: usize = 1024;
        const MAX_HTML_SIZE: usize = 128;

        let env = Rc::new(
            TestEnvironment::with_config(
                TestEnvironment::base_config()
                    .max_file_size(MAX_SIZE)
                    .max_file_size_html(MAX_HTML_SIZE)
                    .build()?,
            )
            .await?,
        );

        env.fake_release()
            .await
            .name("dummy")
            .version("0.1.0")
            .rustdoc_file_with("small.html", &[b'A'; MAX_HTML_SIZE / 2] as &[u8])
            .rustdoc_file_with("exact.html", &[b'A'; MAX_HTML_SIZE] as &[u8])
            .rustdoc_file_with("big.html", &[b'A'; MAX_HTML_SIZE * 2] as &[u8])
            .rustdoc_file_with("small.js", &[b'A'; MAX_SIZE / 2] as &[u8])
            .rustdoc_file_with("exact.js", &[b'A'; MAX_SIZE] as &[u8])
            .rustdoc_file_with("big.js", &[b'A'; MAX_SIZE * 2] as &[u8])
            .create()
            .await?;

        let file = |path| {
            let env = env.clone();
            async move {
                File::from_path(
                    env.async_storage(),
                    &format!("rustdoc/dummy/0.1.0/{path}"),
                    env.config(),
                )
                .await
            }
        };
        let assert_len = |len, path| async move {
            assert_eq!(len, file(path).await.unwrap().0.content.len());
        };
        let assert_too_big = |path| async move {
            file(path)
                .await
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .and_then(|io| io.get_ref())
                .and_then(|err| err.downcast_ref::<crate::error::SizeLimitReached>())
                .is_some()
        };

        assert_len(MAX_HTML_SIZE / 2, "small.html").await;
        assert_len(MAX_HTML_SIZE, "exact.html").await;
        assert_len(MAX_SIZE / 2, "small.js").await;
        assert_len(MAX_SIZE, "exact.js").await;

        assert_too_big("big.html").await;
        assert_too_big("big.js").await;

        Ok(())
    }
}
