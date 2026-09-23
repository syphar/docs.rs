//! Retrying JSON GET requests with a bounded, shared cache and ETag revalidation.
use anyhow::{Result, bail};
use docs_rs_headers::{Age, CacheControl, ETag, HeaderMapExt, IfNoneMatch, cache_control_ttl};
use docs_rs_utils::APP_USER_AGENT;
use moka::{
    future::Cache,
    ops::compute::{CompResult, Op},
};
use reqwest::{StatusCode, header::HeaderMap};
use reqwest_middleware::{ClientBuilder as MiddlewareClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use serde::de::DeserializeOwned;
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use tracing::{debug, error, instrument};
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
/// Moka bounds the retained entries. Freshness is checked separately so expired
/// values and validators remain available for conditional requests and error fallback.
///
/// Compared to a browser, additionally supports:
/// * caching 404, even without caching headers in the response
/// * serving stale data from the cache when the refresh fails.
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
    stale_if_error: Option<Duration>,
    cache: Cache<Url, Arc<Snapshot<T>>>,
}

#[derive(Debug)]
struct Snapshot<T> {
    value: Option<Arc<T>>,
    etag: Option<ETag>,
    cache_control: Option<CacheControl>,
    expires_at: Instant,
}

impl<T> Snapshot<T> {
    fn is_fresh(&self) -> bool {
        Instant::now() < self.expires_at
    }

