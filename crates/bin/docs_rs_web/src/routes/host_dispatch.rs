use crate::extractors::RequestedHost;
use axum::{
    Router as AxumRouter,
    extract::Request as AxumHttpRequest,
    response::{IntoResponse, Response as AxumResponse},
};
use std::{
    convert::Infallible,
    future::Future,
    future::ready,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;

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
        let has_subdomain = match RequestedHost::from_headers(request.headers()) {
            Ok(host) => host.is_some_and(|host| host.subdomain().is_some()),
            Err(err) => return Box::pin(ready(Ok(err.into_response()))),
        };

        if has_subdomain {
            Box::pin(self.subdomain_router.call(request))
        } else {
            Box::pin(self.main_router.call(request))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::AxumResponseTestExt;
    use axum::{body::Body, routing::get};
    use docs_rs_headers::X_ROBOTS_TAG;
    use http::{Request, header::HOST};
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
