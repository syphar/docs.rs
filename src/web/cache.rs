use crate::{
    config::Config,
    web::headers::{SURROGATE_CONTROL, SURROGATE_KEY, SurrogateKey, SurrogateKeys},
};
use axum::{
    Extension,
    extract::{FromRequestParts, Request as AxumHttpRequest},
    middleware::Next,
    response::Response as AxumResponse,
};
use axum_extra::headers::HeaderMapExt as _;
use http::{
    HeaderMap, HeaderName, HeaderValue, StatusCode,
    header::{CACHE_CONTROL, ETAG},
    request::Parts,
};
use std::{convert::Infallible, sync::Arc};

pub const X_RLNG_SOURCE_CDN: HeaderName = HeaderName::from_static("x-rlng-source-cdn");

/// a surrogate key that is attached to _all_ content.
/// This enables us to use the fastly "soft purge" for everything.
pub const SURROGATE_KEY_ALL: SurrogateKey = SurrogateKey::from_static("all");

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseCacheHeaders {
    pub cache_control: Option<HeaderValue>,
    pub surrogate_control: Option<HeaderValue>,
    pub surrogate_keys: Option<SurrogateKeys>,
    pub needs_cdn_invalidation: bool,
}

impl ResponseCacheHeaders {
    fn set_on_response(&self, headers: &mut HeaderMap) {
        if let Some(ref cache_control) = self.cache_control {
            headers.insert(CACHE_CONTROL, cache_control.clone());
        }
        if let Some(ref surrogate_control) = self.surrogate_control {
            headers.insert(&SURROGATE_CONTROL, surrogate_control.clone());
        }
        if let Some(ref surrogate_keys) = self.surrogate_keys {
            headers.typed_insert(surrogate_keys.clone());
        }
    }
}

/// No caching in the CDN & in the browser.
/// Browser & CDN often still store the file,
/// but then always revalidate using `If-Modified-Since` (with last modified)
/// or `If-None-Match` (with etag).
/// Browser might still sometimes use cached content, for example when using
/// the "back" button.
pub static NO_CACHING: ResponseCacheHeaders = ResponseCacheHeaders {
    cache_control: Some(HeaderValue::from_static("max-age=0")),
    surrogate_control: None,
    surrogate_keys: None,
    needs_cdn_invalidation: false,
};

/// Cache for a short time in the browser & in the CDN.
/// Helps protecting against traffic spikes.
pub static SHORT: ResponseCacheHeaders = ResponseCacheHeaders {
    cache_control: Some(HeaderValue::from_static("public, max-age=60")),
    surrogate_control: None,
    surrogate_keys: None,
    needs_cdn_invalidation: false,
};

/// don't cache, don't even store. Never. Ever.
pub static NO_STORE_MUST_REVALIDATE: ResponseCacheHeaders = ResponseCacheHeaders {
    cache_control: Some(HeaderValue::from_static(
        "no-cache, no-store, must-revalidate, max-age=0",
    )),
    surrogate_control: None,
    surrogate_keys: None,
    needs_cdn_invalidation: false,
};

pub static FOREVER_IN_FASTLY_CDN: ResponseCacheHeaders = ResponseCacheHeaders {
    // explicitly forbid browser caching, same as NO_CACHING above.
    cache_control: Some(HeaderValue::from_static("max-age=0")),

    // set `surrogate-control`, cache forever in the CDN
    // https://www.fastly.com/documentation/reference/http/http-headers/Surrogate-Control/
    //
    // TODO: evaluate if we can / should set `stale-while-revalidate` or `stale-if-error` here,
    // especially in combination with our fastly compute service.
    // https://www.fastly.com/documentation/guides/concepts/edge-state/cache/stale/
    surrogate_control: Some(HeaderValue::from_static("max-age=31536000")),
    surrogate_keys: None,

    needs_cdn_invalidation: true,
};

