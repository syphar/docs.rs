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

    pub(crate) fn from_header_value(header: &HeaderValue) -> Result<Option<Self>, AxumNope> {
        parse_host_header_value(header, "host")
    }

    fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, AxumNope> {
        if let Some(header) = headers.get(&X_FORWARDED_HOST) {
            return Self::from_forwarded_header_value(header);
        }

        if let Some(header) = headers.get(HOST) {
            return Self::from_header_value(header);
        }

        Ok(None)
    }

    fn from_forwarded_header_value(header: &HeaderValue) -> Result<Option<Self>, AxumNope> {
        let value = header
            .to_str()
            .context("invalid x-forwarded-host header")
            .map_err(AxumNope::BadRequest)?;
        let host = value
            .split(',')
            .next()
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| AxumNope::BadRequest(anyhow!("invalid x-forwarded-host header")))?;

        parse_authority(host)
            .context("invalid x-forwarded-host header")
            .map(RequestedHost)
            .map(Some)
            .map_err(AxumNope::BadRequest)
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

fn parse_authority(host: &str) -> Result<Authority, InvalidUri> {
    host.trim_end_matches('.').parse::<Authority>()
}

fn parse_host_header_value(
    header: &HeaderValue,
    header_name: &str,
) -> Result<Option<RequestedHost>, AxumNope> {
    let host = header
        .to_str()
        .with_context(|| format!("invalid {header_name} header"))
        .map_err(AxumNope::BadRequest)?
        .trim();
    if host.is_empty() {
        return Err(AxumNope::BadRequest(anyhow!(
            "invalid {header_name} header"
        )));
    }

    parse_authority(host)
        .with_context(|| format!("invalid {header_name} header"))
        .map(RequestedHost)
        .map(Some)
        .map_err(AxumNope::BadRequest)
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
        let host = RequestedHost::from_header_value(&header).unwrap().unwrap();
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
    fn invalid_forwarded_host_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));
        headers.insert(&X_FORWARDED_HOST, HeaderValue::from_static("bad/host"));

        assert!(RequestedHost::from_headers(&headers).is_err());
    }
}
