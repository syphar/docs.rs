use crate::error::AxumNope;
use anyhow::anyhow;
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::{HeaderValue, header::HOST, request::Parts},
};
use std::{convert::Infallible, net::IpAddr};

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
        Ok(parts.headers.get(HOST).and_then(Self::from_header_value))
    }
}

fn host_uses_subdomain(host: &str) -> bool {
    let host = host_without_port(host.trim()).trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok() {
        return false;
    }

    host.split('.')
        .filter(|segment| !segment.is_empty())
        .count()
        >= 3
}

fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split_once(']').map(|(ip, _)| ip).unwrap_or(host);
    }

    host.split_once(':').map(|(name, _)| name).unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
