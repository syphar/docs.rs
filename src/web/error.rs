use crate::{
    db::PoolError,
    storage::PathNotFoundError,
    web::{AxumErrorPage, cache::CachePolicy, escaped_uri::EscapedURI, releases::Search},
};
use anyhow::{Result, anyhow};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response as AxumResponse},
};
use std::borrow::Cow;
use tracing::error;

#[derive(Debug, thiserror::Error)]
pub enum AxumNope {
    #[error("Requested resource not found")]
    ResourceNotFound,
    #[error("Requested build not found")]
    BuildNotFound,
    #[error("Requested crate not found")]
    CrateNotFound,
    #[error("Requested owner not found")]
    OwnerNotFound,
    #[error("Requested crate does not have specified version")]
    VersionNotFound,
    #[error("Requested release doesn't have docs for the given target")]
    TargetNotFound,
    #[error("Search yielded no results")]
    NoResults,
    #[error("Unauthorized: {0}")]
    Unauthorized(&'static str),
    #[error("internal error")]
    InternalError(anyhow::Error),
    #[error("bad request")]
    BadRequest(anyhow::Error),
    #[error("redirect")]
    Redirect(EscapedURI, CachePolicy),
}

// FUTURE: Ideally, the split between the 3 kinds of responses would
// be done by having multiple nested enums in the first place instead
// of just `AxumNope`, to keep everything statically type-checked
// throughout instead of having the potential for a runtime error.

impl AxumNope {
    fn into_error_info(self) -> ErrorInfo {
        match self {
            AxumNope::ResourceNotFound => {
                // user tried to navigate to a resource (doc page/file) that doesn't exist
                ErrorInfo {
                    title: "The requested resource does not exist",
                    message: "no such resource".into(),
                    status: StatusCode::NOT_FOUND,
                }
            }
            AxumNope::BuildNotFound => ErrorInfo {
                title: "The requested build does not exist",
                message: "no such build".into(),
                status: StatusCode::NOT_FOUND,
            },
            AxumNope::TargetNotFound => {
                // user tried to navigate to a target that doesn't exist
                ErrorInfo {
                    title: "The requested target does not exist",
                    message: "no such target".into(),
                    status: StatusCode::NOT_FOUND,
                }
            }
            AxumNope::CrateNotFound => {
                // user tried to navigate to a crate that doesn't exist
                // TODO: Display the attempted crate and a link to a search for said crate
                ErrorInfo {
                    title: "The requested crate does not exist",
                    message: "no such crate".into(),
                    status: StatusCode::NOT_FOUND,
                }
            }
            AxumNope::OwnerNotFound => ErrorInfo {
                title: "The requested owner does not exist",
                message: "no such owner".into(),
                status: StatusCode::NOT_FOUND,
            },
            AxumNope::VersionNotFound => {
                // user tried to navigate to a crate with a version that does not exist
                // TODO: Display the attempted crate and version
                ErrorInfo {
                    title: "The requested version does not exist",
                    message: "no such version for this crate".into(),
                    status: StatusCode::NOT_FOUND,
                }
            }
            AxumNope::NoResults => {
                // user did a search with no search terms
                unreachable!()
            }
            AxumNope::BadRequest(source) => ErrorInfo {
                title: "Bad request",
                message: Cow::Owned(source.to_string()),
                status: StatusCode::BAD_REQUEST,
            },
            AxumNope::Unauthorized(what) => ErrorInfo {
                title: "Unauthorized",
                message: what.into(),
                status: StatusCode::UNAUTHORIZED,
            },
            AxumNope::InternalError(source) => {
                crate::utils::report_error(&source);
                ErrorInfo {
                    title: "Internal Server Error",
                    message: Cow::Owned(source.to_string()),
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                }
            }
            AxumNope::Redirect(_target, _cache_policy) => unreachable!(),
        }
    }
}

struct ErrorInfo {
    // For the title of the page
    pub title: &'static str,
    // The error message, displayed as a description
    pub message: Cow<'static, str>,
    // The status code of the response
    pub status: StatusCode,
}

fn redirect_with_policy(target: EscapedURI, cache_policy: CachePolicy) -> AxumResponse {
    match super::axum_cached_redirect(target, cache_policy) {
        Ok(response) => response.into_response(),
        Err(err) => AxumNope::InternalError(err).into_response(),
    }
}

impl IntoResponse for AxumNope {
    fn into_response(self) -> AxumResponse {
        match self {
            AxumNope::NoResults => {
                // user did a search with no search terms
                Search {
                    title: "No results given for empty search query".to_owned(),
                    status: StatusCode::NOT_FOUND,
                    ..Default::default()
                }
                .into_response()
            }
            AxumNope::Redirect(target, cache_policy) => redirect_with_policy(target, cache_policy),
            _ => {
                let ErrorInfo {
                    title,
                    message,
                    status,
                } = self.into_error_info();
                AxumErrorPage {
                    title,
                    message,
                    status,
                }
                .into_response()
            }
        }
    }
}

/// `AxumNope` but generating error responses in JSON (for API).
pub(crate) struct JsonAxumNope(pub AxumNope);

impl IntoResponse for JsonAxumNope {
    fn into_response(self) -> AxumResponse {
        match self.0 {
            AxumNope::NoResults => {
                // user did a search with no search terms; invalid,
                // return 404
                StatusCode::NOT_FOUND.into_response()
            }
            AxumNope::Redirect(target, cache_policy) => redirect_with_policy(target, cache_policy),
            _ => {
                let ErrorInfo {
                    title,
                    message,
                    status,
                } = self.0.into_error_info();
                (
                    status,
                    Json(serde_json::json!({
                        "title": title,
                        "message": message,
                    })),
                )
                    .into_response()
            }
        }
    }
}

impl From<anyhow::Error> for AxumNope {
    fn from(err: anyhow::Error) -> Self {
        match err.downcast::<AxumNope>() {
            Ok(axum_nope) => axum_nope,
            Err(err) => match err.downcast::<PathNotFoundError>() {
                Ok(_) => AxumNope::ResourceNotFound,
                Err(err) => AxumNope::InternalError(err),
            },
        }
    }
}

impl From<sqlx::Error> for AxumNope {
    fn from(err: sqlx::Error) -> Self {
        AxumNope::InternalError(anyhow!(err))
    }
}

impl From<PoolError> for AxumNope {
    fn from(err: PoolError) -> Self {
        AxumNope::InternalError(anyhow!(err))
    }
}

pub(crate) type AxumResult<T> = Result<T, AxumNope>;
pub(crate) type JsonAxumResult<T> = Result<T, JsonAxumNope>;

#[cfg(test)]
mod tests {
    use super::{AxumNope, EscapedURI, IntoResponse};
    use crate::test::{AxumResponseTestExt, AxumRouterTestExt, async_wrapper};
    use crate::web::cache::CachePolicy;
    use http::Uri;
    use kuchikiki::traits::TendrilSink;
    use test_case::test_case;

