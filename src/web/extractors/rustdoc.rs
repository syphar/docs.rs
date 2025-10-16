//! special rustdoc extractors

use anyhow::anyhow;
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, MatchedPath},
    http::{Uri, request::Parts},
};
use itertools::Itertools as _;
use serde::Deserialize;

use crate::web::{MetaData, ReqVersion, error::AxumNope, extractors::Path};

/// can extract rustdoc parameters from path and uri.
///
/// includes parsing / interpretation logic using the crate metadata.
///
/// TODO: features to add?
/// * generate standard URLs for these params? Same for the parsed version?
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
            name: params.name.trim().to_owned(),
            version: params.version,
            target: params.target.map(|t| t.trim().to_owned()),
            path: params.path.map(|p| p.trim().to_owned()),
        })
    }
}

impl RustdocParams {
    pub(crate) fn parse_from_metadata(self, metadata: &MetaData) -> ParsedRustdocParams {
        let default_target = metadata.default_target.as_ref();
        self.parse(default_target, metadata.doc_targets.iter().flatten())
    }

    /// parse the params, mostly split the path into the target and the inner path.
    /// A path can looks like
    /// * `/:crate/:version/:target/:*path`
    /// * `/:crate/:version/:*path`
    ///
    /// Since our route matching just contains `/:crate/:version/*path` we need a way to figure
    /// out if we have a target in the path or not.
    ///
    /// We do this by comparing the first part of the path with the list of targets for that crate.
    pub(crate) fn parse<I, S, T>(
        mut self,
        default_target: Option<T>,
        doc_targets: I,
    ) -> ParsedRustdocParams
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        T: AsRef<str>,
    {
        // TODO: optimization: less owned variables, more references
        // TODO: nicer target logic
        let default_target = default_target.map(|t| t.as_ref().to_owned());

        let doc_targets = doc_targets
            .into_iter()
            .map(|s| s.as_ref().to_owned())
            .collect::<Vec<_>>();

        dbg!(&self.path);
        let mut new_path = if let Some(ref path) = self.path {
            path.trim_start_matches('/').trim().to_string()
        } else {
            String::new()
        };

        dbg!(&new_path);

        let mut new_target: Option<String> = None;

        dbg!(&self);

        if let Some(given_target) = dbg!(self.target.take())
            && !given_target.trim().is_empty()
        {
            let given_target = given_target.trim();
            dbg!(&given_target);
            // if a target is given in a separate url parameter, check if it's a target we
            // know about. If yes, keep it, if not, make it part of the path.
            if doc_targets.iter().any(|s| s == given_target) {
                dbg!("known target");
                new_target = Some(given_target.into());
            } else {
                new_target = None;
                if !new_path.is_empty() {
                    new_path = format!("{}/{}", given_target, new_path);
                } else {
                    new_path = format!("{}/", given_target);
                }
            }
        } else {
            // there is no separate target component given.
            // we look at the first component of the path and see if it matches a target.

            if let Some(pos) = new_path.find('/') {
                let potential_target = dbg!(&new_path[..pos]);

                if doc_targets.iter().any(|s| s == potential_target) {
                    new_target = Some(potential_target.to_owned());
                    new_path = new_path
                        .get((pos + 1)..)
                        .map(ToOwned::to_owned)
                        .unwrap_or_default();
                }
            } else {
                // no slash in the path, can be target or inner path
                if doc_targets.iter().any(|s| s == &new_path) {
                    new_target = Some(new_path.to_owned());
                    new_path.clear();
                } else {
                    new_target = None;
                    // new_path stays the same
                }
            };
        }

        self.target = if new_target == default_target {
            None
        } else {
            new_target
        };
        self.path = Some(new_path);

        ParsedRustdocParams {
            doc_targets,
            default_target,
            inner: self,
        }
    }

