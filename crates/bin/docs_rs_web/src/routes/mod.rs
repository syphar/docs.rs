pub(crate) mod host_dispatch;
pub(crate) mod main;
pub(crate) mod subdomain;

use crate::{cache::CachePolicy, error::AxumNope, metrics::request_recorder};
use axum::{
    Extension,
    extract::Request as AxumHttpRequest,
    handler::Handler as AxumHandler,
    middleware::{self, Next},
    response::{IntoResponse, Redirect},
    routing::{MethodRouter, get, post},
};
use std::convert::Infallible;
use tracing::{debug, instrument};

const INTERNAL_PREFIXES: &[&str] = &["-", "about", "crate", "releases", "sitemap.xml"];

#[instrument(skip_all)]
pub(crate) fn get_static<H, T, S>(handler: H) -> MethodRouter<S, Infallible>
where
    H: AxumHandler<T, S>,
    T: 'static,
    S: Clone + Send + Sync + 'static,
{
    get(handler).route_layer(middleware::from_fn(|request, next| async {
        request_recorder(request, next, Some("static resource")).await
    }))
}

#[instrument(skip_all)]
fn get_internal<H, T, S>(handler: H) -> MethodRouter<S, Infallible>
where
    H: AxumHandler<T, S>,
    T: 'static,
    S: Clone + Send + Sync + 'static,
{
    get(handler).route_layer(middleware::from_fn(|request, next| async {
        request_recorder(request, next, None).await
    }))
}

#[instrument(skip_all)]
fn post_internal<H, T, S>(handler: H) -> MethodRouter<S, Infallible>
where
    H: AxumHandler<T, S>,
    T: 'static,
    S: Clone + Send + Sync + 'static,
{
    post(handler).route_layer(middleware::from_fn(|request, next| async {
        request_recorder(request, next, None).await
    }))
}

#[instrument(skip_all)]
fn get_rustdoc<H, T, S>(handler: H) -> MethodRouter<S, Infallible>
where
    H: AxumHandler<T, S>,
    T: 'static,
    S: Clone + Send + Sync + 'static,
{
    get(handler)
        .route_layer(middleware::from_fn(|request, next| async {
            request_recorder(request, next, Some("rustdoc page")).await
        }))
        .layer(middleware::from_fn(block_blacklisted_prefixes_middleware))
}

async fn block_blacklisted_prefixes_middleware(
    request: AxumHttpRequest,
    next: Next,
) -> impl IntoResponse {
    if let Some(first_component) = request.uri().path().trim_matches('/').split('/').next()
        && !first_component.is_empty()
        && (INTERNAL_PREFIXES.binary_search(&first_component).is_ok())
    {
        debug!(
            first_component = first_component,
            uri = ?request.uri(),
            "blocking blacklisted prefix"
        );
        return AxumNope::CrateNotFound.into_response();
    }

    next.run(request).await
}

fn cached_permanent_redirect(uri: &str) -> impl IntoResponse {
    (
        Extension(CachePolicy::ForeverInCdnAndBrowser),
        Redirect::permanent(uri),
    )
}

async fn fallback() -> impl IntoResponse {
    AxumNope::ResourceNotFound
}