    #[test]
    fn test_redirect_error_encodes_url_path() {
        let response = AxumNope::Redirect(
            EscapedURI::from_path("/something>"),
            CachePolicy::ForeverInCdnAndBrowser,
        )
        .into_response();

        assert_eq!(response.status(), 302);
        assert_eq!(response.headers().get("Location").unwrap(), "/something%3E");
    }

    #[test_case("/something" => "/something")]
    #[test_case("/something>" => "/something%3E")]
    fn test_escaped_uri_encodes_from_path(input: &str) -> String {
        let escaped = EscapedURI::from_path(input);
        escaped.path().to_owned()
    }

    #[test_case("/something" => "/something"; "plain path")]
    #[test_case("/somethingäöü" => "/something%C3%A4%C3%B6%C3%BC"; "path with umlauts")]
    fn test_escaped_uri_encodes_path_from_uri(path: &str) -> String {
        let uri: Uri = path.parse().unwrap();
        let escaped = EscapedURI::from_uri(uri);
        escaped.path().to_string()
    }

    #[test]
    fn test_escaped_uri_from_uri_with_query_args() {
        let uri: Uri = "/something?key=value&foo=bar".parse().unwrap();
        let escaped = EscapedURI::from_uri(uri);
        assert_eq!(escaped.path(), "/something");
        assert_eq!(escaped.query(), Some("key=value&foo=bar"));
    }

    #[test_case("/something>")]
    #[test_case("/something?key=<value&foo=\rbar")]
    fn test_escaped_uri_encodes_path_from_uri_invalid(input: &str) {
        // things that are invalid URIs should error out,
        // so are unusable for EscapedURI::from_uri`
        //
        // More to test if my assumption is correct that we don't have to re-encode.
        assert!(input.parse::<Uri>().is_err());
    }