    pub(crate) fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("")
    }

    pub(crate) fn path_is_folder(&self) -> bool {
        self.path
            .as_deref()
            .map(|p| p.is_empty() || p.ends_with('/'))
            .unwrap_or(true)
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.path.as_deref().and_then(|p| {
            p.rsplit_once('.').and_then(|(_, ext)| {
                if ext.contains('/') {
                    // to handle cases like `foo.html/bar` where I want `None`
                    None
                } else {
                    Some(ext)
                }
            })
        })
    }

    pub(crate) fn storage_path(&'_ self) -> String {
        // FIXME: drop target from path if it's the default target.
        // FIXME: remove self.target parameter when it's the default target
        let mut storage_path = if let Some(ref target) = self.target {
            format!("{}/{}", target, self.path.as_deref().unwrap_or_default())
        } else {
            self.path.clone().unwrap_or_default()
        };

        if self.path_is_folder() {
            if !storage_path.ends_with('/') {
                // this can happen in the case of an empty path
                storage_path.push('/');
            }
            storage_path.push_str("index.html");
        }

        storage_path
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedRustdocParams {
    inner: RustdocParams,
    doc_targets: Vec<String>,
    default_target: Option<String>,
}

impl ParsedRustdocParams {
    pub(crate) fn name(&self) -> &str {
        &self.inner.name
    }
    pub(crate) fn version(&self) -> &ReqVersion {
        &self.inner.version
    }
    pub(crate) fn storage_path(&'_ self) -> String {
        self.inner.storage_path()
    }
    pub(crate) fn target(&self) -> Option<&str> {
        self.inner.target.as_deref()
    }
    pub(crate) fn path_is_folder(&self) -> bool {
        self.inner.path_is_folder()
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.inner.file_extension()
    }

    pub(crate) fn path(&self) -> &str {
        // in our logic, when `parse` is done, the path is never `None`.
        self.inner.path.as_deref().unwrap_or_default()
    }

    pub(crate) fn update<F>(self, f: F) -> Self
    where
        F: FnOnce(&mut RustdocParams),
    {
        let mut this = self;
        f(&mut this.inner);
        this.inner.parse(
            this.default_target,
            this.doc_targets.iter().map(String::as_str),
        )
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
    } else if suffix.len() == 1 && suffix[0].is_empty() {
        // special case: if the suffix is just empty, return None
        None
    } else {
        Some(suffix.iter().rev().join("/"))
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
    fn test_find_static_route_suffix(route: &str, path: &str) -> Option<String> {
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

    #[test_case(None, None, None, ""; "super empty 1")]
    #[test_case(Some(""), Some(""), None, ""; "super empty 2")]
    // test cases when no separate "target" component was present in the params
    #[test_case(None, Some("/"), None, ""; "just slash")]
    #[test_case(None, Some("something"), None, "something"; "without trailing slash")]
    #[test_case(None, Some("/something"), None, "something"; "leading slash is cut")]
    #[test_case(None, Some("something/"), None, "something/"; "with trailing slash")]
    // a target is given, but as first component of the path, for routes without separate
    // "target" component
    #[test_case(None, Some("some-target-name"), Some("some-target-name"), ""; "just target without trailing slash")]
    #[test_case(None, Some("some-target-name/"), Some("some-target-name"), ""; "just target with trailing slash")]
    #[test_case(None, Some("some-target-name/one"), Some("some-target-name"), "one"; "target + one without trailing slash")]
    #[test_case(None, Some("some-target-name/one/"), Some("some-target-name"), "one/"; "target + one target with trailing slash")]
    #[test_case(None, Some("unknown-target-name/one/"), None, "unknown-target-name/one/"; "unknown target stays in path")]
    #[test_case(None, Some("some-target-name/some/inner/path"), Some("some-target-name"), "some/inner/path"; "all without trailing slash")]
    #[test_case(None, Some("some-target-name/some/inner/path/"), Some("some-target-name"), "some/inner/path/"; "all with trailing slash")]
    // here we have a separate target path parameter, we check it and use it accordingly
    #[test_case(Some("some-target-name"), None, Some("some-target-name"), ""; "actual target")]
    #[test_case(Some("some-target-name"), Some("inner/path.html"), Some("some-target-name"), "inner/path.html"; "actual target with path")]
    #[test_case(Some("some-target-name"), Some("inner/path/"), Some("some-target-name"), "inner/path/"; "actual target with path slash")]
    #[test_case(Some("unknown-target"), None, None, "unknown-target/"; "unknown target")]
    #[test_case(Some("unknown-target"), Some("inner/path.html"), None, "unknown-target/inner/path.html"; "unknown target with path")]
    #[test_case(Some("unknown-target"), Some("inner/path/"), None, "unknown-target/inner/path/"; "unknown target with path slash")]
    fn test_split_path_and_target_name(
        target: Option<&str>,
        path: Option<&str>,
        expected_target: Option<&str>,
        expected_path: &str,
    ) {
        static TARGETS: &[&str] = &["some-target-name", "other-target"];
        static DEFAULT_TARGET: &str = "some-target-name";

        let parsed = RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            target: target.map(|s| s.into()),
            path: path.map(|s| s.into()),
        }
        .parse(DEFAULT_TARGET.into(), TARGETS.iter());

        assert_eq!(parsed.name(), "krate");
        assert_eq!(parsed.version(), &ReqVersion::Latest);
        assert_eq!(parsed.target(), expected_target);
        assert_eq!(parsed.path(), expected_path);
        // FIXME: tests for storage path?
    }
}
