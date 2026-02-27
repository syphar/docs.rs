use anyhow::anyhow;
use axum::{RequestPartsExt as _, extract::FromRequestParts};
use http::request::Parts;
use serde::Deserialize;

use crate::{
    config::Via,
    error::AxumNope,
    extractors::{Path, RequestedHost},
};

#[derive(Debug, Deserialize)]
struct UrlParams {
    pub name: String,
    pub version: Option<String>,
    pub target: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubdomainUrlParams {
    pub version: Option<String>,
    pub target: Option<String>,
}

/// Intermediate struct to accept more variants than
/// `RustdocParams` would accept.
///
/// After we handled the edge cases we convert this struct
/// into `RustdocParams`.
#[derive(Debug)]
pub(crate) struct RustdocRedirectorParams {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) target: Option<String>,
    pub(crate) requested_host: Option<RequestedHost>,
    pub(crate) via: Via,
}

impl RustdocRedirectorParams {
    pub(crate) fn first_path_element(&self) -> Option<&str> {
        match self.via {
            Via::ApexDomain => Some(&self.name),
            Via::SubDomain => self.version.as_deref(),
        }
    }
}

impl<S> FromRequestParts<S> for RustdocRedirectorParams
where
    S: Send + Sync,
{
    type Rejection = AxumNope;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let requested_host = parts.extract::<Option<RequestedHost>>().await?;
        Ok(
            if let Some(requested_host) = &requested_host
                && let RequestedHost::SubDomain(subdomain, _parent) = requested_host
            {
                let Path(params) =
                    parts
                        .extract::<Path<SubdomainUrlParams>>()
                        .await
                        .map_err(|err| {
                            AxumNope::BadRequest(
                                anyhow!(err).context("error parsing subdomain url params"),
                            )
                        })?;

                RustdocRedirectorParams {
                    name: subdomain.clone(),
                    version: params.version,
                    target: params.target,
                    requested_host: Some(requested_host.clone()),
                    via: Via::SubDomain,
                }
            } else {
                let Path(params) = parts.extract::<Path<UrlParams>>().await.map_err(|err| {
                    AxumNope::BadRequest(anyhow!(err).context("error parsing full url params"))
                })?;
                RustdocRedirectorParams {
                    name: params.name,
                    version: params.version,
                    target: params.target,
                    requested_host: requested_host,
                    via: Via::ApexDomain,
                }
            },
        )
    }
}
