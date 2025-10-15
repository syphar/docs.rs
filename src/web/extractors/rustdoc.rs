//! special rustdoc extractors

use anyhow::anyhow;
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, MatchedPath},
    http::{Uri, request::Parts},
};
use itertools::Itertools as _;
use serde::Deserialize;

use crate::web::{ReqVersion, error::AxumNope, extractors::Path};

/// can extract rustdoc parameters from path and uri.
///
/// includes parsing / interpretation logic using the crate metadata.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RustdocParams {
    pub(crate) name: String,
    pub(crate) version: ReqVersion,
    pub(crate) target: Option<String>,
    pub(crate) path: Option<String>,
}

/// the parameters that might come as url parameters via route.
#[derive(Deserialize, Debug)]
struct UrlParams {
    pub(crate) name: String,
    pub(crate) version: ReqVersion,
    pub(crate) target: Option<String>,
    pub(crate) path: Option<String>,
}

impl<S> FromRequestParts<S> for RustdocParams
where
    S: Send + Sync,
{
    type Rejection = AxumNope;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Path(mut params) = parts.extract::<Path<UrlParams>>().await?;
        let uri = parts.extract::<Uri>().await.expect("infallible extractor");
        let matched_path = parts
            .extract::<MatchedPath>()
            .await
            .map_err(|err| anyhow!("couldn't extract matched path: {err}"))?;

        let static_route_suffix = find_static_route_suffix(matched_path.as_str(), uri.path());

        if let Some(static_suffix) = static_route_suffix {
            if let Some(ref mut path) = params.path
                && !path.is_empty()
            {
                path.push('/');
                path.push_str(&static_suffix);
            } else {
                params.path = Some(static_suffix);
            }
        }

        Ok(RustdocParams {
            name: params.name,
            version: params.version,
            target: params.target,
            path: params.path,
        })
    }
}

/// we sometimes have routes with a static suffix.
///
/// For example: `/{name}/{version}/help.html`
/// In this case, we won't get the `help.html` part in our `path` parameter, since there is
/// no `{*path}` in the route.
///
/// We're working around that by re-attaching the static suffix. This function is to find the
/// shared suffix between the route and the actual path.
fn find_static_route_suffix<'a, 'b>(route: &'a str, path: &'b str) -> Option<String> {
    // TODO: optimization: return Option<'a str> directly, avoiding allocation

    // TODO: compare component count. if it doesn't match, return None. But only if there is no
    // `{*path}` component.

    let mut suffix: Vec<&'a str> = Vec::new();

    for (route_component, path_component) in route.rsplit('/').zip(path.rsplit('/')) {
        if route_component.starts_with('{') && route_component.ends_with('}') {
            // we've reached a dynamic component in the route, stop here
            break;
        }

        if route_component != path_component {
            // components don't match, no static suffix.
            // Everything has to match up to the last dynamic component.
            return None;
        }

        // components match, continue to the next component
        suffix.push(route_component);
    }

    if suffix.is_empty() {
        None
    } else {
        if suffix.len() == 1 && suffix[0].is_empty() {
            // special case: if the suffix is just empty, return None
            None
        } else {
            Some(suffix.iter().rev().join("/"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::{AxumResponseTestExt, AxumRouterTestExt};
    use axum::{Router, routing::get};
    use test_case::test_case;

    #[test_case(
        "/{name}/{version}/help/some.html",
        "/foo/1.2.3/help/some.html"
        => Some("help/some.html".into());
        "suffix with path"
    )]
    #[test_case("/{name}/{version}/help.html", "/foo/1.2.3/help.html" => Some("help.html".into()); "simple suffix")]
    #[test_case("help.html", "help.html" => Some("help.html".into()); "simple suffix without other components")]
    #[test_case("/{name}/{version}/help/", "/foo/1.2.3/help/" => Some("help/".into()); "suffix is folder")]
    #[test_case("{name}/{version}/help/", "foo/1.2.3/help/" => Some("help/".into()); "without leading slash")]
    #[test_case("/{name}/{version}/{*path}", "/foo/1.2.3/help.html" => None; "no suffix in route")]
    #[test_case("/{name}/{version}/help.html", "/foo/1.2.3/other.html" => None; "different suffix")]
    #[test_case(
        "/{name}/{version}/some/help.html",
        "/foo/1.2.3/other/help.html"
        => None;
        "different suffix later"
    )]
    #[test_case("", "" => None; "empty strings")]
    #[test_case("/", "" => None; "one slash, one empty")]
    fn test_find_static_route_suffix<'a, 'b>(route: &'a str, path: &'b str) -> Option<String> {
        find_static_route_suffix(route, path)
    }

    #[test_case(
        "/{name}/{version}",
        "/krate/latest",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            target: None,
            path: None
        };
        "just name and version"
    )]
    #[test_case(
        "/{name}/{version}/{*path}",
        "/krate/latest/static.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            target: None,
            path: Some("static.html".into())
        };
        "name, version, path extract"
    )]
    #[test_case(
        "/{name}/{version}/{path}/static.html",
        "/krate/latest/path_add/static.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            target: None,
            path: Some("path_add/static.html".into())
        };
        "name, version, path extract, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/static.html",
        "/krate/latest/static.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            target: None,
            path: Some("static.html".into())
        };
        "name, version, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}",
        "/krate/1.2.3/some-target",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            target: Some("some-target".into()),
            path: None
        };
        "name, version, target"
    )]
    #[test_case(
        "/{name}/{version}/{target}/folder/something.html",
        "/krate/1.2.3/some-target/folder/something.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            target: Some("some-target".into()),
            path: Some("folder/something.html".into())
        };
        "name, version, target, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}/",
        "/krate/1.2.3/some-target/",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            target: Some("some-target".into()),
            path: None
        };
        "name, version, target trailing slash"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{*path}",
        "/krate/1.2.3/some-target/some/path/to/a/file.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            target: Some("some-target".into()),
            path: Some("some/path/to/a/file.html".into())
        };
        "name, version, target, path"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{path}/path/to/a/file.html",
        "/krate/1.2.3/some-target/path_add/path/to/a/file.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            target: Some("some-target".into()),
            path: Some("path_add/path/to/a/file.html".into())
        };
        "name, version, target, path, static suffix"
    )]
    #[tokio::test]
    async fn test_extract_rustdoc_params_from_request(
        route: &str,
        path: &str,
        expected: RustdocParams,
    ) -> anyhow::Result<()> {
        let app = Router::new().route(
            route,
            get(|params: RustdocParams| async move { format!("{:?}", params) }),
        );

        let res = app.get(path).await?;
        assert!(res.status().is_success());
        assert_eq!(res.text().await?, format!("{:?}", expected));

        Ok(())
    }
}
