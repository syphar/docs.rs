use crate::error::AxumNope;
use anyhow::anyhow;
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::{HeaderMap, HeaderName, HeaderValue, header::HOST, request::Parts},
};
use http::uri::Authority;
use std::{convert::Infallible, net::IpAddr};

const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");

/// Extractor for the HTTP Host header.
///
/// Use `Option<RequestedHost>` when the header is optional.
/// Use `RequestedHost` when the header is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestedHost(String);

impl RequestedHost {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn uses_subdomain(&self) -> bool {
        host_uses_subdomain(self.as_str())
    }

    pub(crate) fn from_header_value(header: &HeaderValue) -> Option<Self> {
        header
            .to_str()
            .ok()
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(|host| Self(host.to_string()))
    }

    fn from_headers(headers: &HeaderMap) -> Option<Self> {
        headers
            .get(&X_FORWARDED_HOST)
            .and_then(Self::from_forwarded_header_value)
            .or_else(|| headers.get(HOST).and_then(Self::from_header_value))
    }

    fn from_forwarded_header_value(header: &HeaderValue) -> Option<Self> {
        header
            .to_str()
            .ok()
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(|host| Self(host.to_string()))
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
            .await
            .expect("infallible extractor")
            .ok_or_else(|| AxumNope::BadRequest(anyhow!("host header not found or invalid")))
    }
}

impl<S> OptionalFromRequestParts<S> for RequestedHost
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers))
    }
}

fn host_uses_subdomain(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.');
    let authority = match host.parse::<Authority>() {
        Ok(authority) => authority,
        Err(_) => return false,
    };
    let host = authority.host();

    if host.eq_ignore_ascii_case("localhost") || host_as_ip_addr(host).is_some() {
        return false;
    }

    host.split('.')
        .filter(|segment| !segment.is_empty())
        .count()
        >= 3
}

fn host_as_ip_addr(host: &str) -> Option<IpAddr> {
    host.trim_matches(['[', ']']).parse::<IpAddr>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;
    use test_case::test_case;

    #[test_case("docs.rs", false)]
    #[test_case("foo.docs.rs", true)]
    #[test_case("foo.docs.rs:443", true)]
    #[test_case("localhost", false)]
    #[test_case("127.0.0.1:3000", false)]
    fn detects_subdomain(host: &str, expected: bool) {
        let header = HeaderValue::from_str(host).unwrap();
        let host = RequestedHost::from_header_value(&header).unwrap();
        assert_eq!(host.uses_subdomain(), expected);
    }

    #[test]
    fn takes_host_header_when_forwarded_host_missing() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));

        let extracted = RequestedHost::from_headers(&headers).unwrap();
        assert_eq!(extracted.as_str(), "docs.rs");
    }

    #[test]
    fn prefers_x_forwarded_host_over_host() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));
        headers.insert(
            &X_FORWARDED_HOST,
            HeaderValue::from_static("crate.docs.rs, docs.rs"),
        );

        let extracted = RequestedHost::from_headers(&headers).unwrap();
        assert_eq!(extracted.as_str(), "crate.docs.rs");
    }
}
