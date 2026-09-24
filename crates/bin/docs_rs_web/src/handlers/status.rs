use crate::{
    cache::CachePolicy,
    error::AxumResult,
    extractors::{DbConnection, Path},
    impl_axum_webpage,
    page::{
        templates::{RenderBrands, RenderSolid},
        warnings::{self, ActiveAbnormalities},
    },
};
use askama::Template;
use axum::{
    extract::Extension,
    response::{IntoResponse, Response as AxumResponse},
};
use docs_rs_build_queue::AsyncBuildQueue;
use docs_rs_database::service_config::Abnormality;
use docs_rs_rustsec::{OsvAdvisory, RustsecClient};
use docs_rs_std_replacements::{ReplacementDetails, StdReplacements};
use docs_rs_types::KrateName;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::error;

#[derive(Debug, Clone, PartialEq, Template)]
#[template(path = "core/about/status.html")]
struct AboutStatus {
    abnormalities: Vec<Abnormality>,
}

impl_axum_webpage!(
    AboutStatus,
    cache_policy = |_| CachePolicy::ShortInCdnAndBrowser
);

#[derive(Template)]
#[template(path = "header/abnormalities.html")]
#[derive(Debug, Clone)]
struct Abnormalities {
    abnormalities: ActiveAbnormalities,
}

impl_axum_webpage! {
    Abnormalities,
    cache_policy = |_| CachePolicy::LongerInCdnAndBrowser
}

pub(crate) async fn status_handler(
    Extension(build_queue): Extension<Arc<AsyncBuildQueue>>,
    mut conn: DbConnection,
) -> AxumResult<impl IntoResponse> {
    Ok(AboutStatus {
        abnormalities: warnings::load_abnormalities(&mut conn, &build_queue).await?,
    })
}

pub(crate) async fn abnormalities(
    Extension(build_queue): Extension<Arc<AsyncBuildQueue>>,
    mut conn: DbConnection,
) -> AxumResult<AxumResponse> {
    Ok(Abnormalities {
        abnormalities: warnings::load_abnormalities(&mut conn, &build_queue).await?,
    }
    .into_response())
}

#[derive(Template)]
#[template(path = "header/crate_warnings.html")]
struct CrateWarnings {
    replacement: Option<Arc<ReplacementDetails>>,
    unmaintained: Option<Arc<OsvAdvisory>>,
    ttl: Duration,
}

impl_axum_webpage! {
    CrateWarnings,
    cache_policy = |page| CachePolicy::InCdnAndBrowser(page.ttl)
}

