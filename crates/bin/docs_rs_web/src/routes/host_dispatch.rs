use crate::{
    cache::CachePolicy,
    error::AxumNope,
    extractors::RequestedHost,
    handlers::{
        about, build_details, builds, crate_details, features, releases, rustdoc, sitemap, source,
        statics::{build_static_router, static_root_dir},
        status,
    },
    metrics::request_recorder,
};
use anyhow::Result;
use askama::Template;
use axum::{
    Extension, RequestPartsExt as _, Router as AxumRouter,
    extract::Request as AxumHttpRequest,
    handler::Handler as AxumHandler,
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response as AxumResponse},
    routing::{MethodRouter, get, post},
};
use axum_extra::routing::RouterExt;
use docs_rs_headers::X_ROBOTS_TAG;
use http::HeaderValue;
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;
use tower::ServiceExt as _;
use tracing::{debug, instrument};

/// small tower service that dispatches to two separate axum routers,
/// depending on if we have a request with or without subdomain.
#[derive(Clone)]
pub(crate) struct HostDispatchService {
    main_router: AxumRouter,
    subdomain_router: AxumRouter,
}

impl HostDispatchService {
    pub(crate) fn new(main_router: AxumRouter, subdomain_router: AxumRouter) -> Self {
        Self {
            main_router,
            subdomain_router,
        }
    }
}

impl Service<AxumHttpRequest> for HostDispatchService {
    type Response = AxumResponse;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: AxumHttpRequest) -> Self::Future {
        let main_router = self.main_router.clone();
        let subdomain_router = self.subdomain_router.clone();

        Box::pin(async move {
            let (mut parts, body) = request.into_parts();
            let has_subdomain = match parts.extract::<Option<RequestedHost>>().await {
                Ok(host) => host.is_some_and(|host| host.subdomain().is_some()),
                Err(err) => return Ok(err.into_response()),
            };
            let request = AxumHttpRequest::from_parts(parts, body);

            Ok(if has_subdomain {
                subdomain_router
                    .oneshot(request)
                    .await
                    .expect("axum router service is infallible")
            } else {
                main_router
                    .oneshot(request)
                    .await
                    .expect("axum router service is infallible")
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{AxumResponseTestExt, AxumRouterTestExt};
    use axum::body::Body;
    use http::{
        Request,
        header::{HOST, VARY},
    };
    use reqwest::StatusCode;
    use test_case::test_case;

    #[test_case("crate.docs.rs")]
    #[test_case("crate.localhost")]
    #[tokio::test]
    async fn subdomain_requests_use_subdomain_router(host: &str) {
        let main_router = AxumRouter::new().route("/", get(|| async { "main" }));
        let subdomain_router = AxumRouter::new().route(
            "/",
            get(|host: RequestedHost| async move {
                format!("subdomain: {}", host.subdomain().unwrap())
            }),
        );
        let request = Request::builder()
            .uri("/")
            .header(HOST, host)
            .body(Body::empty())
            .unwrap();

        let response = HostDispatchService::new(main_router, subdomain_router)
            .call(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "subdomain: crate");
    }

    #[test_case("docs.rs")]
    #[test_case("localhost")]
    #[tokio::test]
    async fn root_domain_requests_use_main_router(host: &str) {
        let main_router = AxumRouter::new().route("/", get(|| async { "main" }));
        let subdomain_router = AxumRouter::new().route("/", get(|| async { "subdomain" }));
        let request = Request::builder()
            .uri("/")
            .header(HOST, host)
            .body(Body::empty())
            .unwrap();

        let response = HostDispatchService::new(main_router, subdomain_router)
            .call(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(VARY).unwrap(), "X-Forwarded-Host");
        assert!(response.headers().get(&X_ROBOTS_TAG).is_none());
        assert_eq!(response.text().await.unwrap(), "main");
    }

    #[tokio::test]
    async fn invalid_host_is_bad_request() {
        let main_router = AxumRouter::new().route("/", get(|| async { "main" }));
        let subdomain_router = AxumRouter::new().route("/", get(|| async { "subdomain" }));
        let request = Request::builder()
            .uri("/")
            .header(HOST, "bad/host")
            .body(Body::empty())
            .unwrap();

        let response = HostDispatchService::new(main_router, subdomain_router)
            .call(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
