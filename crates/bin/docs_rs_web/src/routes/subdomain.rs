use anyhow::Result;
use axum::{
    Router as AxumRouter,
    middleware::{self, Next},
};
use docs_rs_headers::X_ROBOTS_TAG;
use http::HeaderValue;

use crate::{
    handlers::{
        StorageChangeDetection, rustdoc,
        statics::{build_static_router, static_root_dir},
    },
    routes::{cached_permanent_redirect, fallback, get_internal, get_rustdoc, get_static},
};

pub(crate) fn build_subdomain_axum_routes() -> Result<AxumRouter> {
    // TODO:
    // * serve robots.txt, currently forbid, later for crate?
    // * add sitemap just for the subdomain (?)
    // * reference these sub-sitemaps in the main sitemap.

    // Keep this separate from the main router so we can evolve subdomain-only behavior
    // without changing the non-subdomain route tree.
    Ok(AxumRouter::new()
        .route(
            "/robots.txt",
            get_static(|| async {
                // for now, forbid everyone to crawl on subdomains
                "User-agent: *\nDisallow: /"
            }),
        )
        .route(
            "/favicon.ico",
            get_static(|| async {
                // FIXME: crate specific favicon? where would that be?
                cached_permanent_redirect("/-/static/favicon.ico")
            }),
        )
        // `.nest` with fallbacks is currently broken, `.nest_service works
        // https://github.com/tokio-rs/axum/issues/3138
        // FIXME: caching: could we somehow cache the assets across the crate subdomains?
        .nest_service("/-/static", build_static_router(static_root_dir()?))
        // .route(
        //     "/opensearch.xml",
        //     get_static(|| async { cached_permanent_redirect("/-/static/opensearch.xml") }),
        // )
        // .route_with_tsr("/sitemap.xml", get_internal(sitemap::sitemapindex_handler))
        .route(
            "/-/rustdoc.static/{*path}",
            // FIXME: caching: could we somehow cache the assets across the crate subdomains?
            get_internal(rustdoc::static_asset_handler),
        )
        .route(
            "/-/storage-change-detection.html",
            get_internal(|| async { StorageChangeDetection }),
        )
        .route("/badge.svg", get_internal(rustdoc::badge_handler))
        // FIXME: redirects need to redirect to subdomain or main domain, depending on where the
        // request came from
        .route("/", get_rustdoc(rustdoc::rustdoc_redirector_handler))
        .route(
            "/{version}",
            get_rustdoc(rustdoc::rustdoc_redirector_handler),
        )
        .route(
            "/{version}/",
            get_rustdoc(rustdoc::rustdoc_redirector_handler),
        )
        .route(
            "/{version}/all.html",
            get_rustdoc(rustdoc::rustdoc_html_server_handler),
        )
        .route(
            "/{version}/help.html",
            get_rustdoc(rustdoc::rustdoc_html_server_handler),
        )
        .route(
            "/{version}/settings.html",
            get_rustdoc(rustdoc::rustdoc_html_server_handler),
        )
        .route(
            "/{version}/scrape-examples-help.html",
            get_rustdoc(rustdoc::rustdoc_html_server_handler),
        )
        .route(
            "/{version}/{target}",
            get_rustdoc(rustdoc::rustdoc_redirector_handler),
        )
        .route(
            "/{version}/{target}/",
            get_rustdoc(rustdoc::rustdoc_html_server_handler),
        )
        .route(
            "/{version}/{target}/{*path}",
            get_rustdoc(rustdoc::rustdoc_html_server_handler),
        )
        .fallback(fallback)
        .layer(middleware::from_fn(|request, next: Next| async {
            let mut response = next.run(request).await;
            let headers = response.headers_mut();

            // for now, forbid everyone to crawl on subdomains
            headers.insert(
                &X_ROBOTS_TAG,
                HeaderValue::from_static("noindex, nofollow, noarchive"),
            );

            response
        })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::host_dispatch::HostDispatchService;
    use axum::{body::Body, routing::get};
    use docs_rs_headers::X_ROBOTS_TAG;
    use http::{Request, header::HOST};
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
        assert_eq!(
            response.headers().get(&X_ROBOTS_TAG).unwrap(),
            "noindex, nofollow, noarchive"
        );
    }
}
