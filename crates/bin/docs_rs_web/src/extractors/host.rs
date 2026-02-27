use crate::error::AxumNope;
use anyhow::{Context as _, anyhow};
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::{HeaderMap, request::Parts, uri::Authority, uri::Scheme},
};
use axum_extra::headers::HeaderMapExt;
use docs_rs_headers::{Host, X_FORWARDED_HOST, X_FORWARDED_PROTO, XForwardedHost, XForwardedProto};
use http::header::HOST;
use serde::Serialize;
use std::net::IpAddr;
use std::str::FromStr;

/// Extractor for the requested hostname.
///
/// First tries `X-Forwarded-Host`, then `Host`. If neither header is present, returns `None`.
///
/// Use `Option<RequestedHost>` when the header is optional.
/// Use `RequestedHost` when the header is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum RequestedHost {
    IPAddr(IpAddr),
    ApexDomain(String),
    SubDomain(String, String),
}

impl RequestedHost {
    pub fn subdomain(&self) -> Option<&str> {
        match self {
            Self::SubDomain(subdomain, _) => Some(subdomain),
            _ => None,
        }
    }

    pub(crate) fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, AxumNope> {
        requested_authority(headers).and_then(|authority| {
            authority
                .map(|authority| authority.host().parse().map_err(AxumNope::BadRequest))
                .transpose()
        })
    }
}

pub(crate) fn requested_authority(headers: &HeaderMap) -> Result<Option<Authority>, AxumNope> {
    if let Some(header) = headers
        .typed_try_get::<XForwardedHost>()
        .with_context(|| format!("invalid {} header", X_FORWARDED_HOST))
        .map_err(AxumNope::BadRequest)?
        .filter(|h| !h.is_empty())
    {
        Ok(header.iter().next().cloned())
    } else if let Some(header) = headers
        .typed_try_get::<Host>()
        .with_context(|| format!("invalid {} header", HOST))
        .map_err(AxumNope::BadRequest)?
    {
        let host = header.hostname();
        let authority = if let Some(port) = header.port() {
            format!("{host}:{port}")
        } else {
            host.to_string()
        };
        Ok(Some(authority.parse().map_err(|err| {
            AxumNope::BadRequest(anyhow!("invalid {HOST} header authority: {err}"))
        })?))
    } else {
        Ok(None)
    }
}

pub(crate) fn requested_scheme(headers: &HeaderMap) -> Result<Option<Scheme>, AxumNope> {
    headers
        .typed_try_get::<XForwardedProto>()
        .with_context(|| format!("invalid {} header", X_FORWARDED_PROTO))
        .map_err(AxumNope::BadRequest)
        .map(|proto| proto.map(|proto| proto.proto().to_owned()))
}

impl FromStr for RequestedHost {
    type Err = anyhow::Error;

    fn from_str(host: &str) -> Result<Self, Self::Err> {
        let host = host.trim().trim_matches('.');

        if host.is_empty() {
            return Err(anyhow!("host is empty"));
        }

        if let Ok(ip_addr) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            return Ok(Self::IPAddr(ip_addr));
        }

        if host.eq_ignore_ascii_case("localhost") {
            return Ok(Self::ApexDomain(host.to_string()));
        } else if let Some((subdomain, host)) = host.rsplit_once('.')
            && host.eq_ignore_ascii_case("localhost")
        {
            return Ok(Self::SubDomain(subdomain.to_string(), host.to_string()));
        }

        let mut dots = host.rmatch_indices('.').map(|(i, _)| i);

        Ok(if let Some(sep) = dots.nth(1) {
            Self::SubDomain(host[0..sep].to_string(), host[sep + 1..].to_string())
        } else {
            Self::ApexDomain(host.to_string())
        })
    }
}

impl<S> FromRequestParts<S> for RequestedHost
where
    S: Send + Sync,
{
    type Rejection = AxumNope;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extract::<Option<Self>>().await?.ok_or_else(|| {
            AxumNope::BadRequest(anyhow!("no X-Forwarded-Host or Host header found"))
        })
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
    use http::{HeaderMap, HeaderValue};
    use test_case::test_case;

    #[test_case("foo.docs.rs", RequestedHost::SubDomain("foo".to_string(), "docs.rs".to_string()))]
    #[test_case("foo.bar.docs.rs", RequestedHost::SubDomain("foo.bar".to_string(), "docs.rs".to_string()))]
    #[test_case("foo.docs.rs:443", RequestedHost::SubDomain("foo".to_string(), "docs.rs".to_string()))]
    #[test_case("docs.rs", RequestedHost::ApexDomain("docs.rs".to_string()))]
    #[test_case("localhost", RequestedHost::ApexDomain("localhost".to_string()))]
    #[test_case("127.0.0.1:3000", RequestedHost::IPAddr("127.0.0.1".parse().unwrap()))]
    #[test_case("[::1]:3000", RequestedHost::IPAddr("::1".parse().unwrap()))]
    fn classifies_host(host: &'static str, expected: RequestedHost) {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static(host));

        let host = RequestedHost::from_headers(&headers).unwrap().unwrap();
        assert_eq!(host, expected);
    }

    #[test]
    fn takes_host_header_when_forwarded_host_missing() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("docs.rs"));

        let extracted = RequestedHost::from_headers(&headers).unwrap().unwrap();
        assert_eq!(extracted, RequestedHost::ApexDomain("docs.rs".to_string()));
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
        assert_eq!(
            extracted,
            RequestedHost::SubDomain("crate".to_string(), "docs.rs".to_string())
        );
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
            RequestedHost::from_headers(&headers).unwrap().unwrap(),
            RequestedHost::ApexDomain("docs.rs".to_string())
        );
    }
}