    #[test_case(
        "/something", "key=value&foo=bar"
        => ("/something".into(), "key=value&foo=bar".into());
        "plain convert"
    )]
    #[test_case(
        "/something", "value=foo\rbar&key=<value"
        => ("/something".into(), "value=foo%0Dbar&key=%3Cvalue".into());
        "invalid query gets re-encoded without error"
    )]
    fn test_escaped_uri_from_raw_query(path: &str, query: &str) -> (String, String) {
        let uri = EscapedURI::from_path_and_raw_query(path, Some(query));

        (uri.path().to_owned(), uri.query().unwrap().to_owned())
    }

    #[test]
    fn test_escaped_uri_from_query() {
        let uri =
            EscapedURI::from_path_and_query("/something", &[("key", "value"), ("foo", "bar")]);

        assert_eq!(uri.path(), "/something");
        assert_eq!(uri.query(), Some("key=value&foo=bar"));
    }

    #[test]
    fn test_escaped_uri_from_query_with_chars_to_encode() {
        let uri =
            EscapedURI::from_path_and_query("/something", &[("key", "value>"), ("foo", "\rbar")]);

        assert_eq!(uri.path(), "/something");
        assert_eq!(uri.query(), Some("key=value%3E&foo=%0Dbar"));
    }

    #[test]
    fn test_escaped_uri_append_query_pairs_without_path() {
        let uri = Uri::builder().build().unwrap();

        let parts = uri.into_parts();
        // `append_query_pairs` has a special case when path_and_query is `None`,
        // which I want to test here.
        assert!(parts.path_and_query.is_none());

        // also tests appending query pairs if there are no existing query args
        let uri = EscapedURI::from_uri(Uri::from_parts(parts).unwrap())
            .append_query_pairs(&[("foo", "bar"), ("bar", "baz")]);

        assert_eq!(uri.path(), "/");
        assert_eq!(uri.query(), Some("foo=bar&bar=baz"));
    }

    #[test]
    fn test_escaped_uri_append_query_pairs() {
        let uri = EscapedURI::from_path_and_query("/something", &[("key", "value")])
            .append_query_pairs(&[("foo", "bar"), ("bar", "baz")])
            .append_query_pair("last", "one");

        assert_eq!(uri.path(), "/something");
        assert_eq!(uri.query(), Some("key=value&foo=bar&bar=baz&last=one"));
    }

    #[test]
    fn check_404_page_content_crate() {
        async_wrapper(|env| async move {
            let page = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/crate-which-doesnt-exist")
                    .await?
                    .text()
                    .await?,
            );
            assert_eq!(page.select("#crate-title").unwrap().count(), 1);
            assert_eq!(
                page.select("#crate-title")
                    .unwrap()
                    .next()
                    .unwrap()
                    .text_contents(),
                "The requested crate does not exist",
            );

            Ok(())
        });
    }

    #[test]
    fn check_404_page_content_resource() {
        async_wrapper(|env| async move {
            let page = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/resource-which-doesnt-exist.js")
                    .await?
                    .text()
                    .await?,
            );
            assert_eq!(page.select("#crate-title").unwrap().count(), 1);
            assert_eq!(
                page.select("#crate-title")
                    .unwrap()
                    .next()
                    .unwrap()
                    .text_contents(),
                "The requested resource does not exist",
            );

            Ok(())
        });
    }

    #[test]
    fn check_400_page_content_not_semver_version() {
        async_wrapper(|env| async move {
            env.fake_release().await.name("dummy").create().await?;

            let response = env.web_app().await.get("/dummy/not-semver").await?;
            assert_eq!(response.status(), 400);

            let page = kuchikiki::parse_html().one(response.text().await?);
            assert_eq!(page.select("#crate-title").unwrap().count(), 1);
            assert_eq!(
                page.select("#crate-title")
                    .unwrap()
                    .next()
                    .unwrap()
                    .text_contents(),
                "Bad request"
            );

            Ok(())
        });
    }

    #[test]
    fn check_404_page_content_nonexistent_version() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("1.0.0")
                .create()
                .await?;
            let page = kuchikiki::parse_html()
                .one(env.web_app().await.get("/dummy/2.0").await?.text().await?);
            assert_eq!(page.select("#crate-title").unwrap().count(), 1);
            assert_eq!(
                page.select("#crate-title")
                    .unwrap()
                    .next()
                    .unwrap()
                    .text_contents(),
                "The requested version does not exist",
            );

            Ok(())
        });
    }

    #[test]
    fn check_404_page_content_any_version_all_yanked() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("1.0.0")
                .yanked(true)
                .create()
                .await?;
            let page = kuchikiki::parse_html()
                .one(env.web_app().await.get("/dummy/*").await?.text().await?);
            assert_eq!(page.select("#crate-title").unwrap().count(), 1);
            assert_eq!(
                page.select("#crate-title")
                    .unwrap()
                    .next()
                    .unwrap()
                    .text_contents(),
                "The requested version does not exist",
            );

            Ok(())
        });
    }
}