/// Render crate warnings for insertion into the documentation topbar.
pub(crate) async fn crate_warnings(
    rustsec: Option<Extension<Arc<RustsecClient>>>,
    std_replacements: Option<Extension<Arc<StdReplacements>>>,
    Path(name): Path<KrateName>,
) -> AxumResult<impl IntoResponse> {
    let started_at = Instant::now();

    // Failed lookups return an empty result with zero TTL; disabled clients return None.
    let (std_replacement, unmaintained) = tokio::join!(
        async {
            match std_replacements {
                Some(Extension(client)) => Some(
                    client.get(&name).await
                        .inspect_err(|err| error!(?err, %name, "failed to fetch standard-library replacements"))
                        .unwrap_or_default(),
                ),
                None => None,
            }
        },
        async {
            match rustsec {
                Some(Extension(client)) => Some(
                    client
                        .find_unmaintained(&name)
                        .await
                        .inspect_err(
                            |err| error!(?err, %name, "failed to fetch RustSec advisories"),
                        )
                        .unwrap_or_default(),
                ),
                None => None,
            }
        }
    );

    let ttl = match (&std_replacement, &unmaintained) {
        (Some(replacement), Some(advisory)) => replacement.ttl.min(advisory.ttl),
        // Disabled sources don't restrict the other source's TTL.
        (Some(replacement), None) => replacement.ttl,
        (None, Some(advisory)) => advisory.ttl,
        (None, None) => Duration::ZERO,
    };
    // Conservatively account for time spent waiting for the slower lookup.
    let ttl = ttl.saturating_sub(started_at.elapsed());

    Ok(CrateWarnings {
        replacement: std_replacement.and_then(|result| result.value),
        unmaintained: unmaintained.and_then(|result| result.value),
        ttl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cache::CachePolicy,
        testing::{
            AxumResponseTestExt, AxumRouterTestExt, TestEnvironment, TestEnvironmentExt as _,
            headers::test_typed_encode,
        },
    };
    use anyhow::Result;
    use axum_extra::headers::{CacheControl, HeaderMapExt as _};
    use bon::bon;
    use docs_rs_config::AppConfig as _;
    use docs_rs_database::service_config::{Abnormality, ConfigName, set_config};
    use docs_rs_std_replacements::{ReplacementDetails, ReplacementMap, testing::std_replacement};
    use docs_rs_types::{Duration, KrateName, testing::V1};
    use docs_rs_uri::EscapedURI;
    use http::{StatusCode, header::CACHE_CONTROL};
    use kuchikiki::traits::TendrilSink;
    use std::{iter, str::FromStr, sync::Arc};
    use test_case::test_case;

    const OWNED_ALLOC: KrateName = KrateName::from_static("owned-alloc");

    struct WarningSourceMock {
        std_replacement_server: mockito::ServerGuard,
        rustsec_server: mockito::ServerGuard,
        mocks: Vec<mockito::Mock>,
    }

    #[bon]
    impl WarningSourceMock {
        async fn new() -> Result<Self> {
            Ok(Self {
                std_replacement_server: mockito::Server::new_async().await,
                rustsec_server: mockito::Server::new_async().await,
                mocks: Vec::new(),
            })
        }

        #[builder(start_fn(name = mock_rustsec), finish_fn(name = mock))]
        async fn with_rustsec(
            mut self,
            #[builder(start_fn)] krate: KrateName,
            #[builder(default = StatusCode::OK)] status_code: StatusCode,
            #[builder(default = false)] empty: bool,
            cache_control: Option<CacheControl>,
        ) -> Self {
            let mut mock = self
                .rustsec_server
                .mock("GET", format!("/packages/{}.json", krate).as_str())
                .with_status(status_code.as_u16().into());

            if let Some(cache_control) = cache_control {
                let value = test_typed_encode(cache_control);
                mock = mock.with_header(CACHE_CONTROL, value.to_str().unwrap());
            }

            let empty = empty || (status_code.is_client_error() || status_code.is_server_error());

            self.mocks.push(
                mock.with_body(if empty {
                    ""
                } else {
                    include_str!("../../../../lib/docs_rs_rustsec/tests/fixtures/owned-alloc.json")
                })
                .create_async()
                .await,
            );

            self
        }

        async fn with_replacement(
            self,
            krate: KrateName,
            details: ReplacementDetails,
            cache_control: Option<CacheControl>,
        ) -> Self {
            self.with_replacements([(krate, details)], cache_control)
                .await
        }

        async fn with_replacements(
            self,
            items: impl IntoIterator<Item = (KrateName, ReplacementDetails)>,
            cache_control: Option<CacheControl>,
        ) -> Self {
            self.with_replacements_and_status(items, cache_control, StatusCode::OK)
                .await
        }

        async fn with_replacements_and_status(
            mut self,
            items: impl IntoIterator<Item = (KrateName, ReplacementDetails)>,
            cache_control: Option<CacheControl>,
            status_code: StatusCode,
        ) -> Self {
            let map = ReplacementMap::from_iter(
                items
                    .into_iter()
                    .map(|(krate, details)| (krate, Arc::new(details))),
            );

            let mut mock = self
                .std_replacement_server
                .mock("GET", "/all.json")
                .expect(1)
                .with_status(status_code.as_u16().into());

            if let Some(cache_control) = cache_control {
                let value = test_typed_encode(cache_control);
                mock = mock.with_header(CACHE_CONTROL, value.to_str().unwrap());
            }

            self.mocks.push(
                mock.with_body(serde_json::to_string(&map).unwrap())
                    .expect(1)
                    .create_async()
                    .await,
            );

            self
        }

        fn std_replacements_config(
            &self,
            default_ttl: Option<Duration>,
        ) -> docs_rs_std_replacements::Config {
            docs_rs_std_replacements::Config::builder()
                .url(
                    format!("{}/all.json", self.std_replacement_server.url())
                        .parse()
                        .unwrap(),
                )
                .max_retries(0)
                .maybe_cache_default_ttl(default_ttl)
                .build()
        }

        fn rustsec_config(&self, ttl: Option<Duration>) -> docs_rs_rustsec::Config {
            docs_rs_rustsec::Config::builder()
                .base_url(self.rustsec_server.url().parse().unwrap())
                .max_retries(0)
                .maybe_cache_default_ttl(ttl)
                .build()
        }

        async fn assert_async(self) {
            for mock in self.mocks {
                mock.assert_async().await;
            }
        }
    }

    fn assert_ttl(response: &axum::response::Response, expected: Duration) {
        let header = response
            .headers()
            .typed_get::<CacheControl>()
            .expect("valid Cache-Control header");

        let ttl = header.max_age().expect("max-age directive");
        if expected == Duration::ZERO {
            assert!(!header.public());
            assert_eq!(ttl, expected);
        } else {
            let expected = expected.as_secs();
            assert!(header.public());
            assert!(
                (expected.saturating_sub(10)..=expected).contains(&ttl.as_secs()),
                "{header:?}"
            );
        }
    }

    #[test_case(Duration::from_mins(1), Duration::from_mins(2), false, Duration::from_mins(1); "replacement shorter")]
    #[test_case(Duration::from_mins(2), Duration::from_mins(1), false, Duration::from_mins(1); "rustsec shorter")]
    #[test_case(Duration::from_secs(1200), Duration::from_secs(1800), false, Duration::from_secs(1200); "longer TTL")]
    #[test_case(Duration::ZERO, Duration::from_mins(2), false, Duration::ZERO; "uncacheable")]
    #[test_case(Duration::from_mins(1), Duration::from_mins(2), true, Duration::from_mins(1); "empty HTML")]
    #[tokio::test(flavor = "multi_thread")]
    async fn crate_warnings_uses_remaining_ttl(
        std_ttl: Duration,
        rustsec_ttl: Duration,
        empty: bool,
        expected: Duration,
    ) -> Result<()> {
        let replacement = std_replacement("Use std");

        let mut mock_server = WarningSourceMock::new().await?;

        let std_cache = CacheControl::new().with_max_age(std_ttl.into());
        mock_server = if empty {
            mock_server
                .with_replacements(iter::empty(), Some(std_cache))
                .await
        } else {
            mock_server
                .with_replacement(OWNED_ALLOC, replacement.clone(), Some(std_cache))
                .await
        };

        let rustsec_cache = CacheControl::new().with_max_age(rustsec_ttl.into());
        mock_server = if empty {
            mock_server
                .mock_rustsec(OWNED_ALLOC)
                .status_code(StatusCode::NOT_FOUND)
                .cache_control(rustsec_cache)
                .mock()
                .await
        } else {
            mock_server
                .mock_rustsec(OWNED_ALLOC)
                .empty(false)
                .cache_control(rustsec_cache)
                .mock()
                .await
        };

        let env = TestEnvironment::builder()
            .std_replacements_config(mock_server.std_replacements_config(None))
            .rustsec_config(mock_server.rustsec_config(Some(rustsec_ttl)))
            .build()
            .await?;

        let response = env
            .web_app()
            .await
            .assert_success("/-/partial/crate-warnings/owned-alloc/")
            .await?;

        if expected == Duration::ZERO {
            response.assert_cache_control(CachePolicy::NoCaching, env.config());
        } else {
            assert_ttl(&response, expected);
        }

        let html = response.text().await?;
        if empty {
            assert!(html.is_empty());
        } else {
            assert!(html.contains("Std alternative"));
            assert!(html.contains("Unmaintained"));
            let page = kuchikiki::parse_html().one(format!("<ul>{html}</ul>"));
            assert_eq!(
                page.select("ul > li.crate-warning > a.warn")
                    .unwrap()
                    .count(),
                2
            );
            assert_eq!(
                page.select("li.crate-warning + li.crate-warning")
                    .unwrap()
                    .count(),
                1
            );
        }

        mock_server.assert_async().await;
        Ok(())
    }

    #[test_case(false; "uncached 404")]
    #[test_case(true; "no-store 404")]
    #[tokio::test(flavor = "multi_thread")]
    async fn crate_warnings_does_not_cache_uncached_replacement_404(no_store: bool) -> Result<()> {
        let cache_control = no_store.then(|| CacheControl::new().with_no_store());

        let mock_server = WarningSourceMock::new()
            .await?
            .mock_rustsec(OWNED_ALLOC)
            .cache_control(CacheControl::new().with_max_age(std::time::Duration::from_secs(600)))
            .mock()
            .await
            .with_replacements_and_status(iter::empty(), cache_control, StatusCode::NOT_FOUND)
            .await;

        let env = TestEnvironment::builder()
            .std_replacements_config(mock_server.std_replacements_config(None))
            .rustsec_config(mock_server.rustsec_config(None))
            .build()
            .await?;

        let response = env
            .web_app()
            .await
            .assert_success("/-/partial/crate-warnings/owned-alloc/")
            .await?;

        response.assert_cache_control(CachePolicy::NoCaching, env.config());

        let html = response.text().await?;
        assert!(html.contains("Unmaintained"));
        assert!(!html.contains("Std alternative"));

        mock_server.assert_async().await;
        Ok(())
    }

    #[test_case(true; "replacement unavailable")]
    #[test_case(false; "rustsec unavailable")]
    #[tokio::test(flavor = "multi_thread")]
    async fn crate_warnings_preserves_healthy_source(replacement_fails: bool) -> Result<()> {
        let mut mocks = WarningSourceMock::new().await?;
        if replacement_fails {
            mocks = mocks
                .mock_rustsec(OWNED_ALLOC)
                .maybe_cache_control(None)
                .mock()
                .await
                .with_replacements_and_status(iter::empty(), None, StatusCode::SERVICE_UNAVAILABLE)
                .await;
        } else {
            mocks = mocks
                .with_replacement(OWNED_ALLOC, std_replacement("Use std"), None)
                .await
                .mock_rustsec(OWNED_ALLOC)
                .status_code(StatusCode::SERVICE_UNAVAILABLE)
                .mock()
                .await;
        }
        let env = TestEnvironment::builder()
            .std_replacements_config(mocks.std_replacements_config(None))
            .rustsec_config(mocks.rustsec_config(None))
            .build()
            .await?;
        let response = env
            .web_app()
            .await
            .assert_success("/-/partial/crate-warnings/owned-alloc/")
            .await?;
        response.assert_cache_control(CachePolicy::NoCaching, env.config());
        let html = response.text().await?;
        assert_eq!(html.contains("Unmaintained"), replacement_fails);
        assert_eq!(html.contains("Std alternative"), !replacement_fails);
        mocks.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn crate_warnings_accepts_missing_clients() -> Result<()> {
        let response =
            super::crate_warnings(None, None, crate::extractors::Path("unknown".parse()?))
                .await?
                .into_response();
        assert_eq!(response.status(), http::StatusCode::OK);
        assert!(response.headers().get(CACHE_CONTROL).is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn crate_warnings_partial_returns_unmaintained_advisory() -> Result<()> {
        let mock_server = WarningSourceMock::new()
            .await?
            .mock_rustsec(OWNED_ALLOC)
            .maybe_cache_control(None)
            .mock()
            .await;
        let env = TestEnvironment::builder()
            .rustsec_config(mock_server.rustsec_config(None))
            .build()
            .await?;

        assert!(env.rustsec().is_some());
        let web = env.web_app().await;
        let response = web
            .assert_success("/-/partial/crate-warnings/owned-alloc/")
            .await?;
        assert_ttl(&response, Duration::from_mins(10));
        let page = kuchikiki::parse_html().one(response.text().await?);
        let link = page.select_first("a.pure-menu-link.warn").unwrap();
        assert_eq!(link.text_contents().trim(), "Unmaintained");
        {
            let attrs = link.attributes.borrow();
            assert_eq!(
                attrs.get("href"),
                Some("https://rustsec.org/advisories/RUSTSEC-2026-0299.html")
            );
            assert_eq!(attrs.get("title"), Some("`owned-alloc` is unmaintained"));
        }
        mock_server.assert_async().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn crate_warnings_partial_returns_replacement() -> Result<()> {
        let replacement = ReplacementDetails::new(
            "Use std::sync::LazyLock (stable since Rust 1.80).",
            "https://doc.rust-lang.org/std/sync/struct.LazyLock.html".parse()?,
        );
        const LAZY_STATIC: KrateName = KrateName::from_static("lazy_static");

        let mock_server = WarningSourceMock::new()
            .await?
            .with_replacement(LAZY_STATIC, replacement.clone(), None)
            .await;

        let env = TestEnvironment::builder()
            .std_replacements_config(
                mock_server.std_replacements_config(Some(Duration::from_secs(90))),
            )
            .build()
            .await?;
        assert!(env.std_replacements().is_some());

        let web = env.web_app().await;

        let response = web
            .assert_success("/-/partial/crate-warnings/lazy_static/")
            .await?;
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_ttl(&response, Duration::from_secs(90));
        let page = kuchikiki::parse_html().one(response.text().await?);
        let link = page.select_first("a.pure-menu-link.warn").unwrap();
        assert_eq!(link.text_contents().trim(), "Std alternative");

        {
            let attrs = link.attributes.borrow();
            assert_eq!(
                attrs.get("href"),
                Some(replacement.url().to_string().as_str())
            );
            assert_eq!(attrs.get("title"), Some(replacement.description()));
        }
        mock_server.assert_async().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn crate_warnings_partial_without_replacement_is_empty() -> Result<()> {
        let env = TestEnvironment::new().await?;
        assert!(env.rustsec().is_none());
        assert!(env.std_replacements().is_none());
        let web = env.web_app().await;
        let response = web
            .assert_success("/-/partial/crate-warnings/unknown/")
            .await?;
        response.assert_cache_control(CachePolicy::NoCaching, env.config());
        assert!(response.text().await?.is_empty());
        Ok(())
    }

    #[test]
    fn crate_warnings_escapes_description() -> Result<()> {
        use askama::Template as _;
        let description = "Use <std> & \"quotes\"";
        let warnings = super::CrateWarnings {
            ttl: std::time::Duration::ZERO,
            unmaintained: None,
            replacement: Some(Arc::new(ReplacementDetails::new(
                description,
                "https://example.com".parse()?,
            ))),
        };
        let html = warnings.render()?;
        assert!(!html.contains("<std>"));
        let page = kuchikiki::parse_html().one(html);
        let link = page.select_first("a").unwrap();
        assert_eq!(link.attributes.borrow().get("title"), Some(description));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn abnormalities_partial_renders_configured_link() -> Result<()> {
        let env = TestEnvironment::new().await?;

        let mut conn = env.async_conn().await?;
        set_config(
            &mut conn,
            ConfigName::Abnormality,
            Abnormality {
                url: "https://example.com/maintenance"
                    .parse::<EscapedURI>()
                    .unwrap(),
                text: "Scheduled maintenance".into(),
                explanation: Some("Planned maintenance is in progress.".into()),
            },
        )
        .await?;

        let web = env.web_app().await;
        let page = kuchikiki::parse_html().one(
            web.assert_success_cached(
                "/-/partial/abnormalities/",
                CachePolicy::LongerInCdnAndBrowser,
                env.config(),
            )
            .await?
            .text()
            .await?,
        );
        let alert = page
            .select("a.pure-menu-link.warn")
            .unwrap()
            .next()
            .expect("missing abnormality");

        assert_eq!(alert.attributes.borrow().get("href"), Some("/-/status/"));
        assert!(alert.text_contents().trim().is_empty());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn abnormalities_partial_renders_queue_alert() -> Result<()> {
        let mut queue_config = docs_rs_build_queue::Config::test_config()?;
        queue_config.length_warning_threshold = 1;
        let env = TestEnvironment::builder()
            .build_queue_config(queue_config)
            .build()
            .await?;
        let queue = env.build_queue()?.clone();

        for idx in 0..2 {
            let name = KrateName::from_str(&format!("queued-crate-{idx}"))?;
            queue.add_crate(&name, &V1, 0).await?;
        }

        let web = env.web_app().await;
        let page = kuchikiki::parse_html().one(
            web.assert_success_cached(
                "/-/partial/abnormalities/",
                CachePolicy::LongerInCdnAndBrowser,
                env.config(),
            )
            .await?
            .text()
            .await?,
        );
        let alert = page
            .select("a.pure-menu-link.warn")
            .unwrap()
            .next()
            .expect("missing queue alert");

        assert_eq!(alert.attributes.borrow().get("href"), Some("/-/status/"));
        assert!(alert.text_contents().trim().is_empty());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn about_status_page_renders_abnormality_details() -> Result<()> {
        let env = TestEnvironment::new().await?;

        let mut conn = env.async_conn().await?;
        set_config(
            &mut conn,
            ConfigName::Abnormality,
            Abnormality {
                url: "https://example.com/maintenance"
                    .parse::<EscapedURI>()
                    .unwrap(),
                text: "Scheduled maintenance".into(),
                explanation: Some("Planned maintenance is in progress.".into()),
            },
        )
        .await?;
        drop(conn);

        let web = env.web_app().await;
        let page = kuchikiki::parse_html().one(
            web.assert_success_cached(
                "/-/status/",
                CachePolicy::ShortInCdnAndBrowser,
                env.config(),
            )
            .await?
            .text()
            .await?,
        );

        let body_text = page.text_contents();
        assert!(body_text.contains("Scheduled maintenance"));
        assert!(body_text.contains("Planned maintenance is in progress."));

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn about_status_page_shows_no_abnormalities_when_clean() -> Result<()> {
        let env = TestEnvironment::new().await?;
        let web = env.web_app().await;

        let page = kuchikiki::parse_html().one(
            web.assert_success_cached(
                "/-/status/",
                CachePolicy::ShortInCdnAndBrowser,
                env.config(),
            )
            .await?
            .text()
            .await?,
        );

        let body_text = page.text_contents();
        assert!(body_text.contains("No abnormalities detected currently."));
        assert_eq!(
            page.select(".about h3").unwrap().count(),
            0,
            "should not render any abnormality headings"
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn about_status_page_renders_html_explanation() -> Result<()> {
        let env = TestEnvironment::new().await?;

        let mut conn = env.async_conn().await?;
        set_config(
            &mut conn,
            ConfigName::Abnormality,
            Abnormality {
                url: "https://example.com/maintenance"
                    .parse::<EscapedURI>()
                    .unwrap(),
                text: "Scheduled maintenance".into(),
                explanation: Some(
                    "Planned maintenance is <em>in progress</em>. See <a href=\"/details\">details</a>.".into(),
                ),
            },
        )
        .await?;
        drop(conn);

        let web = env.web_app().await;
        let html = web
            .assert_success_cached(
                "/-/status/",
                CachePolicy::ShortInCdnAndBrowser,
                env.config(),
            )
            .await?
            .text()
            .await?;
        let page = kuchikiki::parse_html().one(html.clone());

        // The <em> tag should be rendered as an actual HTML element, not escaped.
        assert!(
            html.contains("<em>in progress</em>"),
            "HTML in explanation should be rendered unescaped"
        );

        // The <a> tag should be rendered as an actual link.
        let link = page
            .select(".about p a[href='/details']")
            .unwrap()
            .next()
            .expect("explanation should contain a rendered <a> link");
        assert!(link.text_contents().contains("details"));

        Ok(())
    }
}