pub static FOREVER_IN_CLOUDFRONT_CDN: ResponseCacheHeaders = ResponseCacheHeaders {
    // A missing `max-age` or `s-maxage` in the Cache-Control header will lead to
    // CloudFront using the default TTL, while the browser not seeing any caching header.
    //
    // Default TTL is set here:
    // https://github.com/rust-lang/simpleinfra/blob/becf4532a10a7a218aedb34d4648ecb73e61f5fd/terraform/docs-rs/cloudfront.tf#L24
    //
    // This means we can have the CDN caching the documentation while just
    // issuing a purge after a build.
    // https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/Expiration.html#ExpirationDownloadDist
    //
    // There might be edge cases where browsers add caching based on arbitraty heuristics
    // when `Cache-Control` is missing.
    cache_control: None,
    surrogate_control: None,
    surrogate_keys: None,
    needs_cdn_invalidation: true,
};

/// cache forever in browser & CDN.
/// Only usable for content with unique filenames.
///
/// We use this policy mostly for static files, rustdoc toolchain assets,
/// or build assets.
pub static FOREVER_IN_CDN_AND_BROWSER: ResponseCacheHeaders = ResponseCacheHeaders {
    cache_control: Some(HeaderValue::from_static(
        "public, max-age=31104000, immutable",
    )),
    surrogate_control: None,
    surrogate_keys: None,
    needs_cdn_invalidation: false,
};

#[derive(Debug, Copy, Clone, PartialEq)]
#[cfg_attr(test, derive(strum::EnumIter))]
pub enum TargetCdn {
    Fastly,
    CloudFront,
}

impl<S> FromRequestParts<S> for TargetCdn
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if parts.headers.contains_key(X_RLNG_SOURCE_CDN) {
            Ok(TargetCdn::Fastly)
        } else {
            Ok(TargetCdn::CloudFront)
        }
    }
}

/// defines the wanted caching behaviour for a web response.
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(strum::EnumIter))]
pub enum CachePolicy {
    /// no browser or CDN caching.
    /// In some cases the browser might still use cached content,
    /// for example when using the "back" button or when it can't
    /// connect to the server.
    NoCaching,
    /// don't cache, plus
    /// * enforce revalidation
    /// * never store
    NoStoreMustRevalidate,
    /// cache for a short time in the browser & CDN.
    /// right now: one minute.
    /// Can be used when the content can be a _little_ outdated,
    /// while protecting against spikes in traffic.
    ShortInCdnAndBrowser,
    /// cache forever in browser & CDN.
    /// Valid when you have hashed / versioned filenames and every rebuild would
    /// change the filename.
    ForeverInCdnAndBrowser,
    /// cache forever in CDN, but not in the browser.
    /// Since we control the CDN we can actively purge content that is cached like
    /// this, for example after building a crate.
    /// Note: The CDN (Fastly) needs a list of surrogate keys ( = tags )to be able to purge a
    /// subset of the pages
    /// Example usage: `/latest/` rustdoc pages and their redirects.
    ForeverInCdn(SurrogateKeys),
    /// cache forever in the CDN, but allow stale content in the browser.
    /// Note: The CDN (Fastly) needs a list of surrogate keys ( = tags )to be able to purge a
    /// subset of the pages
    /// Example: rustdoc pages with the version in their URL.
    /// A browser will show the stale content while getting the up-to-date
    /// version from the origin server in the background.
    /// This helps building a PWA.
    ForeverInCdnAndStaleInBrowser(SurrogateKeys),
}

