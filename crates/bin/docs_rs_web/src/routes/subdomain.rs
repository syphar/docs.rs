use anyhow::Result;
use axum::{
    Router as AxumRouter,
    middleware::{self, Next},
    routing::get,
};
use docs_rs_headers::X_ROBOTS_TAG;
use http::HeaderValue;

pub(crate) fn build_subdomain_axum_routes() -> Result<AxumRouter> {
    // TODO:
    // * serve robots.txt, currently forbid, later for crate?
    // * add sitemap just for the subdomain (?)
    // * reference these sub-sitemaps in the main sitemap.

    // Keep this separate from the main router so we can evolve subdomain-only behavior
    // without changing the non-subdomain route tree.
    Ok(AxumRouter::new()
        .route("/", get(|| async { "subdomain" }))
        .route("/{*path}", get(|| async { "subdomain" }))
        .layer(middleware::from_fn(|request, next: Next| async {
            // temporary forbid search engines on all subdomain routes.
            let mut response = next.run(request).await;
            let headers = response.headers_mut();
            headers.insert(
                &X_ROBOTS_TAG,
                HeaderValue::from_static("noindex, nofollow, noarchive"),
            );
            headers.insert("x-docsrs-subdomain-router", HeaderValue::from_static("1"));
            response
        })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::host_dispatch::HostDispatchService;
    use axum::body::Body;
    use docs_rs_headers::X_ROBOTS_TAG;
    use http::{
        Request,
        header::{HOST, VARY},
    };
    use reqwest::StatusCode;
    use tower::Service as _;

    #[tokio::test]
    async fn built_subdomain_router_adds_response_headers() {
        let main_router = AxumRouter::new().route("/", get(|| async { "main" }));
        let subdomain_router = super::build_subdomain_axum_routes().unwrap();
        let request = Request::builder()
            .uri("/")
            .header(HOST, "crate.docs.rs")
            .body(Body::empty())
            .unwrap();

        let response = HostDispatchService::new(main_router, subdomain_router)
            .call(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(VARY).unwrap(), "X-Forwarded-Host");
        assert_eq!(
            response.headers().get(&X_ROBOTS_TAG).unwrap(),
            "noindex, nofollow, noarchive"
        );
        assert_eq!(
            response.headers().get("x-docsrs-subdomain-router").unwrap(),
            "1"
        );
    }
}
