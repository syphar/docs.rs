use headers::{Age, CacheControl, HeaderMapExt};
use http::HeaderMap;
use std::time::Duration;

/// Remaining freshness from `Cache-Control: max-age` and the response's `Age`.
/// Returns zero for `no-cache` or `no-store`, and `None` when no usable
/// `max-age` is available, leaving the fallback policy to the caller.
pub fn response_ttl(headers: &HeaderMap) -> Option<Duration> {
    cache_control_ttl(
        headers.typed_get::<CacheControl>().as_ref(),
        headers
            .typed_get::<Age>()
            .map(Duration::from)
            .unwrap_or_default(),
    )
}

/// Compute freshness using parsed cache directives, which may have been retained
/// from an earlier response when a 304 omits `Cache-Control`.
/// This interprets `max-age`, `no-cache`, and `no-store`; it is not a full HTTP
/// cache policy evaluator. Missing `max-age` returns `None`.
pub fn cache_control_ttl(control: Option<&CacheControl>, age: Duration) -> Option<Duration> {
    if control.is_some_and(|control| control.no_cache() || control.no_store()) {
        return Some(Duration::ZERO);
    }
    control
        .and_then(CacheControl::max_age)
        .map(|ttl| ttl.saturating_sub(age))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("max-age=600", 0, Some(600); "max age")]
    #[test_case("public, max-age=600", 86, Some(514); "subtract age")]
    #[test_case("max-age=600", 700, Some(0); "already stale")]
    #[test_case("no-store, max-age=600", 0, Some(0); "no store")]
    #[test_case("no-cache, max-age=600", 0, Some(0); "no cache")]
    #[test_case("max-age=0", 0, Some(0); "zero")]
    #[test_case("", 0, None; "missing max age")]
    #[test_case("max-age=invalid", 0, None; "invalid max age")]
    fn parses_ttl(control: &str, age: u64, seconds: Option<u64>) {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CACHE_CONTROL, control.parse().unwrap());
        headers.typed_insert(Age::from_secs(age));
        let expected = seconds.map(Duration::from_secs);
        assert_eq!(response_ttl(&headers), expected);
        assert_eq!(
            cache_control_ttl(
                headers.typed_get::<CacheControl>().as_ref(),
                Duration::from_secs(age)
            ),
            expected
        );
    }

    #[test]
    fn absent_headers_leave_fallback_to_caller() {
        assert_eq!(response_ttl(&HeaderMap::new()), None);
    }

    #[test]
    fn handles_multiple_header_values_and_absent_age() {
        let mut headers = HeaderMap::new();
        headers.append(http::header::CACHE_CONTROL, "public".parse().unwrap());
        headers.append(http::header::CACHE_CONTROL, "max-age=600".parse().unwrap());
        assert_eq!(response_ttl(&headers), Some(Duration::from_secs(600)));
        headers.append(http::header::CACHE_CONTROL, "no-store".parse().unwrap());
        assert_eq!(response_ttl(&headers), Some(Duration::ZERO));
    }
}
