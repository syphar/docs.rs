use crate::error::AxumNope;
use anyhow::{Context as _, anyhow};
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::{HeaderMap, request::Parts},
};
use axum_extra::headers::HeaderMapExt;
use docs_rs_headers::{Host, X_FORWARDED_HOST, XForwardedHost};
use http::header::HOST;
use std::net::IpAddr;

/// Extractor for the requested hostname.
///
/// First tries `X-Forwarded-Host`, then `Host`. If neither header is present, returns `None`.
///
/// Use `Option<RequestedHost>` when the header is optional.
/// Use `RequestedHost` when the header is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestedHost(String);

impl RequestedHost {
    pub fn subdomain(&self) -> Option<&str> {
        if self.is_ip_address() || self.0.eq_ignore_ascii_case("localhost") {
            return None;
        }

        let parts = self
            .0
            .split('.')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();

        (parts.len() == 3).then(|| parts[0])
    }

    pub fn is_apex_domain(&self) -> bool {
        !self.is_ip_address()
            && !self.0.eq_ignore_ascii_case("localhost")
            && self.0.split('.').filter(|part| !part.is_empty()).count() == 2
    }

    pub fn is_ip_address(&self) -> bool {
        self.0.trim_matches(['[', ']']).parse::<IpAddr>().is_ok()
    }

    fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, AxumNope> {
        if let Some(header) = headers
            .typed_try_get::<XForwardedHost>()
            .with_context(|| format!("invalid {} header", X_FORWARDED_HOST))
            .map_err(AxumNope::BadRequest)?
            .filter(|h| !h.is_empty())
        {
            Ok(header
                .iter()
                .next()
                .map(|authority| Self(authority.host().to_string())))
        } else if let Some(header) = headers
            .typed_try_get::<Host>()
            .with_context(|| format!("invalid {} header", HOST))
            .map_err(AxumNope::BadRequest)?
        {
            Ok(Some(Self(header.hostname().to_string())))
        } else {
            Ok(None)
        }
    }
}

impl<S> FromRequestParts<S> for RequestedHost
where
    S: Send + Sync,
{
    type Rejection = AxumNope;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extract::<Option<Self>>()
            .await?
            .ok_or_else(|| AxumNope::BadRequest(anyhow!("no X-ForwardedFor or Host header found")))
    }
}

impl<S> OptionalFromRequestParts<S> for RequestedHost
where
    S: Send + Sync,
{
    type Rejection = AxumNope;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        Self::from_headers(&parts.headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HOST;
    use http::{HeaderMap, HeaderValue};
    use test_case::test_case;

    #[test_case("foo.docs.rs", "foo")]
    #[test_case("foo.docs.rs:443", "foo")]
    fn detects_has_subdomain(host: &'static str, expected: &str) {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static(host));

        let host = RequestedHost::from_headers(&headers).unwrap().unwrap();
        assert_eq!(host.subdomain().unwrap(), expected);
    }

    #[test_case("docs.rs")]
    #[test_case("localhost")]
    #[test_case("127.0.0.1:3000")]
    fn detects_no_subdomain(host: &'static str) {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static(host));

        let host = RequestedHost::from_headers(&headers).unwrap().unwrap();
        assert!(host.subdomain().is_none());
    }

    #[test]
    fn takes_host_header_when_forwarded_host_missing() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));

        let extracted = RequestedHost::from_headers(&headers).unwrap().unwrap();
        assert_eq!(extracted.0, "docs.rs");
    }

    #[test]
    fn prefers_x_forwarded_host_over_host() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));
        headers.insert(
            &X_FORWARDED_HOST,
            HeaderValue::from_static("crate.docs.rs, docs.rs"),
        );

        let extracted = RequestedHost::from_headers(&headers).unwrap().unwrap();
        assert_eq!(extracted.0, "crate.docs.rs");
    }

    #[test]
    fn invalid_host_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("bad/host"));

        assert!(RequestedHost::from_headers(&headers).is_err());
    }

    #[test]
    fn empty_host_header_is_err() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static(""));

        assert!(RequestedHost::from_headers(&headers).is_err());
    }

    #[test]
    fn invalid_forwarded_host_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));
        headers.insert(&X_FORWARDED_HOST, HeaderValue::from_static("bad/host"));

        assert!(RequestedHost::from_headers(&headers).is_err());
    }

    #[test]
    fn empty_forwarded_host_header_is_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));
        headers.insert(&X_FORWARDED_HOST, HeaderValue::from_static(""));

        assert_eq!(
            RequestedHost::from_headers(&headers).unwrap().unwrap().0,
            "docs.rs"
        );
    }
}
