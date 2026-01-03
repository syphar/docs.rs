//! Web interface of docs.rs

pub(crate) mod build_details;
pub(crate) mod builds;
pub(crate) mod crate_details;
pub(crate) mod features;
pub(crate) mod releases;
pub(crate) mod rustdoc;
pub(crate) mod sitemap;
pub(crate) mod source;
pub(crate) mod statics;
pub(crate) mod status;

use crate::Config;
use crate::metrics::WebMetrics;
use crate::middleware::{csp, security};
use crate::page::{self, TemplateData};
use crate::{cache, routes};
use anyhow::{Context as _, Error, Result, anyhow, bail};
use axum::{
    Router as AxumRouter,
    extract::{Extension, MatchedPath, Request as AxumRequest},
    http::StatusCode,
    middleware,
    middleware::Next,
    response::{IntoResponse, Response as AxumResponse},
};
use axum_extra::middleware::option_layer;
use docs_rs_context::Context;
use sentry::integrations::tower as sentry_tower;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tower::ServiceBuilder;
use tower_http::{catch_panic::CatchPanicLayer, timeout::TimeoutLayer, trace::TraceLayer};
use tracing::{info, instrument};

const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 3000);

async fn log_timeouts_to_sentry(req: AxumRequest, next: Next) -> AxumResponse {
    let uri = req.uri().clone();

    let response = next.run(req).await;

    if response.status() == StatusCode::REQUEST_TIMEOUT {
        tracing::error!(?uri, "request timeout");
    }

    response
}

async fn set_sentry_transaction_name_from_axum_route(
    request: AxumRequest,
    next: Next,
) -> AxumResponse {
    let route_name = if let Some(path) = request.extensions().get::<MatchedPath>() {
        path.as_str()
    } else {
        request.uri().path()
    };

    sentry::configure_scope(|scope| {
        scope.set_transaction(Some(route_name));
    });

    next.run(request).await
}

async fn apply_middleware(
    router: AxumRouter,
    config: Arc<Config>,
    context: &Context,
    template_data: Option<Arc<TemplateData>>,
) -> Result<AxumRouter> {
    let has_templates = template_data.is_some();

    let web_metrics = Arc::new(WebMetrics::new(&context.meter_provider));

    Ok(router.layer(
        ServiceBuilder::new()
            .layer(TraceLayer::new_for_http())
            .layer(sentry_tower::NewSentryLayer::new_from_top())
            .layer(sentry_tower::SentryHttpLayer::new().enable_transaction())
            .layer(middleware::from_fn(
                set_sentry_transaction_name_from_axum_route,
            ))
            .layer(CatchPanicLayer::new())
            .layer(middleware::from_fn(security::security_middleware))
            .layer(option_layer(
                config
                    .report_request_timeouts
                    .then_some(middleware::from_fn(log_timeouts_to_sentry)),
            ))
            .layer(option_layer(config.request_timeout.map(|to| {
                TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, to)
            })))
            .layer(Extension(context.pool()?.clone()))
            .layer(Extension(context.build_queue()?.clone()))
            .layer(Extension(web_metrics))
            .layer(Extension(config.clone()))
            .layer(Extension(context.registry_api()?.clone()))
            .layer(Extension(context.storage()?.clone()))
            .layer(option_layer(template_data.map(Extension)))
            .layer(middleware::from_fn(csp::csp_middleware))
            .layer(option_layer(has_templates.then_some(middleware::from_fn(
                page::web_page::render_templates_middleware,
            ))))
            .layer(middleware::from_fn(crate::cache::cache_middleware)),
    ))
}

pub(crate) async fn build_axum_app(
    config: Arc<Config>,
    context: &Context,
    template_data: Arc<TemplateData>,
) -> Result<AxumRouter, Error> {
    apply_middleware(
        routes::build_axum_routes(),
        config,
        context,
        Some(template_data),
    )
    .await
}