impl CachePolicy {
    pub fn render(&self, config: &Config, target_cdn: TargetCdn) -> ResponseCacheHeaders {
        let mut headers = match *self {
            CachePolicy::NoCaching => NO_CACHING.clone(),
            CachePolicy::NoStoreMustRevalidate => NO_STORE_MUST_REVALIDATE.clone(),
            CachePolicy::ShortInCdnAndBrowser => SHORT.clone(),
            CachePolicy::ForeverInCdnAndBrowser => FOREVER_IN_CDN_AND_BROWSER.clone(),
            CachePolicy::ForeverInCdn(ref surrogate_keys) => {
                if config.cache_invalidatable_responses {
                    let mut policy = match target_cdn {
                        TargetCdn::Fastly => FOREVER_IN_FASTLY_CDN.clone(),
                        TargetCdn::CloudFront => FOREVER_IN_CLOUDFRONT_CDN.clone(),
                    };
                    debug_assert!(policy.surrogate_keys.is_none());
                    policy.surrogate_keys = Some(surrogate_keys.clone());
                    policy
                } else {
                    NO_CACHING.clone()
                }
            }
            CachePolicy::ForeverInCdnAndStaleInBrowser(ref surrogate_keys) => {
                // when caching invalidatable responses is disabled, this results in NO_CACHING
                let mut forever_in_cdn =
                    CachePolicy::ForeverInCdn(surrogate_keys.clone()).render(config, target_cdn);

                if config.cache_invalidatable_responses
                    && let Some(cache_control) =
                        config.cache_control_stale_while_revalidate.map(|seconds| {
                            format!("stale-while-revalidate={seconds}")
                                .parse::<HeaderValue>()
                                .unwrap()
                        })
                {
                    forever_in_cdn.cache_control = Some(cache_control);
                }

                forever_in_cdn
            }
        };

        headers
            .surrogate_keys
            .get_or_insert_default()
            .try_extend([SURROGATE_KEY_ALL])
            .unwrap();

        headers
    }
}

