//! Retrying JSON GET requests with a bounded, shared cache.
use anyhow::Result;
use docs_rs_headers::{Age, CacheControl, HeaderMapExt, cache_control_ttl};
use docs_rs_utils::APP_USER_AGENT;
use moka::{
    future::Cache,
    ops::compute::{CompResult, Op},
};
use reqwest::StatusCode;
use reqwest_middleware::{ClientBuilder as MiddlewareClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use serde::de::DeserializeOwned;
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use tracing::{debug, instrument};
use url::Url;

mod cached_result;
pub use cached_result::CachedResult;

/// A simplified caching HTTP client for one JSON response type.
///
/// Built for fetching & caching rustsec & std-replacements from github pages.
///
/// Clones share connections and cached values.
/// Only GET requests are supported; cache keys are complete URLs.
///
/// Moka bounds the retained entries. Expired values are replaced on the next lookup.
///
/// Compared to a browser, additionally supports:
/// * caching 404, even without caching headers in the response
#[derive(Debug)]
pub struct Client<T: Send + Sync + 'static> {
    inner: Arc<Inner<T>>,
}

impl<T: Send + Sync + 'static> Clone for Client<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[derive(Debug)]
struct Inner<T: Send + Sync + 'static> {
    http: ClientWithMiddleware,
    default_ttl: Duration,
    not_found_ttl: Option<Duration>,
    cache: Cache<Url, Arc<Snapshot<T>>>,
}

#[derive(Debug)]
struct Snapshot<T> {
    value: Option<Arc<T>>,
    expires_at: Instant,
}

impl<T> Snapshot<T> {
    fn is_fresh(&self) -> bool {
        Instant::now() < self.expires_at
    }

    fn cached_result(&self) -> CachedResult<Option<Arc<T>>> {
        CachedResult {
            value: self.value.clone(),
            ttl: self.expires_at.saturating_duration_since(Instant::now()),
        }
    }
}