#[instrument(skip_all)]
pub async fn run_web_server(
    addr: Option<SocketAddr>,
    config: Arc<Config>,
    context: &Context,
) -> Result<(), Error> {
    let template_data = Arc::new(TemplateData::new(config.render_threads)?);

    let axum_addr = addr.unwrap_or(DEFAULT_BIND);

    tracing::info!(
        "Starting web server on `{}:{}`",
        axum_addr.ip(),
        axum_addr.port()
    );

    let app = build_axum_app(config, context, template_data)
        .await?
        .into_make_service();
    let listener = tokio::net::TcpListener::bind(axum_addr)
        .await
        .context("error binding socket for web server")?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("signal received, starting graceful shutdown");
}

#[instrument]
pub(crate) fn axum_redirect<U>(uri: U) -> Result<impl IntoResponse, Error>
where
    U: TryInto<http::Uri> + std::fmt::Debug,
    <U as TryInto<http::Uri>>::Error: std::fmt::Debug,
{
    let uri: http::Uri = uri
        .try_into()
        .map_err(|err| anyhow!("invalid URI: {:?}", err))?;

    if let Some(path_and_query) = uri.path_and_query() {
        if path_and_query.as_str().starts_with("//") {
            bail!("protocol relative redirects are forbidden");
        }
    } else {
        // we always want a path to redirect to, even when it's just `/`
        bail!("missing path in URI");
    }

    Ok((
        StatusCode::FOUND,
        [(
            http::header::LOCATION,
            http::HeaderValue::try_from(uri.to_string()).context("invalid uri for redirect")?,
        )],
    ))
}

