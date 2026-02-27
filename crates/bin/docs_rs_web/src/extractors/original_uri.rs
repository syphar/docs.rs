use crate::{
    error::AxumNope,
    extractors::host::{requested_authority, requested_scheme},
};
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, OriginalUri},
    http::{Uri, request::Parts, uri::Scheme},
};
use http::HeaderMap;

/// Extractor for the original URI enriched with request origin data.
///
/// Uses axum's `OriginalUri` and augments it with scheme and authority from
/// forwarded/host headers, preserving original host and port.
#[derive(Debug, Clone)]
pub(crate) struct OriginalUriWithHost(pub(crate) Uri);

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

        Ok(Self(fill_request_origin(original_uri.0, &parts.headers)?))
    }
}

fn fill_request_origin(uri: Uri, headers: &HeaderMap) -> Result<Uri, AxumNope> {
    let Some(authority) = requested_authority(headers)? else {
        return Ok(uri);
    };

    let mut parts = uri.into_parts();
    parts.authority = Some(authority);
    parts.scheme = Some(requested_scheme(headers)?.unwrap_or(Scheme::HTTP));

    Ok(Uri::from_parts(parts).expect("scheme and authority are set together"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{AxumResponseTestExt, AxumRouterTestExt};
    use axum::{Router, routing::get};
    use docs_rs_headers::{X_FORWARDED_HOST, X_FORWARDED_PROTO};
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
                h.insert(&X_FORWARDED_PROTO, "https".parse().unwrap());
            })
            .await?;

        assert_eq!(res.status(), http::StatusCode::OK);
        assert_eq!(res.text().await?, "https://docs.rs:8443/hello");
        Ok(())
    }
}
