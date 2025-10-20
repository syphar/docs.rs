//! special rustdoc extractors

use std::borrow::Cow;

use anyhow::{Result, anyhow};
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, MatchedPath},
    http::{Uri, request::Parts},
};
use itertools::Itertools as _;
use serde::Deserialize;

use crate::web::{
    MetaData, ReqVersion,
    error::{AxumNope, EscapedURI},
    extractors::Path,
};

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
    pub(crate) doc_target: Option<String>,
    pub(crate) inner_path: Option<String>,
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
        let Path(mut params) = parts
            .extract::<Path<UrlParams>>()
            .await
            .map_err(|err| AxumNope::BadRequest(err.into()))?;

        // we're extracting the full path since we need to be able to extract static suffixes
        // that are not in the ``
        let uri = parts.extract::<Uri>().await.expect("infallible extractor");
        let uri_path = url_decode(uri.path()).map_err(AxumNope::BadRequest)?;

        let matched_path = parts
            .extract::<MatchedPath>()
            .await
            .map_err(|err| AxumNope::BadRequest(err.into()))?;
        let matched_route = url_decode(matched_path.as_str()).map_err(AxumNope::BadRequest)?;

        let static_route_suffix = find_static_route_suffix(&matched_route, &uri_path);

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
            doc_target: params.target.map(|t| t.trim().to_owned()),
            inner_path: params.path.map(|p| p.trim().to_owned()),
        })
    }
}

impl RustdocParams {
    pub(crate) fn with_name(self, name: &str) -> Self {
        RustdocParams {
            name: name.to_owned(),
            ..self
        }
    }

    pub(crate) fn with_version(self, version: &ReqVersion) -> Self {
        RustdocParams {
            version: version.clone(),
            ..self
        }
    }

    pub(crate) fn parse_with_metadata(self, metadata: &MetaData) -> Result<ParsedRustdocParams> {
        Ok(self.parse(
            metadata
                .default_target
                .as_deref()
                .ok_or_else(|| anyhow!("default target missing in release"))?,
            metadata
                .target_name
                .as_deref()
                .ok_or_else(|| anyhow!("target name missing in release"))?,
            metadata.doc_targets.iter().flatten(),
        ))
    }

