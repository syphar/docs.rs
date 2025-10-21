//! special rustdoc extractors

use std::{borrow::Cow, iter};

use anyhow::{Result, anyhow};
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, MatchedPath},
    http::{Uri, request::Parts},
};
use docsrs_metadata::HOST_TARGET;
use itertools::Itertools as _;
use serde::Deserialize;

use crate::{
    db::ReleaseId,
    web::{
        MatchedRelease, MetaData, ReqVersion, error::AxumNope, escaped_uri::EscapedURI,
        extractors::Path,
    },
};

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum PageKind {
    #[default]
    Rustdoc,
    Source,
}

/// can extract rustdoc parameters from path and uri.
///
/// includes parsing / interpretation logic using the crate metadata.
///
/// TODO: features to add?
/// * generate standard URLs for these params? Same for the parsed version?
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RustdocParams {
    original_uri: Option<Uri>,
    pub(crate) name: String,
    pub(crate) version: ReqVersion,
    pub(crate) doc_target: Option<String>,
    pub(crate) inner_path: Option<String>,
    page_kind: PageKind,
}

/// the parameters that might come as url parameters via route.
#[derive(Deserialize, Debug)]
struct UrlParams {
    pub(crate) name: String,
    #[serde(default)]
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

        let original_uri = parts.extract::<Uri>().await.expect("infallible extractor");

        // we need to be able to extract static suffixes that are not in the route `{*path}`.
        //
        // TODO: there is an edge case where for `/crate/{krate}/{version}/source/ we would
        // previously treat the folder as suffix.
        // How to solve? Either only for the suffix logic only for non-folders? Or have adapted
        // parameter logic for the source views?
        if get_file_extension(original_uri.path()).is_some() {
            let uri_path = url_decode(original_uri.path()).map_err(AxumNope::BadRequest)?;

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
        }

        Ok(RustdocParams {
            name: params.name.trim().to_owned(),
            version: params.version,
            doc_target: params.target.map(|t| t.trim().to_owned()),
            inner_path: params.path.map(|p| p.trim().to_owned()),
            original_uri: Some(original_uri),
            page_kind: PageKind::default(),
        })
    }
}

