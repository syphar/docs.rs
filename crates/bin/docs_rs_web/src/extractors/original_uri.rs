use crate::{Config, error::AxumNope, extractors::host::requested_authority};
use anyhow::Context as _;
use axum::{
    Extension, RequestPartsExt,
    extract::{FromRequestParts, OriginalUri},
    http::{Uri, request::Parts},
};
use docs_rs_uri::EscapedURI;
use http::HeaderMap;
use std::{net::IpAddr, ops::Deref, sync::Arc};

/// Extractor for the original URI enriched with request origin data.
///
/// Uses axum's `OriginalUri` and augments it with scheme and authority from
/// forwarded/host headers, preserving original host and port.
#[derive(Debug, Clone)]
pub(crate) struct OriginalUriWithHost(pub(crate) EscapedURI);

impl Deref for OriginalUriWithHost {
    type Target = EscapedURI;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl OriginalUriWithHost {
    pub(crate) fn apex_domain(&self) -> &str {
        // FIXME: store this on the struct to safe time
        let host = self.0.host().expect("missing host in original uri");

        split_subdomain_from_host(host)
            .map(|(_subdomain, apex_domain)| apex_domain)
            .unwrap_or(host)
    }

    pub(crate) fn subdomain(&self) -> Option<&str> {
        // FIXME: store this on the struct to safe time

        split_subdomain_from_host(self.0.host()?).map(|(subdomain, _)| subdomain)
    }
}

impl<S> FromRequestParts<S> for OriginalUriWithHost
where
    S: Send + Sync,
{
    type Rejection = AxumNope;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let original_uri = parts
            .extract::<OriginalUri>()
            .await
            .expect("infallible extractor");

        let Extension(config) = parts
            .extract::<Extension<Arc<Config>>>()
            .await
            .context("could not extract config extension")?;

        let uri = fill_request_origin(original_uri.0, &config, &parts.headers)?;

        Ok(Self(EscapedURI::from_uri(uri)))
    }
}

fn fill_request_origin(uri: Uri, config: &Config, headers: &HeaderMap) -> Result<Uri, AxumNope> {
    let Some(authority) = requested_authority(headers)? else {
        return Ok(uri);
    };

    let mut parts = uri.into_parts();
    parts.authority = Some(authority);
    parts.scheme = Some(config.default_url_scheme.clone());

    Ok(Uri::from_parts(parts).expect("scheme and authority are set together"))
}

pub(crate) fn split_subdomain_from_host(host: &str) -> Option<(&str, &str)> {
    let host = host.trim().trim_matches('.');

    if host.is_empty() {
        return None;
    }

    if let Ok(_ip_addr) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return None;
    }

    if let Some((subdomain, host)) = host.rsplit_once('.')
        && host.eq_ignore_ascii_case("localhost")
    {
        return Some((subdomain, host));
    }

    let mut dots = host.rmatch_indices('.').map(|(i, _)| i);

    if let Some(sep) = dots.nth(1) {
        Some((&host[0..sep], &host[sep + 1..]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{AxumResponseTestExt, AxumRouterTestExt};
    use axum::{Router, routing::get};
    use docs_rs_headers::X_FORWARDED_HOST;
    use http::header::HOST;

    #[tokio::test]
    async fn enriches_with_host_header() -> anyhow::Result<()> {
        let app = Router::new().route(
            "/hello",
            get(|uri: OriginalUriWithHost| async move { uri.0.to_string() }),
        );

        let res = app
            .get_with_headers("/hello", |h| {
                h.insert(HOST, "docs.rs".parse().unwrap());
            })
            .await?;

        assert_eq!(res.status(), http::StatusCode::OK);
        assert_eq!(res.text().await?, "http://docs.rs/hello");
        Ok(())
    }

    #[tokio::test]
    async fn enriches_with_forwarded_host_scheme_and_port() -> anyhow::Result<()> {
        let app = Router::new().route(
            "/hello",
            get(|uri: OriginalUriWithHost| async move { uri.0.to_string() }),
        );

        let res = app
            .get_with_headers("/hello", |h| {
                h.insert(HOST, "internal.docs.rs:3000".parse().unwrap());
                h.insert(&X_FORWARDED_HOST, "docs.rs:8443".parse().unwrap());
            })
            .await?;

        assert_eq!(res.status(), http::StatusCode::OK);
        assert_eq!(res.text().await?, "https://docs.rs:8443/hello");
        Ok(())
    }
}