    /// TODO: nice docstring
    pub(crate) fn parse<D, T, I, V>(
        mut self,
        default_target: D,
        target_name: T,
        doc_targets: I,
    ) -> ParsedRustdocParams
    where
        D: AsRef<str>,
        T: AsRef<str>,
        I: IntoIterator<Item = V>,
        V: AsRef<str>,
    {
        // TODO: optimization: less owned variables, more references
        // TODO: nicer target logic
        let default_target = default_target.as_ref().to_owned();
        debug_assert!(!default_target.is_empty());

        let doc_targets = doc_targets
            .into_iter()
            .map(|s| s.as_ref().to_owned())
            .collect::<Vec<_>>();

        debug_assert!(!doc_targets.is_empty());

        let mut new_path = if let Some(ref path) = self.inner_path {
            path.trim_start_matches('/').trim().to_string()
        } else {
            String::new()
        };

        let mut new_target: Option<String> = None;

        if let Some(given_target) = self.doc_target.take()
            && !given_target.trim().is_empty()
        {
            let given_target = given_target.trim();
            // if a target is given in a separate url parameter, check if it's a target we
            // know about. If yes, keep it, if not, make it part of the path.
            if doc_targets.iter().any(|s| s == given_target) {
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
                let potential_target = &new_path[..pos];

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

        self.doc_target = new_target;
        self.inner_path = Some(new_path);
        let target_name = target_name.as_ref().to_owned();
        debug_assert!(!target_name.is_empty());

        ParsedRustdocParams {
            doc_targets,
            default_target,
            target_name,
            inner: self,
        }
    }

    pub(crate) fn inner_path(&self) -> &str {
        if let Some(ref path) = self.inner_path {
            debug_assert!(!path.starts_with('/')); // we trim leading slashes
            path
        } else {
            ""
        }
    }

    pub(crate) fn path_is_folder(&self) -> bool {
        self.inner_path
            .as_deref()
            .map(|p| p.is_empty() || p.ends_with('/'))
            .unwrap_or(true)
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.inner_path.as_deref().and_then(|p| {
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

    /// generate the path portion of a URL for these params.
    ///
    /// Simpler version than `ParsedRustdocParams::path_for_url`, for cases where we don't
    /// have all the information yet.
    pub(crate) fn path_for_url(&self) -> String {
        // FIXME: make nicer.
        let path = if let Some(ref target) = self.doc_target {
            format!(
                "{}/{}",
                target,
                self.inner_path.as_deref().unwrap_or_default()
            )
        } else {
            // without target in the url params, we can just use the path.
            self.inner_path.clone().unwrap_or_default()
        };

        // for folders we might have `index.html` at the end.
        // We want to normalize the requests here, so a trailing `/index.html` will be cut off.
        if path.ends_with("/index.html") {
            path.trim_end_matches("index.html").to_string()
        } else {
            path
        }
    }

    pub(crate) fn rustdoc_url(&self) -> EscapedURI {
        EscapedURI::new(&format!(
            "/{}/{}/{}",
            self.name,
            self.version,
            self.path_for_url(),
        ))
    }

    pub(crate) fn crate_details_url(&self) -> EscapedURI {
        EscapedURI::new(&format!("/crate/{}/{}", self.name, self.version))
    }

    pub(crate) fn target_redirect_url(&self) -> EscapedURI {
        EscapedURI::new(&format!(
            "/crate/{}/{}/target-redirect/{}",
            self.name,
            self.version,
            self.path_for_url(),
        ))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedRustdocParams {
    inner: RustdocParams,
    doc_targets: Vec<String>,
    default_target: String,
    target_name: String,
}

impl ParsedRustdocParams {
    pub(crate) fn name(&self) -> &str {
        &self.inner.name
    }

    pub(crate) fn version(&self) -> &ReqVersion {
        &self.inner.version
    }

    pub(crate) fn with_version(mut self, version: &ReqVersion) -> Self {
        self.inner.version = version.clone();
        self
    }

    pub(crate) fn with_inner_path(mut self, inner_path: &str) -> Self {
        self.inner.inner_path = Some(inner_path.into());
        self
    }

    pub(crate) fn with_doc_target(mut self, doc_target: &str) -> Self {
        self.inner.doc_target = Some(doc_target.into());
        self
    }

    /// generate a potential storage path where to find the file that is described by these params.
    ///
    /// This is the path _inside_ the ZIP file we create in the build process.
    pub(crate) fn storage_path(&'_ self) -> String {
        // FIXME: make nicer.
        let mut storage_path = if let Some(ref target) = self.inner.doc_target {
            if target == &self.default_target {
                // when we have a target url param and it matches the default target
                // we don't include it in the storage path.
                // Files for the default target are placed at the root of the archive.
                self.inner.inner_path.clone().unwrap_or_default()
            } else {
                // all non-default targets are in subfolders named after that target.
                format!(
                    "{}/{}",
                    target,
                    self.inner.inner_path.as_deref().unwrap_or_default()
                )
            }
        } else {
            // without target in the url params, we can just use the path.
            self.inner.inner_path.clone().unwrap_or_default()
        };

        if self.path_is_folder() {
            storage_path.push_str("index.html");
        }

        storage_path
    }

    pub(crate) fn doc_target(&self) -> Option<&str> {
        self.inner.doc_target.as_deref()
    }

    pub(crate) fn doc_target_or_default(&self) -> &str {
        self.inner
            .doc_target
            .as_deref()
            .unwrap_or(&self.default_target)
    }

    pub(crate) fn path_is_folder(&self) -> bool {
        self.inner.path_is_folder()
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.inner.file_extension()
    }

    pub(crate) fn inner_path(&self) -> &str {
        // in our logic, when `parse` is done, the path is never `None`.
        // FIXME: should the inner path default to `/target_name/` when empty?
        self.inner.inner_path.as_deref().unwrap_or_default()
    }

    /// generate the path portion of a URL for these params.
    ///
    /// MOst of the time this is the storage path, but without `/index.html` if its a folder.
    pub(crate) fn path_for_url(&self) -> String {
        // FIXME: make nicer.
        let path = if let Some(ref target) = self.inner.doc_target {
            if target == &self.default_target {
                // when we have a target url param and it matches the default target
                // we don't include it in the storage path.
                // Files for the default target are placed at the root of the archive.
                self.inner.inner_path.clone().unwrap_or_default()
            } else {
                // all non-default targets are in subfolders named after that target.
                format!(
                    "{}/{}",
                    target,
                    self.inner.inner_path.as_deref().unwrap_or_default()
                )
            }
        } else {
            // without target in the url params, we can just use the path.
            self.inner.inner_path.clone().unwrap_or_default()
        };

        // for folders we might have `index.html` at the end.
        // We want to normalize the requests here, so a trailing `/index.html` will be cut off.
        if path.ends_with("/index.html") {
            path.trim_end_matches("index.html").to_string()
        } else {
            path
        }
    }

    /// check if we have a target component in the path, that matches the default
    /// target. This affects the geneated storage path, since default target docs are at the root,
    /// and the other target docs are in subfolders named after the target.
    pub(crate) fn target_is_default(&self) -> bool {
        self.doc_target() == Some(&self.default_target)
    }

    // pub(crate) fn generate_target_redirect_url(&self, other_version: ReqVersion) -> Uri {}

    pub(crate) fn update<F>(self, f: F) -> Self
    where
        F: FnOnce(&mut RustdocParams),
    {
        let mut this = self;
        f(&mut this.inner);
        this.inner.parse(
            this.default_target,
            this.target_name,
            this.doc_targets.iter().map(String::as_str),
        )
    }

    /// generate rustdoc URL to show the rustdoc page for the given params
    pub(crate) fn rustdoc_url(&self) -> EscapedURI {
        EscapedURI::new(&format!(
            "/{}/{}/{}",
            self.name(),
            self.version(),
            self.path_for_url()
        ))
    }

    pub(crate) fn target_redirect_url(&self) -> EscapedURI {
        EscapedURI::new(&format!(
            "/crate/{}/{}/target-redirect/{}",
            self.name(),
            self.version(),
            self.path_for_url(),
        ))
    }

    pub(crate) fn crate_details_url(&self) -> EscapedURI {
        self.inner.crate_details_url()
    }

    /// Generate a possible target path to redirect to, with the information we have.
    ///
    /// Built for the target-redirect view, when we don't find the
    /// target in our storage.
    ///
    /// Input is our set or parameters, plus some details from the metadata.
    ///
    /// This method is typically only used when we already know the target file doesn't exist,
    /// and we just need to redirect to a search or something similar.
    ///
    /// FIXME: add tests! :)
    pub(crate) fn generate_fallback_path(&self) -> (String, Option<String>) {
        // we already split out the potentially leading target information in `Self::parse`.
        // So we have an optional target, and then the path.
        // FIXME: perhaps move this somewhere else? Taking `ParsedRustdocParams` as parameter?
        let components: Vec<_> = self
            .inner_path()
            .trim_start_matches('/')
            .split('/')
            .collect();

        let is_source_view = components.first() == Some(&"src");

        let search_item: Option<String> = if let Some(last_component) = components.last() {
            if *last_component == "index.html" {
                // this is a module, we extract the module name
                if components.len() >= 2 {
                    // path might look like:
                    // `/[krate]/[version]/{target_name}/{module}/index.html`
                    // for the search we want to use the module name.
                    components
                        .get(components.len() - 2)
                        .map(ToString::to_string)
                } else {
                    None
                }
            } else if !is_source_view {
                // this is an item, typically the filename (last component) is something
                // `trait.SomeAwesomeStruct.html`, where we want `SomeAwesomeStruct` for
                // the search
                last_component.split('.').nth(1).map(ToString::to_string)
            } else {
                // this is from the rustdoc source view.
                // Example last component:
                // `tuple_impl.rs.html` where we want just `tuple_impl` for the search.
                last_component
                    .strip_suffix(".rs.html")
                    .map(ToString::to_string)
            }
        } else {
            None
        };

        let path = if let Some(doc_target) = self.doc_target()
            && doc_target != self.default_target
        {
            format!("{doc_target}/{}/", &self.target_name)
        } else {
            format!("{}/", &self.target_name)
        };

        (path, search_item)
    }

    pub(crate) fn generate_fallback_url(&self) -> EscapedURI {
        let (path, search_item) = self.generate_fallback_path();

        if let Some(search_item) = search_item {
            EscapedURI::new_with_query(
                &format!("/{}/{}/{}", self.name(), self.version(), path),
                &[("search", &search_item)],
            )
        } else {
            EscapedURI::new(&format!("/{}/{}/{}", self.name(), self.version(), path))
        }
    }
}

fn url_decode<'a>(input: &'a str) -> Result<Cow<'a, str>> {
    Ok(percent_encoding::percent_decode(input.as_bytes()).decode_utf8()?)
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
            doc_target: None,
            inner_path: None
        };
        "just name and version"
    )]
    #[test_case(
        "/{name}/{version}/{*path}",
        "/krate/latest/static.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("static.html".into())
        };
        "name, version, path extract"
    )]
    #[test_case(
        "/{name}/{version}/{path}/static.html",
        "/krate/latest/path_add/static.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("path_add/static.html".into())
        };
        "name, version, path extract, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/clapproc%20%60macro.html",
        "/clap/latest/clapproc%20%60macro.html",
        RustdocParams {
            name: "clap".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("clapproc `macro.html".into()),
        };
        "name, version, static suffix with some urlencoding"
    )]
    #[test_case(
        "/{name}/{version}/static.html",
        "/krate/latest/static.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("static.html".into())
        };
        "name, version, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}",
        "/krate/1.2.3/some-target",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: None
        };
        "name, version, target"
    )]
    #[test_case(
        "/{name}/{version}/{target}/folder/something.html",
        "/krate/1.2.3/some-target/folder/something.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: Some("folder/something.html".into())
        };
        "name, version, target, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}/",
        "/krate/1.2.3/some-target/",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: None
        };
        "name, version, target trailing slash"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{*path}",
        "/krate/1.2.3/some-target/some/path/to/a/file.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: Some("some/path/to/a/file.html".into())
        };
        "name, version, target, path"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{path}/path/to/a/file.html",
        "/krate/1.2.3/some-target/path_add/path/to/a/file.html",
        RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: Some("path_add/path/to/a/file.html".into())
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

    #[test_case(
        None, None,
        None, "", "index.html";
        "super empty 1"
    )]
    #[test_case(
        Some(""), Some(""),
        None, "", "index.html";
        "super empty 2"
    )]
    // test cases when no separate "target" component was present in the params
    #[test_case(
        None, Some("/"),
        None, "", "index.html";
        "just slash"
    )]
    #[test_case(
        None, Some("something"),
        None, "something", "something";
        "without trailing slash"
    )]
    #[test_case(
        None, Some("/something"),
        None, "something", "something";
        "leading slash is cut"
    )]
    #[test_case(
        None, Some("something/"),
        None, "something/", "something/index.html";
        "with trailing slash"
    )]
    // a target is given, but as first component of the path, for routes without separate
    // "target" component
    #[test_case(
        None, Some("some-target-name"),
        Some("some-target-name"), "", "index.html";
        "just target without trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/"),
        Some("some-target-name"), "", "index.html";
        "just default target with trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/one"),
        Some("some-target-name"), "one", "one";
        "target + one without trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/one/"),
        Some("some-target-name"), "one/", "one/index.html";
        "target + one target with trailing slash"
    )]
    #[test_case(
        None, Some("unknown-target-name/one/"),
        None, "unknown-target-name/one/", "unknown-target-name/one/index.html";
        "unknown target stays in path"
    )]
    #[test_case(
        None, Some("some-target-name/some/inner/path"),
        Some("some-target-name"), "some/inner/path", "some/inner/path";
        "all without trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/some/inner/path/"),
        Some("some-target-name"), "some/inner/path/", "some/inner/path/index.html";
        "all with trailing slash"
    )]
    // here we have a separate target path parameter, we check it and use it accordingly
    #[test_case(
        Some("some-target-name"), None,
        Some("some-target-name"), "", "index.html";
        "actual target, that is default"
    )]
    #[test_case(
        Some("some-target-name"), Some("inner/path.html"),
        Some("some-target-name"), "inner/path.html", "inner/path.html";
        "actual target with path"
    )]
    #[test_case(
        Some("some-target-name"), Some("inner/path/"),
        Some("some-target-name"), "inner/path/", "inner/path/index.html";
        "actual target with path slash"
    )]
    #[test_case(
        Some("unknown-target"), None,
        None, "unknown-target/", "unknown-target/index.html";
        "unknown target"
    )]
    #[test_case(
        Some("unknown-target"), Some("inner/path.html"),
        None, "unknown-target/inner/path.html", "unknown-target/inner/path.html";
        "unknown target with path"
    )]
    #[test_case(
        Some("other-target"), Some("inner/path.html"),
        Some("other-target"), "inner/path.html", "other-target/inner/path.html";
        "other target with path"
    )]
    #[test_case(
        Some("unknown-target"), Some("inner/path/"),
        None, "unknown-target/inner/path/", "unknown-target/inner/path/index.html";
        "unknown target with path slash"
    )]
    #[test_case(
        Some("other-target"), Some("inner/path/"),
        Some("other-target"), "inner/path/", "other-target/inner/path/index.html";
        "other target with path slash"
    )]
    #[test_case(
        Some("some-target-name"), None,
        Some("some-target-name"), "", "index.html";
        "pure default target, without trailing slash"
    )]
    fn test_parse(
        target: Option<&str>,
        path: Option<&str>,
        expected_target: Option<&str>,
        expected_path: &str,
        expected_storage_path: &str,
    ) {
        static TARGETS: &[&str] = &["some-target-name", "other-target"];
        static DEFAULT_TARGET: &str = "some-target-name";

        let parsed = RustdocParams {
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: target.map(|s| s.into()),
            inner_path: path.map(|s| s.into()),
        }
        .parse(DEFAULT_TARGET, "krate", TARGETS.iter());

        assert_eq!(parsed.name(), "krate");
        assert_eq!(parsed.version(), &ReqVersion::Latest);
        assert_eq!(parsed.doc_target(), expected_target);
        assert_eq!(parsed.inner_path(), expected_path);
        assert_eq!(parsed.storage_path(), expected_storage_path);
    }

    // TODO: tests for fallback search
}