impl RustdocParams {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: ReqVersion::default(),
            original_uri: None,
            doc_target: None,
            inner_path: None,
            page_kind: PageKind::default(),
        }
    }

    pub(crate) fn with_name(self, name: &str) -> Self {
        RustdocParams {
            name: name.to_owned(),
            ..self
        }
    }

    pub(crate) fn with_version(self, version: impl Into<ReqVersion>) -> Self {
        RustdocParams {
            version: version.into(),
            ..self
        }
    }

    pub(crate) fn with_maybe_doc_target(self, doc_target: Option<impl Into<String>>) -> Self {
        RustdocParams {
            doc_target: doc_target.map(Into::into),
            ..self
        }
    }

    pub(crate) fn with_doc_target(self, doc_target: impl Into<String>) -> Self {
        self.with_maybe_doc_target(Some(doc_target))
    }

    pub(crate) fn with_inner_path(self, inner_path: impl Into<String>) -> Self {
        self.with_maybe_inner_path(Some(inner_path))
    }

    pub(crate) fn with_maybe_inner_path(self, inner_path: Option<impl Into<String>>) -> Self {
        RustdocParams {
            inner_path: inner_path.map(Into::into),
            ..self
        }
    }

    pub(crate) fn with_original_uri(self, original_uri: impl Into<Uri>) -> Self {
        RustdocParams {
            original_uri: Some(original_uri.into()),
            ..self
        }
    }

    pub(crate) fn with_page_kind(self, page_kind: impl Into<PageKind>) -> Self {
        RustdocParams {
            page_kind: page_kind.into(),
            ..self
        }
    }

    pub(crate) fn parse_with_metadata(self, metadata: &MetaData) -> Result<ParsedRustdocParams> {
        Ok(self.parse(
            metadata.default_target.as_deref(),
            metadata.target_name.as_deref(),
            metadata.doc_targets.iter().flatten(),
        ))
    }

    pub(crate) async fn load_and_parse(
        self,
        conn: &mut sqlx::PgConnection,
        release_id: ReleaseId,
    ) -> Result<ParsedRustdocParams> {
        let krate = sqlx::query!(
            "SELECT
                releases.default_target,
                releases.target_name,
                releases.doc_targets
            FROM releases
            WHERE releases.id = $1;",
            release_id.0,
        )
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(AxumNope::CrateNotFound)?;

        let doc_targets: Vec<String> = krate
            .doc_targets
            .map(MetaData::parse_doc_targets)
            .into_iter()
            .flatten()
            .collect();

        Ok(self.parse(
            krate.default_target.as_deref(),
            krate.target_name.as_deref(),
            doc_targets.iter(),
        ))
    }

    /// TODO: nice docstring
    pub(crate) fn parse<D, T, I, V>(
        mut self,
        default_target: Option<D>,
        target_name: Option<T>,
        doc_targets: I,
    ) -> ParsedRustdocParams
    where
        D: Into<String>,
        T: Into<String>,
        I: IntoIterator<Item = V>,
        V: Into<String>,
    {
        // TODO: optimization: less owned variables, more references
        // TODO: nicer target logic
        let default_target = default_target.map(Into::into);
        debug_assert!(
            default_target
                .as_ref()
                .map(|s| !s.is_empty())
                .unwrap_or(true)
        );

        let doc_targets = doc_targets
            .into_iter()
            .map(|s| s.into())
            .collect::<Vec<_>>();

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
                } else if self.has_trailing_slash() {
                    new_path = format!("{}/", given_target);
                } else {
                    new_path = given_target.into();
                }
            }
        } else {
            // there is no separate target component given in the route parameters.
            // We look at the first component of the path and see if it matches a target.

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

        if let Some(ref new_target) = new_target {
            debug_assert!(!new_target.contains('/'));
            debug_assert!(new_target.contains('-'));
        }
        self.doc_target = new_target;

        debug_assert!(!new_path.starts_with('/')); // we should trim leading slashes
        self.inner_path = Some(new_path);

        let target_name = target_name.map(Into::into);
        debug_assert!(target_name.as_ref().map(|s| !s.is_empty()).unwrap_or(true));

        ParsedRustdocParams {
            doc_targets,
            default_target,
            target_name,
            inner: self,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn inner_path(&self) -> &str {
        self.inner_path.as_deref().unwrap_or_default()
    }

    pub(crate) fn doc_target(&self) -> Option<&str> {
        self.doc_target.as_deref()
    }

    pub(crate) fn path_is_folder(&self) -> bool {
        self.inner_path
            .as_deref()
            .map(|p| p.is_empty() || p.ends_with('/'))
            .unwrap_or(true)
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.original_uri
            .as_ref()
            .and_then(|uri| get_file_extension(uri.path()))
    }

    pub(crate) fn page_kind(&self) -> &PageKind {
        &self.page_kind
    }

    fn path_for_rustdoc_url(&self) -> String {
        if matches!(self.page_kind, PageKind::Rustdoc) {
            generate_path_for_url(
                None,
                None,
                self.doc_target.as_deref(),
                self.inner_path.as_deref(),
            )
        } else {
            generate_path_for_url(None, None, self.doc_target.as_deref(), None)
        }
    }

    pub(crate) fn rustdoc_url(&self) -> EscapedURI {
        generate_rustdoc_url(&self.name, &self.version, &self.path_for_rustdoc_url())
    }

    pub(crate) fn crate_details_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!("/crate/{}/{}", self.name, self.version))
    }

    pub(crate) fn builds_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!("/crate/{}/{}/builds", self.name, self.version))
    }

    pub(crate) fn features_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!("/crate/{}/{}/features", self.name, self.version))
    }

    pub(crate) fn source_url(&self) -> EscapedURI {
        // if the params were created for a rustdoc page,
        // the inner path is a source file path, so is not usable for
        // source urls.
        let inner_path = if matches!(self.page_kind, PageKind::Source) {
            self.inner_path()
        } else {
            ""
        };
        EscapedURI::from_path(format!(
            "/crate/{}/{}/source/{}",
            &self.name, &self.version, &inner_path
        ))
    }

    fn has_trailing_slash(&self) -> bool {
        self.original_path().ends_with('/')
    }

    pub(crate) fn target_redirect_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!(
            "/crate/{}/{}/target-redirect/{}",
            self.name,
            self.version,
            &self.path_for_rustdoc_url(),
        ))
    }

    pub(crate) fn original_path(&self) -> &str {
        self.original_uri
            .as_ref()
            .map(|p| p.path())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParsedRustdocParams {
    inner: RustdocParams,
    doc_targets: Vec<String>,
    default_target: Option<String>,
    target_name: Option<String>,
}

impl ParsedRustdocParams {
    pub(crate) fn name(&self) -> &str {
        &self.inner.name
    }

    pub(crate) fn version(&self) -> &ReqVersion {
        &self.inner.version
    }

    pub(crate) fn with_version(mut self, version: impl Into<ReqVersion>) -> Self {
        self.inner.version = version.into();
        self
    }

    pub(crate) fn with_doc_target(self, doc_target: impl Into<String>) -> Self {
        self.update(|inner| {
            inner.doc_target = Some(doc_target.into());
        })
    }

    /// generate a potential storage path where to find the file that is described by these params.
    ///
    /// This is the path _inside_ the ZIP file we create in the build process.
    pub(crate) fn storage_path(&'_ self) -> String {
        let mut storage_path = if let Some(ref target) = self.inner.doc_target {
            if let Some(ref default_target) = self.default_target
                && target == default_target
            {
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

    pub(crate) fn doc_target_or_default(&self) -> Option<&str> {
        self.inner
            .doc_target
            .as_deref()
            .or(self.default_target.as_deref())
    }

    pub(crate) fn path_is_folder(&self) -> bool {
        self.inner.path_is_folder()
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.inner.file_extension()
    }

    pub(crate) fn inner_path(&self) -> &str {
        self.inner.inner_path.as_deref().unwrap_or_default()
    }

    /// check if we have a target component in the path, that matches the default
    /// target. This affects the geneated storage path, since default target docs are at the root,
    /// and the other target docs are in subfolders named after the target.
    pub(crate) fn target_is_default(&self) -> bool {
        self.default_target
            .as_deref()
            .map(|t| self.doc_target() == Some(t))
            .unwrap_or(false)
    }

    pub(crate) fn update<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut RustdocParams),
    {
        f(&mut self.inner);
        self.inner
            .parse(self.default_target, self.target_name, self.doc_targets)
    }

    pub(crate) fn page_kind(&self) -> &PageKind {
        self.inner.page_kind()
    }

    /// generate rustdoc URL to show the rustdoc page for the given params
    pub(crate) fn rustdoc_url(&self) -> EscapedURI {
        generate_rustdoc_url(self.name(), self.version(), &self.path_for_rustdoc_url())
    }

    pub(crate) fn source_url(&self) -> EscapedURI {
        self.inner.source_url()
    }

    pub(crate) fn builds_url(&self) -> EscapedURI {
        self.inner.builds_url()
    }

    pub(crate) fn features_url(&self) -> EscapedURI {
        self.inner.features_url()
    }

    fn path_for_rustdoc_url(&self) -> String {
        if matches!(self.page_kind(), PageKind::Rustdoc) {
            generate_path_for_url(
                self.target_name.as_deref(),
                self.default_target.as_deref(),
                self.inner.doc_target.as_deref(),
                self.inner.inner_path.as_deref(),
            )
        } else {
            generate_path_for_url(
                self.target_name.as_deref(),
                self.default_target.as_deref(),
                self.inner.doc_target.as_deref(),
                None,
            )
        }
    }

    pub(crate) fn target_redirect_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!(
            "/crate/{}/{}/target-redirect/{}",
            self.name(),
            self.version(),
            &self.path_for_rustdoc_url(),
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
    fn generate_fallback_path(&self) -> (String, Option<String>) {
        // we already split out the potentially leading target information in `Self::parse`.
        // So we have an optional target, and then the path.
        let components: Vec<_> = self
            .inner_path()
            .trim_start_matches('/')
            .split('/')
            .collect();

        let is_source_view = components.first() == Some(&"src");

        let search_item: Option<&str> = components.last().and_then(|&last_component| {
            if last_component.is_empty() || last_component == "index.html" {
                // this is a module, we extract the module name
                //
                // path might look like:
                // `/[krate]/[version]/{target_name}/{module}/index.html` (last_component is index)
                // or
                // `/[krate]/[version]/{target_name}/{module}/` (last_component is empty)
                //
                // for the search we want to use the module name.
                components.iter().rev().nth(1).cloned()
            } else if !is_source_view {
                // this is an item, typically the filename (last component) is something
                // `trait.SomeAwesomeStruct.html`, where we want `SomeAwesomeStruct` for
                // the search
                last_component.split('.').nth(1)
            } else {
                // this is from the rustdoc source view.
                // Example last component:
                // `tuple_impl.rs.html` where we want just `tuple_impl` for the search.
                last_component.strip_suffix(".rs.html")
            }
        });

        let mut path = String::new();

        if let Some(doc_target) = self.doc_target() {
            let is_default_target = self
                .default_target
                .as_ref()
                .map_or(false, |t| t == doc_target);

            if !is_default_target {
                path = format!("{}/", doc_target);
            }
        };

        if let Some(ref target_name) = self.target_name {
            path.push_str(&format!("{target_name}/"));
        }

        (path, search_item.map(ToString::to_string))
    }

    pub(crate) fn generate_fallback_url(&self) -> EscapedURI {
        let (path, search_item) = self.generate_fallback_path();

        if let Some(search_item) = search_item {
            EscapedURI::from_path_and_query(
                &format!("/{}/{}/{}", self.name(), self.version(), path),
                &[("search", &search_item)],
            )
        } else {
            EscapedURI::from_path(format!("/{}/{}/{}", self.name(), self.version(), path))
        }
    }

    pub(crate) fn doc_targets(&self) -> &[String] {
        &self.doc_targets
    }
}

fn get_file_extension(path: &str) -> Option<&str> {
    path.rsplit_once('.').and_then(|(_, ext)| {
        if ext.contains('/') {
            // to handle cases like `foo.html/bar` where I want `None`
            None
        } else {
            Some(ext)
        }
    })
}

impl TryFrom<MatchedRelease> for ParsedRustdocParams {
    type Error = anyhow::Error;

    fn try_from(release: MatchedRelease) -> Result<Self> {
        let target_name = release
            .target_name()
            .ok_or_else(|| anyhow!("default target missing in release"))?
            .to_owned();

        Ok(RustdocParams::new(&release.name)
            .with_version(release.req_version)
            .parse(
                HOST_TARGET.into(),
                target_name.into(),
                iter::once(HOST_TARGET),
            ))
    }
}

fn url_decode<'a>(input: &'a str) -> Result<Cow<'a, str>> {
    Ok(percent_encoding::percent_decode(input.as_bytes()).decode_utf8()?)
}

fn generate_rustdoc_url(name: &str, version: &ReqVersion, path: &str) -> EscapedURI {
    EscapedURI::from_path(format!("/{}/{}/{}", name, version, path))
}

fn generate_path_for_url(
    target_name: Option<&str>,
    default_target: Option<&str>,
    doc_target: Option<&str>,
    inner_path: Option<&str>,
) -> String {
    let inner_path = if let Some(target_name) = target_name {
        if let Some(inner_path) = inner_path
                && !inner_path.is_empty()
                // special case: if the inner path is just "index.html", we assume we have to attach
                // the `target_name`
                && inner_path != "index.html"
        {
            inner_path.to_owned()
        } else {
            format!("{}/", target_name)
        }
    } else {
        inner_path.unwrap_or("").to_owned()
    };

    let path = if let Some(target) = doc_target {
        if let Some(default_target) = default_target
            && target == default_target
        {
            // when we have a target url param and it matches the default target
            // we don't include it in the storage path.
            // Files for the default target are placed at the root of the archive.
            inner_path
        } else {
            // all non-default targets are in subfolders named after that target.
            format!("{}/{}", target, inner_path)
        }
    } else {
        // without target in the url params, we can just use the path.
        inner_path
    };

    // for folders we might have `index.html` at the end.
    // We want to normalize the requests here, so a trailing `/index.html` will be cut off.
    if path.ends_with("/index.html") {
        path.trim_end_matches("index.html").to_string()
    } else {
        path
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
    use semver::Version;
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
        "/{name}",
        RustdocParams {
            original_uri: Some("/krate".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: None,
            page_kind: PageKind::Rustdoc,
        };
        "just name"
    )]
    #[test_case(
        "/{name}/",
        RustdocParams {
            original_uri: Some("/krate/".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: None,
            page_kind: PageKind::Rustdoc,
        };
        "just name with trailing slash"
    )]
    #[test_case(
        "/{name}/{version}",
        RustdocParams {
            original_uri: Some("/krate/latest".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: None,
            page_kind: PageKind::Rustdoc,
        };
        "just name and version"
    )]
    #[test_case(
        "/{name}/{version}/{*path}",
        RustdocParams {
            original_uri: Some("/krate/latest/static.html".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("static.html".into()),
            page_kind: PageKind::Rustdoc,
        };
        "name, version, path extract"
    )]
    #[test_case(
        "/{name}/{version}/{path}/static.html",
        RustdocParams {
            original_uri: Some("/krate/latest/path_add/static.html".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("path_add/static.html".into()),
            page_kind: PageKind::Rustdoc,
        };
        "name, version, path extract, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/clapproc%20%60macro.html",
        RustdocParams {
            original_uri: Some("/clap/latest/clapproc%20%60macro.html".parse().unwrap()),
            name: "clap".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("clapproc `macro.html".into()),
            page_kind: PageKind::Rustdoc,
        };
        "name, version, static suffix with some urlencoding"
    )]
    #[test_case(
        "/{name}/{version}/static.html",
        RustdocParams {
            original_uri: Some("/krate/latest/static.html".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Latest,
            doc_target: None,
            inner_path: Some("static.html".into()),
            page_kind: PageKind::Rustdoc,
        };
        "name, version, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}",
        RustdocParams {
            original_uri: Some("/krate/1.2.3/some-target".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: None,
            page_kind: PageKind::Rustdoc,
        };
        "name, version, target"
    )]
    #[test_case(
        "/{name}/{version}/{target}/folder/something.html",
        RustdocParams {
            original_uri: Some("/krate/1.2.3/some-target/folder/something.html".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: Some("folder/something.html".into()),
            page_kind: PageKind::Rustdoc,
        };
        "name, version, target, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}/",
        RustdocParams {
            original_uri: Some("/krate/1.2.3/some-target/".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: None,
            page_kind: PageKind::Rustdoc,
        };
        "name, version, target trailing slash"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{*path}",
        RustdocParams {
            original_uri: Some("/krate/1.2.3/some-target/some/path/to/a/file.html".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: Some("some/path/to/a/file.html".into()),
            page_kind: PageKind::Rustdoc,
        };
        "name, version, target, path"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{path}/path/to/a/file.html",
        RustdocParams {
            original_uri: Some("/krate/1.2.3/some-target/path_add/path/to/a/file.html".parse().unwrap()),
            name: "krate".into(),
            version: ReqVersion::Exact("1.2.3".parse().unwrap()),
            doc_target: Some("some-target".into()),
            inner_path: Some("path_add/path/to/a/file.html".into()),
            page_kind: PageKind::Rustdoc,
        };
        "name, version, target, path, static suffix"
    )]
    #[tokio::test]
    async fn test_extract_rustdoc_params_from_request(
        route: &str,
        expected: RustdocParams,
    ) -> anyhow::Result<()> {
        let app = Router::new().route(
            route,
            get(|params: RustdocParams| async move { format!("{:?}", params) }),
        );

        let path = expected.original_uri.as_ref().unwrap().path().to_owned();

        let res = app.get(&path).await?;
        assert!(res.status().is_success());
        assert_eq!(res.text().await?, format!("{:?}", expected));

        Ok(())
    }

    #[test_case(
        None, None, false,
        None, "", "index.html";
        "super empty 1"
    )]
    #[test_case(
        Some(""), Some(""), false,
        None, "", "index.html";
        "super empty 2"
    )]
    // test cases when no separate "target" component was present in the params
    #[test_case(
        None, Some("/"), true,
        None, "", "index.html";
        "just slash"
    )]
    #[test_case(
        None, Some("something"), false,
        None, "something", "something";
        "without trailing slash"
    )]
    #[test_case(
        None, Some("/something"), false,
        None, "something", "something";
        "leading slash is cut"
    )]
    #[test_case(
        None, Some("something/"), true,
        None, "something/", "something/index.html";
        "with trailing slash"
    )]
    // a target is given, but as first component of the path, for routes without separate
    // "target" component
    #[test_case(
        None, Some("some-target-name"), false,
        Some("some-target-name"), "", "index.html";
        "just target without trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/"), true,
        Some("some-target-name"), "", "index.html";
        "just default target with trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/one"), false,
        Some("some-target-name"), "one", "one";
        "target + one without trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/one/"), true,
        Some("some-target-name"), "one/", "one/index.html";
        "target + one target with trailing slash"
    )]
    #[test_case(
        None, Some("unknown-target-name/one/"), true,
        None, "unknown-target-name/one/", "unknown-target-name/one/index.html";
        "unknown target stays in path"
    )]
    #[test_case(
        None, Some("some-target-name/some/inner/path"), false,
        Some("some-target-name"), "some/inner/path", "some/inner/path";
        "all without trailing slash"
    )]
    #[test_case(
        None, Some("some-target-name/some/inner/path/"), true,
        Some("some-target-name"), "some/inner/path/", "some/inner/path/index.html";
        "all with trailing slash"
    )]
    // here we have a separate target path parameter, we check it and use it accordingly
    #[test_case(
        Some("some-target-name"), None, false,
        Some("some-target-name"), "", "index.html";
        "actual target, that is default"
    )]
    #[test_case(
        Some("some-target-name"), Some("inner/path.html"), false,
        Some("some-target-name"), "inner/path.html", "inner/path.html";
        "actual target with path"
    )]
    #[test_case(
        Some("some-target-name"), Some("inner/path/"), true,
        Some("some-target-name"), "inner/path/", "inner/path/index.html";
        "actual target with path slash"
    )]
    #[test_case(
        Some("unknown-target"), None, true,
        None, "unknown-target/", "unknown-target/index.html";
        "unknown target"
    )]
    #[test_case(
        Some("unknown-target"), None, false,
        None, "unknown-target", "unknown-target";
        "unknown target without trailing slash"
    )]
    #[test_case(
        Some("unknown-target"), Some("inner/path.html"), false,
        None, "unknown-target/inner/path.html", "unknown-target/inner/path.html";
        "unknown target with path"
    )]
    #[test_case(
        Some("other-target"), Some("inner/path.html"), false,
        Some("other-target"), "inner/path.html", "other-target/inner/path.html";
        "other target with path"
    )]
    #[test_case(
        Some("unknown-target"), Some("inner/path/"), true,
        None, "unknown-target/inner/path/", "unknown-target/inner/path/index.html";
        "unknown target with path slash"
    )]
    #[test_case(
        Some("other-target"), Some("inner/path/"), true,
        Some("other-target"), "inner/path/", "other-target/inner/path/index.html";
        "other target with path slash"
    )]
    #[test_case(
        Some("some-target-name"), None, false,
        Some("some-target-name"), "", "index.html";
        "pure default target, without trailing slash"
    )]
    fn test_parse(
        target: Option<&str>,
        path: Option<&str>,
        had_trailing_slash: bool,
        expected_target: Option<&str>,
        expected_path: &str,
        expected_storage_path: &str,
    ) {
        static TARGETS: &[&str] = &["some-target-name", "other-target"];
        static DEFAULT_TARGET: &str = "some-target-name";

        let mut dummy_path = match (target, path) {
            (Some(target), Some(path)) => format!("{}/{}", target, path),
            (Some(target), None) => target.to_string(),
            (None, Some(path)) => path.to_string(),
            (None, None) => String::new(),
        };
        dummy_path.insert(0, '/');
        if had_trailing_slash && !dummy_path.is_empty() {
            dummy_path.push('/');
        }

        let parsed = RustdocParams::new("krate")
            .with_version(ReqVersion::Latest)
            .with_maybe_doc_target(target)
            .with_maybe_inner_path(path)
            .with_original_uri(dummy_path.parse::<Uri>().unwrap())
            .parse(
                DEFAULT_TARGET.into(),
                "krate".into(),
                TARGETS.iter().cloned(),
            );

        assert_eq!(parsed.name(), "krate");
        assert_eq!(parsed.version(), &ReqVersion::Latest);
        assert_eq!(parsed.doc_target(), expected_target);
        assert_eq!(parsed.inner_path(), expected_path);
        assert_eq!(parsed.storage_path(), expected_storage_path);
    }

    #[test_case("dummy/struct.WindowsOnly.html", Some("WindowsOnly"))]
    #[test_case("dummy/struct.DefaultOnly.html", Some("DefaultOnly"))]
    #[test_case("dummy/some_module/struct.SomeItem.html", Some("SomeItem"))]
    #[test_case("dummy/some_module/index.html", Some("some_module"))]
    #[test_case("dummy/some_module/", Some("some_module"))]
    #[test_case("src/folder1/folder2/logic.rs.html", Some("logic"))]
    fn test_generate_fallback_url(path: &str, search: Option<&str>) {
        static TARGETS: &[&str] = &["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"];
        static DEFAULT_TARGET: &str = "x86_64-unknown-linux-gnu";

        // non-default target, target stays in the url
        let mut params = RustdocParams::new("dummy")
            .with_version(ReqVersion::Exact(Version::new(0, 4, 0)))
            .with_doc_target(TARGETS[1])
            .with_inner_path(path)
            .parse(
                DEFAULT_TARGET.into(),
                "dummy".into(),
                TARGETS.iter().cloned(),
            );

        assert_eq!(
            params.generate_fallback_path(),
            (
                "x86_64-pc-windows-msvc/dummy/".into(),
                search.map(ToOwned::to_owned)
            )
        );
        assert_eq!(
            params.generate_fallback_url().to_string(),
            format!(
                "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/{}",
                search.map(|s| format!("?search={}", s)).unwrap_or_default()
            )
        );

        // change to default target, check url again
        params = params.with_doc_target(DEFAULT_TARGET);

        assert_eq!(
            params.generate_fallback_path(),
            ("dummy/".into(), search.map(ToOwned::to_owned))
        );
        assert_eq!(
            params.generate_fallback_url().to_string(),
            format!(
                "/dummy/0.4.0/dummy/{}",
                search.map(|s| format!("?search={}", s)).unwrap_or_default()
            )
        );
    }

    #[test]
    fn test_parse_source() {
        let params = ParsedRustdocParams {
            inner: RustdocParams {
                original_uri: Some("/crate/dummy/0.4.0/source/README.md".parse().unwrap()),
                name: "dummy".into(),
                version: ReqVersion::Exact(Version::new(0, 4, 0)),
                doc_target: None,
                inner_path: Some("README.md".into()),
                page_kind: PageKind::Source,
            },
            doc_targets: vec![
                "x86_64-pc-windows-msvc".into(),
                "x86_64-unknown-linux-gnu".into(),
            ],
            default_target: Some("x86_64-unknown-linux-gnu".into()),
            target_name: Some("dummy".into()),
        };

        assert_eq!(params.rustdoc_url().to_string(), "/dummy/0.4.0/dummy/");
        assert_eq!(
            params.source_url().to_string(),
            "/crate/dummy/0.4.0/source/README.md"
        );
        assert_eq!(
            params.target_redirect_url().to_string(),
            "/crate/dummy/0.4.0/target-redirect/dummy/"
        );
    }

    #[test]
    fn test_parse_source_2() {
        let params = ParsedRustdocParams {
            inner: RustdocParams {
                original_uri: Some("/crate/dummy/0.4.0/source/README.md".parse().unwrap()),
                name: "dummy".into(),
                version: ReqVersion::Exact(Version::new(0, 4, 0)),
                doc_target: Some("x86_64-pc-windows-msvc".into()),
                inner_path: Some("README.md".into()),
                page_kind: PageKind::Source,
            },
            doc_targets: vec![
                "x86_64-pc-windows-msvc".into(),
                "x86_64-unknown-linux-gnu".into(),
            ],
            default_target: Some("x86_64-unknown-linux-gnu".into()),
            target_name: Some("dummy".into()),
        };

        assert_eq!(
            params.rustdoc_url().to_string(),
            "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/"
        );
        assert_eq!(
            params.source_url().to_string(),
            "/crate/dummy/0.4.0/source/README.md"
        );
        assert_eq!(
            params.target_redirect_url().to_string(),
            "/crate/dummy/0.4.0/target-redirect/x86_64-pc-windows-msvc/dummy/"
        );
    }
}
