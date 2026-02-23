use crate::error::AxumNope;
use anyhow::{Context as _, anyhow};
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::{HeaderMap, HeaderName, HeaderValue, header::HOST, request::Parts},
};
use http::uri::{Authority, InvalidUri};
use std::net::IpAddr;

const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");

// FIXME: use typed `headers::Host`, write our own `headers::XForwardedHost`

/// Extractor for the HTTP Host header.
///
/// Use `Option<RequestedHost>` when the header is optional.
/// Use `RequestedHost` when the header is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestedHost(Authority);

impl RequestedHost {
    pub(crate) fn uses_subdomain(&self) -> bool {
        host_uses_subdomain(self.0.host())
    }

    fn parse_authority(header: &[u8]) -> Result<Option<Self>, InvalidUri> {
        if header.is_empty() || header.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(None);
        }

        let auth: Authority = header.try_into()?;
        Ok(Some(Self(auth)))
    }

    fn from_host_header_value(header: &HeaderValue) -> Result<Option<Self>, InvalidUri> {
        Self::parse_authority(header.as_bytes())
    }

    fn from_forwarded_header_value(header: &HeaderValue) -> Result<Option<Self>, InvalidUri> {
        let header = header.as_bytes();

        let slice = match header.iter().position(|&b| b == b',') {
            Some(pos) => &header[..pos],
            None => &header[..],
        };

        Self::parse_authority(slice)
    }

    fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, AxumNope> {
        if let Some(header) = headers.get(&X_FORWARDED_HOST) {
            Self::from_forwarded_header_value(header)
                .with_context(|| format!("invalid {} header", X_FORWARDED_HOST))
                .map_err(AxumNope::BadRequest)
        } else if let Some(header) = headers.get(HOST) {
            Self::from_host_header_value(header)
                .with_context(|| format!("invalid {} header", X_FORWARDED_HOST))
                .map_err(AxumNope::BadRequest)
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
            .ok_or_else(|| AxumNope::BadRequest(anyhow!("host header not found")))
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

fn host_uses_subdomain(host: &str) -> bool {
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
    use anyhow::Result;
    use http::HeaderMap;
    use test_case::test_case;

    #[test_case(""; "empty")]
    #[test_case("  "; "whitespace only")]
    fn test_parse_authority_empty(host: &'static str) -> Result<()> {
        assert!(RequestedHost::parse_authority(host.as_bytes())?.is_none());
        Ok(())
    }

    #[test_case("docs.rs", "docs.rs")]
    #[test_case("docs.rs:443", "docs.rs")]
    fn test_parse_authority(host: &'static str, expected: &str) -> Result<()> {
        let auth = RequestedHost::parse_authority(host.as_bytes())?.unwrap();
        assert_eq!(auth.0.host(), expected);
        Ok(())
    }

    #[test_case("docs.rs", false)]
    #[test_case("foo.docs.rs", true)]
    #[test_case("foo.docs.rs:443", true)]
    #[test_case("localhost", false)]
    #[test_case("127.0.0.1:3000", false)]
    fn detects_subdomain(host: &str, expected: bool) {
        let header = HeaderValue::from_str(host).unwrap();
        let host = RequestedHost::from_host_header_value(&header)
            .unwrap()
            .unwrap();
        assert_eq!(host.uses_subdomain(), expected);
    }

    #[test]
    fn takes_host_header_when_forwarded_host_missing() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));

        let extracted = RequestedHost::from_headers(&headers).unwrap().unwrap();
        assert_eq!(extracted.0.as_str(), "docs.rs");
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
        assert_eq!(extracted.0.as_str(), "crate.docs.rs");
    }

    #[test]
    fn invalid_host_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("bad/host"));

        assert!(RequestedHost::from_headers(&headers).is_err());
    }

    #[test]
    fn empty_host_header_is_none() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static(""));

        assert!(RequestedHost::from_headers(&headers).unwrap().is_none());
    }

    #[test]
    fn invalid_forwarded_host_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));
        headers.insert(&X_FORWARDED_HOST, HeaderValue::from_static("bad/host"));

        assert!(RequestedHost::from_headers(&headers).is_err());
    }

    #[test]
    fn empty_forwarded_host_header_is_none() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));
        headers.insert(&X_FORWARDED_HOST, HeaderValue::from_static(""));

        assert!(RequestedHost::from_headers(&headers).unwrap().is_none());
    }
}