#[instrument]
pub(crate) fn axum_cached_redirect<U>(
    uri: U,
    cache_policy: cache::CachePolicy,
) -> Result<axum::response::Response, Error>
where
    U: TryInto<http::Uri> + std::fmt::Debug,
    <U as TryInto<http::Uri>>::Error: std::fmt::Debug,
{
    let mut resp = axum_redirect(uri)?.into_response();
    resp.extensions_mut().insert(cache_policy);
    Ok(resp)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        match_release::match_version,
        metadata::MetaData,
        testing::{AxumResponseTestExt, AxumRouterTestExt, TestEnvironment, async_wrapper},
        utils::get_correct_docsrs_style_file,
    };
    use docs_rs_database::testing::TestDatabase;
    use docs_rs_test_fakes::FakeBuild;
    use docs_rs_types::{BuildStatus, DocCoverage, ReleaseId, ReqVersion, Version};
    use kuchikiki::traits::TendrilSink;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::str::FromStr as _;
    use test_case::test_case;

    async fn release(version: &str, env: &TestEnvironment) -> ReleaseId {
        let version = Version::parse(version).unwrap();
        env.fake_release()
            .await
            .name("foo")
            .version(version)
            .create()
            .await
            .unwrap()
    }

    async fn version(v: Option<&str>, db: &TestDatabase) -> Option<Version> {
        let mut conn = db.async_conn().await.unwrap();
        let version = match_version(
            &mut conn,
            "foo",
            &ReqVersion::from_str(v.unwrap_or_default()).unwrap(),
        )
        .await
        .ok()?
        .assume_exact_name()
        .ok()?
        .into_version();
        Some(version)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn semver(version: &'static str) -> Option<Version> {
        version.parse().ok()
    }

    #[allow(clippy::unnecessary_wraps)]
    fn exact(version: &'static str) -> Option<Version> {
        version.parse().ok()
    }

    async fn clipboard_is_present_for_path(path: &str, web: &axum::Router) -> bool {
        let data = web.get(path).await.unwrap().text().await.unwrap();
        let node = kuchikiki::parse_html().one(data);
        node.select("#clipboard").unwrap().count() == 1
    }

    #[test]
    fn test_get_correct_docsrs_style_file() {
        assert_eq!(
            get_correct_docsrs_style_file("rustc 1.10.0-nightly (57ef01513 2016-05-23)").unwrap(),
            "rustdoc.css"
        );
        assert_eq!(
            get_correct_docsrs_style_file("docsrs 0.2.0 (ba9ae23 2022-05-26)").unwrap(),
            "rustdoc-2021-12-05.css"
        );
        assert!(get_correct_docsrs_style_file("docsrs 0.2.0").is_err(),);
    }

    #[test]
    fn test_index_returns_success() {
        async_wrapper(|env| async move {
            let web = env.web_app().await;
            assert!(web.get("/").await?.status().is_success());
            Ok(())
        });
    }

    #[test]
    fn test_doc_coverage_for_crate_pages() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("foo")
                .version("0.0.1")
                .source_file("test.rs", &[])
                .doc_coverage(DocCoverage {
                    total_items: 10,
                    documented_items: 6,
                    total_items_needing_examples: 2,
                    items_with_examples: 1,
                })
                .create()
                .await?;
            let web = env.web_app().await;

            let foo_crate = kuchikiki::parse_html()
                .one(web.assert_success("/crate/foo/0.0.1").await?.text().await?);

            for (idx, value) in ["60%", "6", "10", "2", "1"].iter().enumerate() {
                let mut menu_items = foo_crate.select(".pure-menu-item b").unwrap();
                assert!(
                    menu_items.any(|e| e.text_contents().contains(value)),
                    "({idx}, {value:?})"
                );
            }

            let foo_doc = kuchikiki::parse_html()
                .one(web.assert_success("/foo/0.0.1/foo/").await?.text().await?);
            assert!(
                foo_doc
                    .select(".pure-menu-link b")
                    .unwrap()
                    .any(|e| e.text_contents().contains("60%"))
            );

            Ok(())
        });
    }

    #[test]
    fn test_show_clipboard_for_crate_pages() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("fake_crate")
                .version("0.0.1")
                .source_file("test.rs", &[])
                .create()
                .await?;
            let web = env.web_app().await;
            assert!(clipboard_is_present_for_path("/crate/fake_crate/0.0.1", &web).await);
            assert!(clipboard_is_present_for_path("/crate/fake_crate/0.0.1/source/", &web).await);
            assert!(clipboard_is_present_for_path("/fake_crate/0.0.1/fake_crate/", &web).await);
            Ok(())
        });
    }

    #[test]
    fn test_hide_clipboard_for_non_crate_pages() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("fake_crate")
                .version("0.0.1")
                .create()
                .await?;
            let web = env.web_app().await;
            assert!(!clipboard_is_present_for_path("/about", &web).await);
            assert!(!clipboard_is_present_for_path("/releases", &web).await);
            assert!(!clipboard_is_present_for_path("/", &web).await);
            assert!(!clipboard_is_present_for_path("/not/a/real/path", &web).await);
            Ok(())
        });
    }

    #[test]
    fn standard_library_redirects() {
        async fn assert_external_redirect_success(
            web: &axum::Router,
            path: &str,
            expected_target: &str,
        ) -> Result<()> {
            let redirect_response = web.assert_redirect_unchecked(path, expected_target).await?;

            let external_target_url = redirect_response.redirect_target().unwrap();

            let response = reqwest::get(external_target_url).await?;
            let status = response.status();
            assert!(
                status.is_success(),
                "failed to GET {external_target_url}: {status}"
            );
            Ok(())
        }

        async_wrapper(|env| async move {
            let web = env.web_app().await;
            for krate in &["std", "alloc", "core", "proc_macro", "test"] {
                let target = format!("https://doc.rust-lang.org/stable/{krate}/");

                // with or without slash
                assert_external_redirect_success(&web, &format!("/{krate}"), &target).await?;
                assert_external_redirect_success(&web, &format!("/{krate}/"), &target).await?;
            }

            let target = "https://doc.rust-lang.org/stable/proc_macro/";
            // with or without slash
            assert_external_redirect_success(&web, "/proc-macro", target).await?;
            assert_external_redirect_success(&web, "/proc-macro/", target).await?;

            let target = "https://doc.rust-lang.org/nightly/nightly-rustc/";
            // with or without slash
            assert_external_redirect_success(&web, "/rustc", target).await?;
            assert_external_redirect_success(&web, "/rustc/", target).await?;

            let target = "https://doc.rust-lang.org/nightly/nightly-rustc/rustdoc/";
            // with or without slash
            assert_external_redirect_success(&web, "/rustdoc", target).await?;
            assert_external_redirect_success(&web, "/rustdoc/", target).await?;

            // queries are supported
            assert_external_redirect_success(
                &web,
                "/std?search=foobar",
                "https://doc.rust-lang.org/stable/std/?search=foobar",
            )
            .await?;

            Ok(())
        })
    }

    #[test]
    fn double_slash_does_redirect_to_latest_version() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("bat")
                .version("0.2.0")
                .create()
                .await?;
            let web = env.web_app().await;
            web.assert_redirect("/bat//", "/bat/latest/bat/").await?;
            Ok(())
        })
    }

    #[test]
    fn binary_docs_redirect_to_crate() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("bat")
                .version("0.2.0")
                .binary(true)
                .create()
                .await?;
            let web = env.web_app().await;
            web.assert_redirect("/bat/0.2.0", "/crate/bat/0.2.0")
                .await?;
            web.assert_redirect("/bat/0.2.0/aarch64-unknown-linux-gnu", "/crate/bat/0.2.0")
                .await?;
            /* TODO: this should work (https://github.com/rust-lang/docs.rs/issues/603)
            assert_redirect("/bat/0.2.0/aarch64-unknown-linux-gnu/bat", "/crate/bat/0.2.0", web)?;
            assert_redirect("/bat/0.2.0/aarch64-unknown-linux-gnu/bat/", "/crate/bat/0.2.0/", web)?;
            */
            Ok(())
        })
    }

    #[test]
    fn can_view_source() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("regex")
                .version("0.3.0")
                .source_file("src/main.rs", br#"println!("definitely valid rust")"#)
                .create()
                .await?;

            let web = env.web_app().await;
            web.assert_success("/crate/regex/0.3.0/source/src/main.rs")
                .await?;
            web.assert_success("/crate/regex/0.3.0/source/").await?;
            web.assert_success("/crate/regex/0.3.0/source/src").await?;
            web.assert_success("/regex/0.3.0/src/regex/main.rs.html")
                .await?;
            Ok(())
        })
    }

    #[test]
    // https://github.com/rust-lang/docs.rs/issues/223
    fn prereleases_are_not_considered_for_semver() {
        async_wrapper(|env| async move {
            let db = &env.db;
            let version = |v| version(v, db);
            let release = |v| release(v, &env);

            release("0.3.1-pre").await;
            for search in &["*", "newest", "latest"] {
                assert_eq!(version(Some(search)).await, semver("0.3.1-pre"));
            }

            release("0.3.1-alpha").await;
            assert_eq!(version(Some("0.3.1-alpha")).await, exact("0.3.1-alpha"));

            release("0.3.0").await;
            let three = semver("0.3.0");
            assert_eq!(version(None).await, three);
            // same thing but with "*"
            assert_eq!(version(Some("*")).await, three);
            // make sure exact matches still work
            assert_eq!(version(Some("0.3.0")).await, exact("0.3.0"));

            Ok(())
        });
    }

    #[test]
    fn platform_dropdown_not_shown_with_no_targets() {
        async_wrapper(|env| async move {
            release("0.1.0", &env).await;
            let web = env.web_app().await;
            let text = web.get("/foo/0.1.0/foo").await?.text().await?;
            let platform = kuchikiki::parse_html()
                .one(text)
                .select(r#"ul > li > a[aria-label="Platform"]"#)
                .unwrap()
                .count();
            assert_eq!(platform, 0);

            // sanity check the test is doing something
            env.fake_release()
                .await
                .name("foo")
                .version("0.2.0")
                .add_platform("x86_64-unknown-linux-musl")
                .create()
                .await?;
            let text = web.assert_success("/foo/0.2.0/foo/").await?.text().await?;
            let platform = kuchikiki::parse_html()
                .one(text)
                .select(r#"ul > li > a[aria-label="Platform"]"#)
                .unwrap()
                .count();
            assert_eq!(platform, 1);
            Ok(())
        });
    }

    #[test]
    // https://github.com/rust-lang/docs.rs/issues/221
    fn yanked_crates_are_not_considered() {
        async_wrapper(|env| async move {
            let db = &env.db;

            let release_id = release("0.3.0", &env).await;

            sqlx::query!(
                "UPDATE releases SET yanked = true WHERE id = $1 AND version = '0.3.0'",
                release_id.0
            )
            .execute(&mut *db.async_conn().await?)
            .await?;

            assert_eq!(version(None, db).await, None);
            assert_eq!(version(Some("0.3"), db).await, None);

            release("0.1.0+4.1", &env).await;
            assert_eq!(version(Some("0.1.0+4.1"), db).await, exact("0.1.0+4.1"));
            assert_eq!(version(None, db).await, semver("0.1.0+4.1"));

            Ok(())
        });
    }

    #[test]
    fn in_progress_releases_are_ignored_when_others_match() {
        async_wrapper(|env| async move {
            let db = &env.db;

            // normal release
            release("1.0.0", &env).await;

            // in progress release
            env.fake_release()
                .await
                .name("foo")
                .version("1.1.0")
                .builds(vec![
                    FakeBuild::default().build_status(BuildStatus::InProgress),
                ])
                .create()
                .await?;

            // STAR gives me the prod release
            assert_eq!(version(Some("*"), db).await, exact("1.0.0"));

            // exact-match query gives me the in progress release
            assert_eq!(version(Some("=1.1.0"), db).await, exact("1.1.0"));

            Ok(())
        })
    }

    #[test]
    // https://github.com/rust-lang/docs.rs/issues/1682
    fn prereleases_are_considered_when_others_dont_match() {
        async_wrapper(|env| async move {
            let db = &env.db;

            // normal release
            release("1.0.0", &env).await;
            // prereleases
            release("2.0.0-alpha.1", &env).await;
            release("2.0.0-alpha.2", &env).await;

            // STAR gives me the prod release
            assert_eq!(version(Some("*"), db).await, exact("1.0.0"));

            // prerelease query gives me the latest prerelease
            assert_eq!(
                version(Some(">=2.0.0-alpha"), db).await,
                exact("2.0.0-alpha.2")
            );

            Ok(())
        })
    }

    #[test]
    // vaguely related to https://github.com/rust-lang/docs.rs/issues/395
    fn metadata_has_no_effect() {
        async_wrapper(|env| async move {
            let db = &env.db;

            release("0.1.0+4.1", &env).await;
            release("0.1.1", &env).await;
            assert_eq!(version(None, db).await, semver("0.1.1"));
            release("0.5.1+zstd.1.4.4", &env).await;
            assert_eq!(version(None, db).await, semver("0.5.1+zstd.1.4.4"));
            assert_eq!(version(Some("0.5"), db).await, semver("0.5.1+zstd.1.4.4"));
            assert_eq!(
                version(Some("0.5.1+zstd.1.4.4"), db).await,
                exact("0.5.1+zstd.1.4.4")
            );

            Ok(())
        });
    }

    #[test]
    fn serialize_metadata() {
        let mut metadata = MetaData {
            name: "serde".parse().unwrap(),
            version: "1.0.0".parse().unwrap(),
            req_version: ReqVersion::Latest,
            description: Some("serde does stuff".to_string()),
            target_name: None,
            rustdoc_status: Some(true),
            default_target: Some("x86_64-unknown-linux-gnu".to_string()),
            doc_targets: Some(vec![
                "x86_64-unknown-linux-gnu".to_string(),
                "arm64-unknown-linux-gnu".to_string(),
            ]),
            yanked: Some(false),
            rustdoc_css_file: Some("rustdoc.css".to_string()),
        };

        let correct_json = json!({
            "name": "serde",
            "version": "1.0.0",
            "req_version": "latest",
            "description": "serde does stuff",
            "target_name": null,
            "rustdoc_status": true,
            "default_target": "x86_64-unknown-linux-gnu",
            "doc_targets": [
                "x86_64-unknown-linux-gnu",
                "arm64-unknown-linux-gnu",
            ],
            "yanked": false,
            "rustdoc_css_file": "rustdoc.css",
        });

        assert_eq!(correct_json, serde_json::to_value(&metadata).unwrap());

        metadata.target_name = Some("serde_lib_name".to_string());
        let correct_json = json!({
            "name": "serde",
            "version": "1.0.0",
            "req_version": "latest",
            "description": "serde does stuff",
            "target_name": "serde_lib_name",
            "rustdoc_status": true,
            "default_target": "x86_64-unknown-linux-gnu",
            "doc_targets": [
                "x86_64-unknown-linux-gnu",
                "arm64-unknown-linux-gnu",
            ],
            "yanked": false,
            "rustdoc_css_file": "rustdoc.css",
        });

        assert_eq!(correct_json, serde_json::to_value(&metadata).unwrap());

        metadata.description = None;
        let correct_json = json!({
            "name": "serde",
            "version": "1.0.0",
            "req_version": "latest",
            "description": null,
            "target_name": "serde_lib_name",
            "rustdoc_status": true,
            "default_target": "x86_64-unknown-linux-gnu",
            "doc_targets": [
                "x86_64-unknown-linux-gnu",
                "arm64-unknown-linux-gnu",
            ],
            "yanked": false,
            "rustdoc_css_file": "rustdoc.css",
        });

        assert_eq!(correct_json, serde_json::to_value(&metadata).unwrap());
    }

    #[test]
    fn metadata_from_crate() {
        async_wrapper(|env| async move {
            release("0.1.0", &env).await;
            let mut conn = env.async_conn().await?;
            let metadata = MetaData::from_crate(
                &mut conn,
                "foo",
                &"0.1.0".parse().unwrap(),
                Some(ReqVersion::Latest),
            )
            .await;
            assert_eq!(
                metadata.unwrap(),
                MetaData {
                    name: "foo".parse().unwrap(),
                    version: "0.1.0".parse().unwrap(),
                    req_version: ReqVersion::Latest,
                    description: Some("Fake package".to_string()),
                    target_name: Some("foo".to_string()),
                    rustdoc_status: Some(true),
                    default_target: Some("x86_64-unknown-linux-gnu".to_string()),
                    doc_targets: Some(vec!["x86_64-unknown-linux-gnu".to_string()]),
                    yanked: Some(false),
                    rustdoc_css_file: Some("rustdoc.css".to_string()),
                },
            );
            Ok(())
        })
    }

    #[test]
    fn test_tabindex_is_present_on_topbar_crate_search_input() {
        async_wrapper(|env| async move {
            release("0.1.0", &env).await;
            let web = env.web_app().await;
            let text = web.assert_success("/foo/0.1.0/foo/").await?.text().await?;
            let tabindex = kuchikiki::parse_html()
                .one(text)
                .select(r#"#nav-search[tabindex="-1"]"#)
                .unwrap()
                .count();
            assert_eq!(tabindex, 1);
            Ok(())
        });
    }

    #[test]
    fn test_axum_redirect() {
        let response = axum_redirect("/something").unwrap().into_response();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(http::header::LOCATION).unwrap(),
            "/something"
        );
        assert!(
            response
                .headers()
                .get(http::header::CACHE_CONTROL)
                .is_none()
        );
        assert!(response.extensions().get::<cache::CachePolicy>().is_none());
    }

    #[test]
    fn test_axum_redirect_cached() {
        let response = axum_cached_redirect("/something", cache::CachePolicy::NoCaching)
            .unwrap()
            .into_response();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(http::header::LOCATION).unwrap(),
            "/something"
        );
        assert!(matches!(
            response.extensions().get::<cache::CachePolicy>().unwrap(),
            cache::CachePolicy::NoCaching,
        ))
    }

    #[test_case("without_leading_slash")]
    #[test_case("//with_double_leading_slash")]
    fn test_axum_redirect_failure(path: &str) {
        assert!(axum_redirect(path).is_err());
        assert!(axum_cached_redirect(path, cache::CachePolicy::NoCaching).is_err());
    }
}