    fn cached_result(&self) -> CachedResult<Option<Arc<T>>> {
        CachedResult {
            value: self.value.clone(),
            ttl: Some(self.expires_at.saturating_duration_since(Instant::now())),
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

        /// On refresh failure, reuse the previous result and defer retries by this duration.
        /// Without this option, errors propagate. Initial-load errors always propagate.
        stale_if_error: Option<Duration>,
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
                stale_if_error,
            }),
        })
    }

    /// Fetch or revalidate JSON. Concurrent requests for a cached URL serialize
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
            .and_try_compute_with(|entry| async move {
                let snapshot = entry.as_ref().map(|entry| entry.value().as_ref());
                // Another caller may have refreshed while this operation waited.
                if snapshot.is_some_and(Snapshot::is_fresh) {
                    return Ok(Op::Nop);
                }
                let refreshed = match self.refresh(url, snapshot).await {
                    Ok(refreshed) => refreshed,
                    Err(error) => match (snapshot, self.inner.stale_if_error) {
                        (Some(snapshot), Some(delay)) => {
                            error!(?error, %url, "refresh failed; serving cached JSON");
                            Snapshot {
                                value: snapshot.value.clone(),
                                etag: snapshot.etag.clone(),
                                cache_control: snapshot.cache_control.clone(),
                                expires_at: Instant::now() + delay,
                            }
                        }
                        _ => return Err(error),
                    },
                };
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
                ttl: None,
            },
            CompResult::Inserted(entry)
            | CompResult::ReplacedWith(entry)
            | CompResult::Unchanged(entry) => entry.value().cached_result(),
        })
    }

    #[instrument(skip_all, fields(%url))]
    async fn refresh(&self, url: &Url, snapshot: Option<&Snapshot<T>>) -> Result<Snapshot<T>> {
        let mut request = self.inner.http.get(url.clone());
        if let Some(etag) = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.etag.as_ref())
        {
            let mut headers = HeaderMap::new();
            headers.typed_insert(IfNoneMatch(etag.clone().into()));

            request = request.headers(headers);
        }

        debug!(%url, "fetching JSON");
        let response = request.send().await?;
        let received_at = Instant::now();

        let cache_control = response.headers().typed_get::<CacheControl>();
        let age = response
            .headers()
            .typed_get::<Age>()
            .map(Duration::from)
            .unwrap_or_default();

        if response.status() == StatusCode::NOT_FOUND {
            let ttl = self.inner.not_found_ttl.map_or(Duration::ZERO, |fallback| {
                cache_control_ttl(cache_control.as_ref(), age)
                    .unwrap_or(fallback.saturating_sub(age))
            });
            return Ok(Snapshot {
                value: None,
                etag: None,
                cache_control: None,
                expires_at: received_at + ttl,
            });
        }

        let response = response.error_for_status()?;

        let etag: Option<ETag> = response.headers().typed_get();
        let expires_at = |control: Option<&CacheControl>| {
            received_at
                + cache_control_ttl(control, age)
                    .unwrap_or(self.inner.default_ttl.saturating_sub(age))
        };

        if response.status() == StatusCode::NOT_MODIFIED {
            let Some(snapshot) = snapshot else {
                bail!("received 304 without a cached JSON response");
            };
            // if the 304 response doesn't have a cache-control header, use the one
            // from the cached response.
            let cache_control = cache_control.or_else(|| snapshot.cache_control.clone());
            debug!(%url, "cached JSON unchanged");
            Ok(Snapshot {
                value: snapshot.value.clone(),
                // if the 304 response doesn't have an Etag header, use the one
                // from the cached response.
                etag: etag.or_else(|| snapshot.etag.clone()),
                expires_at: expires_at(cache_control.as_ref()),
                cache_control,
            })
        } else {
            let value = response.json::<T>().await?;
            Ok(Snapshot {
                value: Some(Arc::new(value)),
                etag,
                expires_at: expires_at(cache_control.as_ref()),
                cache_control,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use test_case::test_case;

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
        let mock = server
            .mock("GET", "/missing")
            .with_status(404)
            .with_header("cache-control", "max-age=60")
            .with_body("not JSON")
            .expect(if cache_missing { 1 } else { 2 })
            .create_async()
            .await;
        let client = Client::<Value>::builder()
            .max_retries(0u32)
            .maybe_not_found_ttl(cache_missing.then_some(Duration::from_secs(60)))
            .build()?;
        let url = format!("{}/missing", server.url()).parse()?;
        for _ in 0..2 {
            let result = client.get(&url).await?;
            assert!(result.value.is_none());
            if cache_missing {
                assert!(result.ttl.unwrap() <= Duration::from_secs(60));
                assert!(result.ttl.unwrap() > Duration::from_secs(55));
            } else {
                assert!(result.ttl.is_none());
                assert!(client.inner.cache.get(&url).await.is_none());
                client.inner.cache.run_pending_tasks().await;
                assert_eq!(client.inner.cache.entry_count(), 0);
            }
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
            .with_header("etag", "\"one\"")
            .with_header("cache-control", "max-age=0")
            .create_async()
            .await;
        assert_eq!(*client.get(&url).await?.value.unwrap(), 1);
        initial.assert_async().await;
        initial.remove_async().await;
        let missing = server
            .mock("GET", "/")
            .match_header("if-none-match", "\"one\"")
            .with_status(404)
            .with_header("cache-control", "max-age=600")
            .create_async()
            .await;
        let result = client.get(&url).await?;
        assert!(result.value.is_none());
        assert!(result.ttl.is_none());
        assert!(client.inner.cache.get(&url).await.is_none());
        missing.assert_async().await;
        missing.remove_async().await;
        let recovered = server
            .mock("GET", "/")
            .match_header("if-none-match", mockito::Matcher::Missing)
            .with_body("2")
            .create_async()
            .await;
        assert_eq!(*client.get(&url).await?.value.unwrap(), 2);
        recovered.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn clones_share_fetches_and_urls_have_separate_entries() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let a = server
            .mock("GET", "/a")
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
    async fn revalidation_retains_value_and_new_response_clears_etag() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let client = Client::<u64>::builder().max_retries(0u32).build()?;
        let url = server.url().parse()?;
        let initial = server
            .mock("GET", "/")
            .with_body("1")
            .with_header("cache-control", "max-age=60")
            .with_header("etag", "\"one\"")
            .create_async()
            .await;
        let first = client.get(&url).await?.value.unwrap();
        initial.assert_async().await;
        initial.remove_async().await;
        advance(Duration::from_secs(61)).await;
        let unchanged = server
            .mock("GET", "/")
            .match_header("if-none-match", "\"one\"")
            .with_status(304)
            .create_async()
            .await;
        let second = client.get(&url).await?;
        assert!(Arc::ptr_eq(&first, &second.value.unwrap()));
        assert!(second.ttl.unwrap() > Duration::from_secs(55));
        unchanged.assert_async().await;
        unchanged.remove_async().await;
        advance(Duration::from_secs(61)).await;
        let changed = server
            .mock("GET", "/")
            .match_header("if-none-match", "\"one\"")
            .with_body("2")
            .with_header("cache-control", "max-age=0")
            .create_async()
            .await;
        assert_eq!(*client.get(&url).await?.value.unwrap(), 2);
        changed.assert_async().await;
        changed.remove_async().await;
        let no_validator = server
            .mock("GET", "/")
            .match_header("if-none-match", mockito::Matcher::Missing)
            .with_body("3")
            .create_async()
            .await;
        assert_eq!(*client.get(&url).await?.value.unwrap(), 3);
        no_validator.assert_async().await;
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
    #[test_case(Some("no-store"), 0, 0; "no store")]
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
            .with_status(404)
            .with_header("age", &age.to_string())
            .with_body("not JSON")
            .expect(if expected == 0 { 2 } else { 1 });
        if let Some(control) = control {
            mock = mock.with_header("cache-control", control);
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
            assert!(result.ttl.unwrap() <= Duration::from_secs(expected));
            if expected > 0 {
                assert!(result.ttl.unwrap() > Duration::from_secs(expected - 5));
            }
        }
        mock.assert_async().await;
        Ok(())
    }

    const RETRY_DELAY: Duration = Duration::from_secs(30);

    async fn fixture() -> Result<(mockito::ServerGuard, Client<String>, Url)> {
        let server = mockito::Server::new_async().await;
        let url = server.url().parse()?;
        let api = Client::builder()
            .max_retries(0u32)
            .stale_if_error(RETRY_DELAY)
            .build()?;
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
    #[test_case(Some("no-store"), 0, 0; "no store overrides default")]
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
            .with_status(200)
            .with_body(body("empty"))
            .with_header("age", &age.to_string());
        if let Some(control) = control {
            mock = mock.with_header("cache-control", control);
        }
        let mock = mock.create_async().await;
        let result = api.get(&url).await?;
        assert_eq!(result.value.unwrap().as_str(), "empty");
        assert!(result.ttl.unwrap() <= Duration::from_secs(expected));
        if expected > 0 {
            assert!(result.ttl.unwrap() > Duration::from_secs(expected - 5));
        }
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn fallback_ttl_expires_and_is_renewed_by_304() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let api = Client::<String>::builder()
            .max_retries(0u32)
            .default_ttl(Duration::from_secs(90))
            .build()?;
        let url = server.url().parse()?;
        let initial = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("etag", "\"one\"")
            .with_body(body("initial"))
            .expect(1)
            .create_async()
            .await;
        let first = api.get(&url).await?;
        advance(Duration::from_secs(60)).await;
        let cached = api.get(&url).await?;
        assert!(cached.ttl.unwrap() <= Duration::from_secs(30));
        assert!(cached.ttl.unwrap() > Duration::from_secs(25));
        initial.assert_async().await;
        initial.remove_async().await;
        let unchanged = server
            .mock("GET", "/")
            .with_status(304)
            .match_header("if-none-match", "\"one\"")
            .expect(1)
            .create_async()
            .await;
        advance(Duration::from_secs(31)).await;
        let renewed = api.get(&url).await?;
        assert!(Arc::ptr_eq(&first.value.unwrap(), &renewed.value.unwrap()));
        assert!(renewed.ttl.unwrap() <= Duration::from_secs(90));
        assert!(renewed.ttl.unwrap() > Duration::from_secs(85));
        unchanged.assert_async().await;
        Ok(())
    }

    #[test_case(false; "retain cache directives")]
    #[test_case(true; "replace cache directives")]
    #[tokio::test]
    async fn revalidates_etag_and_preserves_snapshot(change_ttl: bool) -> Result<()> {
        let (mut server, api, url) = fixture().await?;
        let initial = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body("initial"))
            .with_header("cache-control", "max-age=600")
            .with_header("age", "500")
            .with_header("etag", "W/\"one\"")
            .create_async()
            .await;
        let old = api.get(&url).await?.value.unwrap();
        initial.assert_async().await;
        initial.remove_async().await;
        advance(Duration::from_secs(101)).await;
        let mut mock = server
            .mock("GET", "/")
            .match_header("if-none-match", "W/\"one\"")
            .with_status(304)
            .with_header("etag", "W/\"two\"");
        if change_ttl {
            mock = mock.with_header("cache-control", "max-age=1200");
        }
        let mock = mock.expect(1).create_async().await;
        let name = url.clone();
        let (first, second) = tokio::try_join!(api.get(&name), api.get(&name))?;
        assert!(Arc::ptr_eq(&old, &first.value.unwrap()));
        assert!(Arc::ptr_eq(&old, &second.value.unwrap()));
        mock.assert_async().await;
        mock.remove_async().await;
        advance(Duration::from_secs(if change_ttl { 1100 } else { 500 })).await;
        assert!(Arc::ptr_eq(&old, &api.get(&url).await?.value.unwrap()));
        advance(Duration::from_secs(101)).await;
        let changed = server
            .mock("GET", "/")
            .match_header("if-none-match", "W/\"two\"")
            .with_status(200)
            .with_body(body("changed"))
            .create_async()
            .await;
        assert_eq!(api.get(&url).await?.value.unwrap().as_str(), "changed");
        assert_eq!(old.as_str(), "initial");
        changed.assert_async().await;
        Ok(())
    }

    #[test_case(500, "failed"; "HTTP failure")]
    #[test_case(200, "invalid"; "invalid JSON")]
    #[tokio::test]
    async fn failed_refresh_retains_snapshot_and_backs_off(
        status: usize,
        body_text: &str,
    ) -> Result<()> {
        let (mut server, api, url) = fixture().await?;
        let initial = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body("old"))
            .with_header("cache-control", "max-age=0")
            .create_async()
            .await;
        let old = api.get(&url).await?.value.unwrap();
        initial.remove_async().await;
        let failed = server
            .mock("GET", "/")
            .with_status(status)
            .with_body(body_text)
            .expect(1)
            .create_async()
            .await;
        for _ in 0..2 {
            let result = api.get(&url).await?;
            assert!(Arc::ptr_eq(&old, &result.value.unwrap()));
            assert!(result.ttl.unwrap() <= RETRY_DELAY);
            assert!(result.ttl.unwrap() > Duration::from_secs(20));
        }
        failed.assert_async().await;
        failed.remove_async().await;
        advance(RETRY_DELAY).await;
        let recovered = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body("new"))
            .create_async()
            .await;
        assert_eq!(api.get(&url).await?.value.unwrap().as_str(), "new");
        recovered.assert_async().await;
        Ok(())
    }

    #[test_case(500, "failed"; "HTTP failure")]
    #[test_case(200, "invalid"; "invalid JSON")]
    #[test_case(304, ""; "unexpected 304")]
    #[tokio::test]
    async fn initial_failure_can_be_retried(status: usize, body_text: &str) -> Result<()> {
        let (mut server, api, url) = fixture().await?;
        let failed = server
            .mock("GET", "/")
            .with_status(status)
            .with_body(body_text)
            .create_async()
            .await;
        assert!(api.get(&url).await.is_err());
        failed.assert_async().await;
        failed.remove_async().await;
        let recovered = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body("empty"))
            .create_async()
            .await;
        assert_eq!(api.get(&url).await?.value.unwrap().as_str(), "empty");
        recovered.assert_async().await;
        Ok(())
    }

    #[test_case(429; "rate limit")]
    #[test_case(500; "server error")]
    #[tokio::test]
    async fn does_not_cache_errors(status: usize) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let failed = server
            .mock("GET", "/")
            .with_status(status)
            .with_header("cache-control", "max-age=3600")
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
            .with_status(200)
            .with_body("[1,2]")
            .expect(1)
            .create_async()
            .await;
        assert_eq!(api.get(&url).await?.value.unwrap().len(), 2);
        available.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn respects_no_store() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("cache-control", "no-store")
            .with_body("[1,2]")
            .expect(2)
            .create_async()
            .await;
        let api = Client::<Vec<u64>>::builder().max_retries(0u32).build()?;
        let url = server.url().parse()?;
        for _ in 0..2 {
            assert_eq!(api.get(&url).await?.value.unwrap().len(), 2);
        }
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn refetches_stale_responses() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let initial = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("cache-control", "max-age=0")
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
            .with_status(200)
            .with_header("cache-control", "max-age=600")
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
            .with_status(503)
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("GET", "/")
            .with_status(200)
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