#[bon::bon]
impl<T: DeserializeOwned + Send + Sync + 'static> Client<T> {
    /// Build a client without making any requests.
    #[builder(finish_fn(name = build), on(_, into))]
    pub fn builder(
        #[builder(default = 3u32)] max_retries: u32,

        /// Timeout for each attempt, including reading the response body.
        #[builder(default = Duration::from_secs(30))]
        request_timeout: Duration,

        /// Maximum number of URLs retained, including stale entries. Zero disables caching.
        #[builder(default = 100_000u64)]
        cache_capacity: u64,

        /// Freshness when Cache-Control has no usable max-age; Age is subtracted.
        #[builder(default = Duration::from_mins(10))]
        default_ttl: Duration,

        /// Enable caching of 404 responses using response freshness headers.
        /// This duration is the fallback when max-age is missing; Age is subtracted.
        /// A 404 always returns None; without this option it is not stored and its TTL is zero.
        not_found_ttl: Option<Duration>,
    ) -> Result<Self> {
        let http = MiddlewareClientBuilder::new(
            reqwest::Client::builder()
                .user_agent(APP_USER_AGENT)
                .gzip(true)
                .timeout(request_timeout)
                .build()?,
        )
        .with(RetryTransientMiddleware::new_with_policy(
            ExponentialBackoff::builder().build_with_max_retries(max_retries),
        ))
        .build();
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                cache: Cache::builder().max_capacity(cache_capacity).build(),
                default_ttl,
                not_found_ttl,
            }),
        })
    }

    /// Fetch or reuse cached JSON. Concurrent requests for a cached URL serialize
    /// refreshes; unrelated URLs never hold the same lock. Invalid JSON is not cached.
    #[instrument(skip(self), fields(%url))]
    pub async fn get(&self, url: &Url) -> Result<CachedResult<Option<Arc<T>>>> {
        if let Some(snapshot) = self.inner.cache.get(url).await
            && snapshot.is_fresh()
        {
            return Ok(snapshot.cached_result());
        }

        let result = self
            .inner
            .cache
            .entry(url.clone())
            .and_try_compute_with::<_, _, anyhow::Error>(|entry| async move {
                let snapshot = entry.as_ref().map(|entry| entry.value().as_ref());
                // Another caller may have refreshed while this operation waited.
                if snapshot.is_some_and(Snapshot::is_fresh) {
                    return Ok(Op::Nop);
                }
                let refreshed = self.refresh(url).await?;
                if refreshed.value.is_none() && self.inner.not_found_ttl.is_none() {
                    // A missing resource invalidates any previous successful response.
                    Ok(Op::Remove)
                } else {
                    Ok(Op::Put(Arc::new(refreshed)))
                }
            })
            .await?;
        Ok(match result {
            CompResult::Removed(_) | CompResult::StillNone(_) => CachedResult {
                value: None,
                ttl: Duration::ZERO,
            },
            CompResult::Inserted(entry)
            | CompResult::ReplacedWith(entry)
            | CompResult::Unchanged(entry) => entry.value().cached_result(),
        })
    }

    #[instrument(skip_all, fields(%url))]
    async fn refresh(&self, url: &Url) -> Result<Snapshot<T>> {
        debug!(%url, "fetching JSON");
        let response = self.inner.http.get(url.clone()).send().await?;
        let received_at = Instant::now();

        let cache_control = response.headers().typed_get::<CacheControl>();
        let age = response
            .headers()
            .typed_get::<Age>()
            .map(Duration::from)
            .unwrap_or_default();

        let (value, fallback_ttl) = if response.status() == StatusCode::NOT_FOUND {
            (None, self.inner.not_found_ttl)
        } else {
            let value = response.error_for_status()?.json::<T>().await?;
            (Some(Arc::new(value)), Some(self.inner.default_ttl))
        };
        let ttl = fallback_ttl.map_or(Duration::ZERO, |fallback| {
            cache_control_ttl(cache_control.as_ref(), age).unwrap_or(fallback.saturating_sub(age))
        });
        Ok(Snapshot {
            value,
            expires_at: received_at + ttl,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docs_rs_headers::{Header, UserAgent, testing::test_typed_encode};
    use reqwest::header::{CACHE_CONTROL, IF_NONE_MATCH};
    use serde_json::Value;
    use test_case::test_case;

    trait MockExt {
        fn with_typed_header<H: Header>(self, header: H) -> Self;
        fn match_typed_header<H: Header>(self, header: H) -> Self;
        fn with_status_code(self, status_code: StatusCode) -> Self;
    }

    impl MockExt for mockito::Mock {
        fn match_typed_header<H: Header>(self, header: H) -> Self {
            let name = H::name();
            let value = test_typed_encode(header);

            self.match_header(name, value.to_str().unwrap())
        }

        fn with_typed_header<H: Header>(self, header: H) -> Self {
            let name = H::name();
            let value = test_typed_encode(header);

            self.with_header(name, value.to_str().unwrap())
        }

        fn with_status_code(self, status_code: StatusCode) -> Self {
            self.with_status(status_code.as_u16().into())
        }
    }

    async fn advance(duration: Duration) {
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
    }

    #[test_case(false; "404 is not cached by default")]
    #[test_case(true; "optional negative cache")]
    #[tokio::test]
    async fn not_found_caching_is_opt_in(cache_missing: bool) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let ttl = Duration::from_mins(1);

        let mock = server
            .mock("GET", "/missing")
            .with_status_code(StatusCode::NOT_FOUND)
            .with_typed_header(CacheControl::new().with_max_age(ttl))
            .with_body("not JSON")
            .expect(if cache_missing { 1 } else { 2 })
            .create_async()
            .await;

        let client = Client::<Value>::builder()
            .max_retries(0u32)
            .maybe_not_found_ttl(cache_missing.then_some(ttl))
            .build()?;

        let url = format!("{}/missing", server.url()).parse()?;
        for _ in 0..2 {
            let result = client.get(&url).await?;
            assert!(result.value.is_none());
            if cache_missing {
                assert!(result.ttl <= Duration::from_secs(60));
                assert!(result.ttl > Duration::from_secs(55));
            } else {
                assert_eq!(result.ttl, Duration::ZERO);
                assert!(client.inner.cache.get(&url).await.is_none());
                client.inner.cache.run_pending_tasks().await;
                assert_eq!(client.inner.cache.entry_count(), 0);
            }
        }
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn refetches_after_negative_cache_expires() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url().parse()?;

        let client = Client::<u64>::builder()
            .max_retries(0u32)
            .not_found_ttl(Duration::from_secs(60))
            .build()?;

        let missing = server
            .mock("GET", "/")
            .with_status_code(StatusCode::NOT_FOUND)
            .expect(1)
            .create_async()
            .await;

        assert!(client.get(&url).await?.value.is_none());
        assert!(client.get(&url).await?.value.is_none());
        missing.assert_async().await;
        missing.remove_async().await;

        let available = server
            .mock("GET", "/")
            .with_body("42")
            .expect(1)
            .create_async()
            .await;

        advance(Duration::from_secs(61)).await;
        assert_eq!(*client.get(&url).await?.value.unwrap(), 42);

        available.assert_async().await;
        Ok(())
    }

    #[test_case(StatusCode::OK, "42"; "successful response")]
    #[test_case(StatusCode::NOT_FOUND, "missing"; "negative response")]
    #[tokio::test]
    async fn zero_capacity_disables_cache(status: StatusCode, body: &str) -> Result<()> {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", "/")
            .with_status_code(status)
            .with_typed_header(CacheControl::new().with_max_age(Duration::from_hours(1)))
            .with_body(body)
            .expect(2)
            .create_async()
            .await;

        let client = Client::<u64>::builder()
            .max_retries(0u32)
            .cache_capacity(0u64)
            .not_found_ttl(Duration::from_secs(600))
            .build()?;

        let url = server.url().parse()?;
        for _ in 0..2 {
            let result = client.get(&url).await?;
            assert_eq!(
                result.value.as_deref().copied(),
                (status == StatusCode::OK).then_some(42)
            );
        }
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn uncached_not_found_removes_previous_snapshot() -> Result<()> {
        let mut server = mockito::Server::new_async().await;

        let client = Client::<u64>::builder().max_retries(0u32).build()?;

        let url = server.url().parse()?;
        let initial = server
            .mock("GET", "/")
            .with_body("1")
            .with_typed_header(CacheControl::new().with_max_age(Duration::ZERO))
            .create_async()
            .await;

        assert_eq!(*client.get(&url).await?.value.unwrap(), 1);
        initial.assert_async().await;
        initial.remove_async().await;

        let missing = server
            .mock("GET", "/")
            .with_status_code(StatusCode::NOT_FOUND)
            .with_typed_header(CacheControl::new().with_max_age(Duration::from_mins(10)))
            .create_async()
            .await;

        let result = client.get(&url).await?;
        assert!(result.value.is_none());
        assert_eq!(result.ttl, Duration::ZERO);
        assert!(client.inner.cache.get(&url).await.is_none());
        missing.assert_async().await;
        missing.remove_async().await;
        let recovered = server.mock("GET", "/").with_body("2").create_async().await;
        assert_eq!(*client.get(&url).await?.value.unwrap(), 2);
        recovered.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn clones_share_fetches_and_urls_have_separate_entries() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let a = server
            .mock("GET", "/a")
            .match_typed_header(UserAgent::from_static(APP_USER_AGENT))
            .with_body("1")
            .expect(1)
            .create_async()
            .await;

        let b = server
            .mock("GET", "/b")
            .with_body("2")
            .expect(1)
            .create_async()
            .await;

        let client = Client::<u64>::builder().max_retries(0u32).build()?;
        let clone = client.clone();
        let url_a = format!("{}/a", server.url()).parse()?;
        let url_b = format!("{}/b", server.url()).parse()?;
        let (first, second, other) =
            tokio::try_join!(client.get(&url_a), clone.get(&url_a), client.get(&url_b))?;
        assert!(Arc::ptr_eq(&first.value.unwrap(), &second.value.unwrap()));
        assert_eq!(*other.value.unwrap(), 2);
        a.assert_async().await;
        b.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn timeout_covers_stalled_response_body() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/", listener.local_addr()?).parse()?;
        let (sent, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n[")
                .await
                .unwrap();
            sent.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let client = Client::<Value>::builder()
            .max_retries(0u32)
            .request_timeout(Duration::from_millis(200))
            .build()?;
        let result = tokio::time::timeout(Duration::from_secs(5), client.get(&url)).await;
        server.abort();
        received.await?;
        let error = result?.unwrap_err();
        assert!(
            error
                .chain()
                .filter_map(|err| err.downcast_ref::<reqwest::Error>())
                .any(reqwest::Error::is_timeout)
        );
        Ok(())
    }

    #[test_case(None, 0, 60; "fallback")]
    #[test_case(None, 20, 40; "fallback age")]
    #[test_case(Some("max-age=120"), 20, 100; "header overrides fallback")]
    #[test_case(Some("public"), 0, 60; "missing max age")]
    #[test_case(Some("max-age=invalid"), 0, 60; "invalid max age")]
    #[test_case(Some("max-age=10"), 20, 0; "expired response")]
    #[test_case(Some("no-cache"), 0, 0; "no cache")]
    #[tokio::test]
    async fn not_found_respects_freshness(
        control: Option<&str>,
        age: u64,
        expected: u64,
    ) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mut mock = server
            .mock("GET", "/")
            .with_status_code(StatusCode::NOT_FOUND)
            .with_typed_header(Age::from_secs(age))
            .with_body("not JSON")
            .expect(if expected == 0 { 2 } else { 1 });
        if let Some(control) = control {
            // Keep the raw value: these cases also test malformed Cache-Control headers.
            mock = mock.with_header(CACHE_CONTROL, control);
        }

        let mock = mock.create_async().await;
        let client = Client::<Value>::builder()
            .max_retries(0u32)
            .not_found_ttl(Duration::from_secs(60))
            .build()?;

        let url = server.url().parse()?;
        for _ in 0..2 {
            let result = client.get(&url).await?;
            assert!(result.value.is_none());
            assert!(result.ttl <= Duration::from_secs(expected));
            if expected > 0 {
                assert!(result.ttl > Duration::from_secs(expected - 5));
            }
        }
        mock.assert_async().await;
        Ok(())
    }

    async fn fixture() -> Result<(mockito::ServerGuard, Client<String>, Url)> {
        let server = mockito::Server::new_async().await;
        let url = server.url().parse()?;
        let api = Client::builder().max_retries(0u32).build()?;
        Ok((server, api, url))
    }

    fn body(value: &str) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test_case(None, 100, 0; "expired fallback")]
    #[test_case(None, 0, 90; "absent cache control")]
    #[test_case(Some("public"), 0, 90; "missing max age")]
    #[test_case(Some("max-age=invalid"), 0, 90; "invalid max age")]
    #[test_case(None, 30, 60; "fallback accounts for age")]
    #[test_case(Some("max-age=600"), 30, 570; "headers override default")]
    #[tokio::test]
    async fn configured_fallback_ttl(control: Option<&str>, age: u64, expected: u64) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let api = Client::<String>::builder()
            .max_retries(0u32)
            .default_ttl(Duration::from_secs(90))
            .build()?;

        let url = server.url().parse()?;
        let mut mock = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_body(body("empty"))
            .with_typed_header(Age::from_secs(age));
        if let Some(control) = control {
            // Keep the raw value: these cases also test malformed Cache-Control headers.
            mock = mock.with_header(CACHE_CONTROL, control);
        }
        let mock = mock.create_async().await;
        let result = api.get(&url).await?;
        assert_eq!(result.value.unwrap().as_str(), "empty");
        assert!(result.ttl <= Duration::from_secs(expected));
        if expected > 0 {
            assert!(result.ttl > Duration::from_secs(expected - 5));
        }
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn fallback_ttl_expires_and_refreshes_without_conditional_request() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let api = Client::<String>::builder()
            .max_retries(0u32)
            .default_ttl(Duration::from_secs(90))
            .build()?;

        let url = server.url().parse()?;
        let initial = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_body(body("initial"))
            .with_header("etag", "\"one\"")
            .expect(1)
            .create_async()
            .await;
        api.get(&url).await?;
        advance(Duration::from_secs(60)).await;
        let cached = api.get(&url).await?;
        assert!(cached.ttl <= Duration::from_secs(30));
        assert!(cached.ttl > Duration::from_secs(25));
        initial.assert_async().await;
        initial.remove_async().await;
        let updated = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .match_header(IF_NONE_MATCH, mockito::Matcher::Missing)
            .with_body(body("updated"))
            .expect(1)
            .create_async()
            .await;
        advance(Duration::from_secs(31)).await;
        let (renewed, concurrent) = tokio::try_join!(api.get(&url), api.get(&url))?;
        assert_eq!(renewed.value.as_deref().unwrap(), "updated");
        assert!(Arc::ptr_eq(
            renewed.value.as_ref().unwrap(),
            concurrent.value.as_ref().unwrap()
        ));
        assert!(renewed.ttl <= Duration::from_secs(90));
        assert!(renewed.ttl > Duration::from_secs(85));
        updated.assert_async().await;
        Ok(())
    }

    #[test_case(StatusCode::INTERNAL_SERVER_ERROR, "failed"; "HTTP failure")]
    #[test_case(StatusCode::OK, "invalid"; "invalid JSON")]
    #[tokio::test]
    async fn failed_refresh_returns_error_and_can_be_retried(
        status: StatusCode,
        body_text: &str,
    ) -> Result<()> {
        let (mut server, api, url) = fixture().await?;
        let initial = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_body(body("old"))
            .with_typed_header(CacheControl::new().with_max_age(Duration::ZERO))
            .create_async()
            .await;
        api.get(&url).await?;
        initial.remove_async().await;
        let failed = server
            .mock("GET", "/")
            .with_status_code(status)
            .with_body(body_text)
            .expect(1)
            .create_async()
            .await;
        assert!(api.get(&url).await.is_err());
        failed.assert_async().await;
        failed.remove_async().await;
        let recovered = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_body(body("new"))
            .create_async()
            .await;
        assert_eq!(api.get(&url).await?.value.unwrap().as_str(), "new");
        recovered.assert_async().await;
        Ok(())
    }

    #[test_case(StatusCode::INTERNAL_SERVER_ERROR, "failed"; "HTTP failure")]
    #[test_case(StatusCode::OK, "invalid"; "invalid JSON")]
    #[test_case(StatusCode::NOT_MODIFIED, ""; "unexpected 304")]
    #[tokio::test]
    async fn initial_failure_can_be_retried(status: StatusCode, body_text: &str) -> Result<()> {
        let (mut server, api, url) = fixture().await?;
        let failed = server
            .mock("GET", "/")
            .with_status_code(status)
            .with_body(body_text)
            .create_async()
            .await;
        assert!(api.get(&url).await.is_err());
        failed.assert_async().await;
        failed.remove_async().await;
        let recovered = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_body(body("empty"))
            .create_async()
            .await;
        assert_eq!(api.get(&url).await?.value.unwrap().as_str(), "empty");
        recovered.assert_async().await;
        Ok(())
    }

    #[test_case(StatusCode::TOO_MANY_REQUESTS; "rate limit")]
    #[test_case(StatusCode::INTERNAL_SERVER_ERROR; "server error")]
    #[tokio::test]
    async fn does_not_cache_errors(status: StatusCode) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let failed = server
            .mock("GET", "/")
            .with_status_code(status)
            .with_typed_header(CacheControl::new().with_max_age(Duration::from_hours(1)))
            .expect(1)
            .create_async()
            .await;
        let api = Client::<Vec<u64>>::builder().max_retries(0u32).build()?;
        let url = server.url().parse()?;
        assert!(api.get(&url).await.is_err());
        failed.assert_async().await;
        failed.remove_async().await;
        let available = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_body("[1,2]")
            .expect(1)
            .create_async()
            .await;
        assert_eq!(api.get(&url).await?.value.unwrap().len(), 2);
        available.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn refetches_stale_responses() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let initial = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_typed_header(CacheControl::new().with_max_age(Duration::ZERO))
            .with_body("[1,2]")
            .expect(1)
            .create_async()
            .await;
        let api = Client::<Vec<u64>>::builder().max_retries(0u32).build()?;
        let url = server.url().parse()?;
        assert_eq!(api.get(&url).await?.value.unwrap().len(), 2);
        initial.assert_async().await;
        initial.remove_async().await;
        let updated = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_typed_header(CacheControl::new().with_max_age(Duration::from_mins(10)))
            .with_body("[]")
            .expect(1)
            .create_async()
            .await;
        assert!(api.get(&url).await?.value.unwrap().is_empty());
        assert!(api.get(&url).await?.value.unwrap().is_empty());
        updated.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn retries_transient_failures() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let failure = server
            .mock("GET", "/")
            .with_status_code(StatusCode::SERVICE_UNAVAILABLE)
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("GET", "/")
            .with_status_code(StatusCode::OK)
            .with_body("[1,2]")
            .expect(1)
            .create_async()
            .await;
        assert_eq!(
            Client::<Vec<u64>>::builder()
                .max_retries(1u32)
                .build()?
                .get(&server.url().parse()?)
                .await?
                .value
                .unwrap()
                .len(),
            2
        );
        failure.assert_async().await;
        success.assert_async().await;
        Ok(())
    }
}