pub(crate) async fn cache_middleware(
    Extension(config): Extension<Arc<Config>>,
    target_cdn: TargetCdn,
    req: AxumHttpRequest,
    next: Next,
) -> AxumResponse {
    let mut response = next.run(req).await;

    debug_assert!(
        !(response
            .headers()
            .keys()
            .any(|h| { h == CACHE_CONTROL || h == SURROGATE_CONTROL || h == SURROGATE_KEY })),
        "handlers should never set their own caching headers and only use CachePolicy to control caching. \n{:?}",
        response.headers(),
    );

    debug_assert!(
        response.status() == StatusCode::NOT_MODIFIED
            || response.status().is_success()
            || !response.headers().contains_key(ETAG),
        "only successful or not-modified responses should have etags. \n{:?}\n{:?}",
        response.status(),
        response.headers(),
    );

    // extract cache policy, default to "forbid caching everywhere".
    // We only use cache policies in our successful responses (with content, or redirect),
    // so any errors (4xx, 5xx) should always get "NoCaching".
    let cache_policy = response
        .extensions()
        .get::<CachePolicy>()
        .unwrap_or(&CachePolicy::NoCaching);

    let cache_headers = cache_policy.render(&config, target_cdn);

    debug_assert!(
        target_cdn == TargetCdn::Fastly || cache_headers.surrogate_control.is_none(),
        "Surrogate-Control header is only supported by Fastly, but got Surrogate-Control header for CDN: {:?}\n{:?}",
        target_cdn,
        cache_headers,
    );

    cache_headers.set_on_response(response.headers_mut());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::{
        TestEnvironment,
        headers::{test_typed_decode, test_typed_encode},
    };
    use anyhow::{Context as _, Result};
    use axum_extra::headers::CacheControl;
    use itertools::Itertools as _;
    use strum::IntoEnumIterator as _;
    use test_case::{test_case, test_matrix};

    fn validate_cache_control(value: &HeaderValue) -> Result<()> {
        assert!(!value.as_bytes().is_empty());

        // first parse attempt.
        // The `CacheControl` typed header impl will just skip over unknown directives.
        let parsed: CacheControl = test_typed_decode(value.clone())?.unwrap();

        // So we just re-render it, re-parse and compare both.
        let re_rendered = test_typed_encode(parsed.clone());
        let re_parsed: CacheControl = test_typed_decode(re_rendered)?.unwrap();

        assert_eq!(parsed, re_parsed);

        Ok(())
    }

    #[test]
    fn test_const_response_consistency() {
        assert_eq!(
            FOREVER_IN_FASTLY_CDN.cache_control,
            NO_CACHING.cache_control
        );
        assert!(FOREVER_IN_CLOUDFRONT_CDN.cache_control.is_none());
    }

    #[test_matrix(
        [true, false],
        [Some(86400), None]
    )]
    fn test_validate_header_syntax_for_all_possible_combinations(
        cache_invalidatable_responses: bool,
        stale_while_revalidate: Option<u32>,
    ) -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_invalidatable_responses(cache_invalidatable_responses)
            .cache_control_stale_while_revalidate(stale_while_revalidate)
            .build()?;

        for (policy, target_cdn) in CachePolicy::iter().cartesian_product(TargetCdn::iter()) {
            let headers = policy.render(&config, target_cdn);

            if let Some(cache_control) = headers.cache_control {
                validate_cache_control(&cache_control).with_context(|| {
                    format!(
                        "couldn't validate Cache-Control header syntax for policy {:?}, CDN: {:?}",
                        policy, target_cdn,
                    )
                })?;
            }

            if let Some(surrogate_control) = headers.surrogate_control {
                validate_cache_control(&surrogate_control).with_context(|| {
                    format!(
                        "couldn't validate Surrogate-Control header syntax for policy {:?}, CDN: {:?}",
                        policy,
                        target_cdn,
                    )
                })?;
            }
        }
        Ok(())
    }

    #[test_case(CachePolicy::NoCaching, Some("max-age=0"))]
    #[test_case(
        CachePolicy::NoStoreMustRevalidate,
        Some("no-cache, no-store, must-revalidate, max-age=0")
    )]
    #[test_case(
        CachePolicy::ForeverInCdnAndBrowser,
        Some("public, max-age=31104000, immutable")
    )]
    fn test_render(cache: CachePolicy, cache_control: Option<&str>) -> Result<()> {
        let config = TestEnvironment::base_config().build()?;
        let headers = cache.render(&config, TargetCdn::CloudFront);

        assert_eq!(
            headers.cache_control,
            cache_control.map(|s| HeaderValue::from_str(s).unwrap())
        );

        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test]
    fn test_render_cache_in_cdn_with_surrogate_keys() -> Result<()> {
        let config = TestEnvironment::base_config().build()?;
        let headers = CachePolicy::ForeverInCdn(SurrogateKey::from_static("something").into())
            .render(&config, TargetCdn::CloudFront);

        assert!(headers.cache_control.is_none());
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test]
    fn test_render_cache_in_cdn_stale_browser_with_surrogate_keys() -> Result<()> {
        let config = TestEnvironment::base_config().build()?;
        let headers = CachePolicy::ForeverInCdnAndStaleInBrowser(
            SurrogateKey::from_static("something").into(),
        )
        .render(&config, TargetCdn::CloudFront);

        assert_eq!(
            headers.cache_control,
            Some(HeaderValue::from_static("stale-while-revalidate=86400"))
        );
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test]
    fn render_stale_without_config() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_control_stale_while_revalidate(None)
            .build()?;

        let headers = CachePolicy::ForeverInCdnAndStaleInBrowser(
            SurrogateKey::from_static("something").into(),
        )
        .render(&config, TargetCdn::CloudFront);
        assert!(headers.cache_control.is_none());
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test]
    fn render_stale_with_config() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_control_stale_while_revalidate(Some(666))
            .build()?;

        let headers = CachePolicy::ForeverInCdnAndStaleInBrowser(
            SurrogateKey::from_static("something").into(),
        )
        .render(&config, TargetCdn::CloudFront);
        assert_eq!(headers.cache_control.unwrap(), "stale-while-revalidate=666");
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test]
    fn render_forever_in_cdn_disabled() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_invalidatable_responses(false)
            .build()?;

        let headers = CachePolicy::ForeverInCdn(SurrogateKey::from_static("something").into())
            .render(&config, TargetCdn::CloudFront);
        assert_eq!(headers.cache_control.unwrap(), "max-age=0");
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test]
    fn render_forever_in_cdn_or_stale_disabled() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_invalidatable_responses(false)
            .build()?;

        let headers = CachePolicy::ForeverInCdnAndStaleInBrowser(
            SurrogateKey::from_static("something").into(),
        )
        .render(&config, TargetCdn::CloudFront);
        assert_eq!(headers.cache_control.unwrap(), "max-age=0");
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test_case(CachePolicy::NoCaching, Some("max-age=0"), None)]
    #[test_case(
        CachePolicy::NoStoreMustRevalidate,
        Some("no-cache, no-store, must-revalidate, max-age=0"),
        None
    )]
    #[test_case(
        CachePolicy::ForeverInCdnAndBrowser,
        Some("public, max-age=31104000, immutable"),
        None
    )]
    fn render_fastly(
        cache: CachePolicy,
        cache_control: Option<&str>,
        surrogate_control: Option<&str>,
    ) -> Result<()> {
        let config = TestEnvironment::base_config().build()?;
        let headers = cache.render(&config, TargetCdn::Fastly);

        assert_eq!(
            headers.cache_control,
            cache_control.map(|s| HeaderValue::from_str(s).unwrap())
        );

        assert_eq!(
            headers.surrogate_control,
            surrogate_control.map(|s| HeaderValue::from_str(s).unwrap())
        );

        Ok(())
    }

    #[test]
    fn render_fastly_forever_in_cdn() -> Result<()> {
        let config = TestEnvironment::base_config().build()?;
        let headers = CachePolicy::ForeverInCdn(SurrogateKey::from_static("something").into())
            .render(&config, TargetCdn::Fastly);

        assert_eq!(
            headers.cache_control,
            Some(HeaderValue::from_static("max-age=0"))
        );

        assert_eq!(
            headers.surrogate_control,
            Some(HeaderValue::from_static("max-age=31536000"))
        );

        Ok(())
    }

    #[test]
    fn render_fastly_forever_in_cdn_stale_in_browser() -> Result<()> {
        let config = TestEnvironment::base_config().build()?;
        let headers = CachePolicy::ForeverInCdnAndStaleInBrowser(
            SurrogateKey::from_static("something").into(),
        )
        .render(&config, TargetCdn::Fastly);

        assert_eq!(
            headers.cache_control,
            Some(HeaderValue::from_static("stale-while-revalidate=86400"))
        );
        assert_eq!(
            headers.surrogate_control,
            Some(HeaderValue::from_static("max-age=31536000"))
        );

        Ok(())
    }

    #[test]
    fn render_stale_without_config_fastly() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_control_stale_while_revalidate(None)
            .build()?;

        let sk = SurrogateKey::from_static("something");
        let mut headers = CachePolicy::ForeverInCdnAndStaleInBrowser(sk.clone().into())
            .render(&config, TargetCdn::Fastly);

        assert_eq!(
            headers.surrogate_keys.take().unwrap(),
            SurrogateKeys::try_from_iter([sk, SURROGATE_KEY_ALL]).unwrap()
        );
        assert_eq!(headers, FOREVER_IN_FASTLY_CDN);

        Ok(())
    }

    #[test]
    fn render_stale_with_config_fastly() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_control_stale_while_revalidate(Some(666))
            .build()?;

        let headers = CachePolicy::ForeverInCdnAndStaleInBrowser(
            SurrogateKey::from_static("something").into(),
        )
        .render(&config, TargetCdn::Fastly);
        assert_eq!(headers.cache_control.unwrap(), "stale-while-revalidate=666");
        assert_eq!(
            headers.surrogate_control,
            FOREVER_IN_FASTLY_CDN.surrogate_control
        );

        Ok(())
    }

    #[test]
    fn render_forever_in_cdn_disabled_fastly() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_invalidatable_responses(false)
            .build()?;

        let headers = CachePolicy::ForeverInCdn(SurrogateKey::from_static("something").into())
            .render(&config, TargetCdn::Fastly);
        assert_eq!(headers.cache_control.unwrap(), "max-age=0");
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }

    #[test]
    fn render_forever_in_cdn_or_stale_disabled_fastly() -> Result<()> {
        let config = TestEnvironment::base_config()
            .cache_invalidatable_responses(false)
            .build()?;

        let headers = CachePolicy::ForeverInCdnAndStaleInBrowser(
            SurrogateKey::from_static("something").into(),
        )
        .render(&config, TargetCdn::Fastly);
        assert_eq!(headers.cache_control.unwrap(), "max-age=0");
        assert!(headers.surrogate_control.is_none());

        Ok(())
    }
}
