//! special rustdoc extractors
//! missing things / questions
//!
//! ## better page_kind
//!
//! I have:
//! - endpoints where the inner path should be used, some endpoints not
//! - if used, then some pages need the static-suffix logic, and some do not.
//!
//! TODO:
//! * write test, initial params with unknown target, gets moved to inner_path?

use crate::{
    db::ReleaseId,
    web::{
        MatchedRelease, MetaData, ReqVersion, error::AxumNope, escaped_uri::EscapedURI,
        extractors::Path,
    },
};
use anyhow::{Context as _, Result, anyhow, bail};
use axum::{
    RequestPartsExt,
    extract::{FromRequestParts, MatchedPath},
    http::{Uri, request::Parts},
};
use docsrs_metadata::HOST_TARGET;
use itertools::Itertools as _;
use serde::Deserialize;
use std::{borrow::Cow, iter};
use tracing::trace;

static INDEX_HTML: &str = "index.html";
static FOLDER_AND_INDEX_HTML: &str = "/index.html";

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PageKind {
    Rustdoc,
    Source,
}

/// can extract rustdoc parameters from path and uri.
///
/// includes parsing / interpretation logic using the crate metadata.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RustdocParams {
    original_uri: Option<Uri>,
    name: String,
    version: ReqVersion,
    doc_target: Option<String>,
    inner_path: Option<String>,
    page_kind: Option<PageKind>,
    static_route_suffix: Option<String>,
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
        let Path(params) = parts
            .extract::<Path<UrlParams>>()
            .await
            .map_err(|err| AxumNope::BadRequest(err.into()))?;

        let original_uri = parts.extract::<Uri>().await.expect("infallible extractor");

        let static_route_suffix = {
            let uri_path = url_decode(original_uri.path()).map_err(AxumNope::BadRequest)?;

            let matched_path = parts
                .extract::<MatchedPath>()
                .await
                .map_err(|err| AxumNope::BadRequest(err.into()))?;
            let matched_route = url_decode(matched_path.as_str()).map_err(AxumNope::BadRequest)?;

            find_static_route_suffix(&matched_route, &uri_path)
        };

        Ok(RustdocParams::new(params.name)
            .with_version(params.version)
            .with_maybe_doc_target(params.target)
            .with_maybe_inner_path(params.path)
            .with_original_uri(original_uri)
            .with_page_kind(PageKind::Rustdoc)
            .with_maybe_static_route_suffix(static_route_suffix))
    }
}

impl RustdocParams {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into().trim().into(),
            version: ReqVersion::default(),
            original_uri: None,
            doc_target: None,
            inner_path: None,
            page_kind: Some(PageKind::Rustdoc),
            static_route_suffix: None,
        }
    }

    pub(crate) fn with_name(self, name: impl Into<String>) -> Self {
        RustdocParams {
            name: name.into().trim().into(),
            ..self
        }
    }

    pub(crate) fn with_version(self, version: impl Into<ReqVersion>) -> Self {
        RustdocParams {
            version: version.into(),
            ..self
        }
    }

    pub(crate) fn with_static_route_suffix(self, static_route_suffix: impl Into<String>) -> Self {
        self.with_maybe_static_route_suffix(Some(static_route_suffix))
    }

    pub(crate) fn with_maybe_static_route_suffix(
        self,
        static_route_suffix: Option<impl Into<String>>,
    ) -> Self {
        RustdocParams {
            static_route_suffix: static_route_suffix.map(Into::into),
            ..self
        }
    }

    pub(crate) fn try_with_version<V>(self, version: V) -> Result<Self>
    where
        V: TryInto<ReqVersion>,
        V::Error: std::error::Error + Send + Sync + 'static,
    {
        Ok(RustdocParams {
            version: version.try_into().context("couldn't parse version")?,
            ..self
        })
    }

    pub(crate) fn with_doc_target(self, doc_target: impl Into<String>) -> Self {
        self.with_maybe_doc_target(Some(doc_target))
    }

    pub(crate) fn with_maybe_doc_target(self, doc_target: Option<impl Into<String>>) -> Self {
        RustdocParams {
            doc_target: doc_target.map(|t| t.into().trim().to_owned()),
            ..self
        }
    }

    pub(crate) fn with_inner_path(self, inner_path: impl Into<String>) -> Self {
        self.with_maybe_inner_path(Some(inner_path))
    }

    pub(crate) fn with_maybe_inner_path(self, inner_path: Option<impl Into<String>>) -> Self {
        RustdocParams {
            inner_path: inner_path.map(|t| t.into().trim().to_owned()),
            ..self
        }
    }

    pub(crate) fn with_original_uri(self, original_uri: impl Into<Uri>) -> Self {
        RustdocParams {
            original_uri: Some(original_uri.into()),
            ..self
        }
    }

    pub(crate) fn try_with_original_uri<V>(self, original_uri: V) -> Result<Self>
    where
        V: TryInto<Uri>,
        V::Error: std::error::Error + Send + Sync + 'static,
    {
        Ok(RustdocParams {
            original_uri: Some(original_uri.try_into().context("couldn't parse uri")?),
            ..self
        })
    }

    pub(crate) fn with_page_kind(self, page_kind: impl Into<PageKind>) -> Self {
        RustdocParams {
            page_kind: Some(page_kind.into()),
            ..self
        }
    }

    pub(crate) fn remove_page_kind(self) -> Self {
        RustdocParams {
            page_kind: None,
            ..self
        }
    }

    pub(crate) fn parse_with_metadata(self, metadata: &MetaData) -> ParsedRustdocParams {
        self.parse(
            metadata.default_target.as_deref(),
            metadata.target_name.as_deref(),
            metadata.doc_targets.iter().flatten(),
        )
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
        .ok_or(AxumNope::VersionNotFound)?;

        Ok(self.parse(
            krate.default_target.as_deref(),
            krate.target_name.as_deref(),
            krate
                .doc_targets
                .map(MetaData::parse_doc_targets)
                .into_iter()
                .flatten(),
        ))
    }

    fn with_fixed_target_and_path<D, I, V>(
        mut self,
        default_target: Option<D>,
        doc_targets: I,
    ) -> RustdocParams
    where
        D: Into<String>,
        I: IntoIterator<Item = V>,
        V: Into<String>,
    {
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

        let mut new_inner_path = if let Some(ref path) = self.inner_path {
            path.trim_start_matches('/').trim().to_string()
        } else {
            String::new()
        };

        let mut new_doc_target: Option<String> = None;

        if let Some(ref given_doc_target) = self.doc_target
            && !given_doc_target.trim().is_empty()
        {
            let given_target = given_doc_target.trim();
            // if a target is given in a separate url parameter, check if it's a target we
            // know about. If yes, keep it, if not, make it part of the path.
            if doc_targets.iter().any(|s| s == given_target) {
                new_doc_target = Some(given_target.into());
            } else {
                new_doc_target = None;
                if !new_inner_path.is_empty() {
                    new_inner_path = format!("{}/{}", given_target, new_inner_path);
                } else if self.has_trailing_slash() {
                    new_inner_path = format!("{}/", given_target);
                } else {
                    new_inner_path = given_target.into();
                }
            }
        } else {
            // there is no separate target component given in the route parameters.
            // We look at the first component of the path and see if it matches a target.

            if let Some(pos) = new_inner_path.find('/') {
                let potential_target = &new_inner_path[..pos];

                if doc_targets.iter().any(|s| s == potential_target) {
                    new_doc_target = Some(potential_target.to_owned());
                    new_inner_path = new_inner_path
                        .get((pos + 1)..)
                        .map(ToOwned::to_owned)
                        .unwrap_or_default();
                }
            } else {
                // no slash in the path, can be target or inner path
                if doc_targets.iter().any(|s| s == &new_inner_path) {
                    new_doc_target = Some(new_inner_path.to_owned());
                    new_inner_path.clear();
                } else {
                    new_doc_target = None;
                }
            };
        }

        if let Some(ref new_target) = new_doc_target {
            debug_assert!(!new_target.contains('/'));
            debug_assert!(new_target.contains('-'));
        }

        debug_assert!(!new_inner_path.starts_with('/')); // we should trim leading slashes

        self.inner_path = Some(new_inner_path);
        self.doc_target = new_doc_target;

        self
    }

    pub(crate) fn parse<D, T, I, V>(
        self,
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
        let doc_targets: Vec<_> = doc_targets.into_iter().map(Into::into).collect();
        let default_target = default_target.map(Into::into);
        let inner = self.with_fixed_target_and_path(default_target.as_deref(), doc_targets.iter());

        let mut merged_inner_path = inner.inner_path().to_owned();
        if matches!(inner.page_kind, Some(PageKind::Rustdoc)) {
            if let Some(ref static_route_suffix) = inner.static_route_suffix
                && !static_route_suffix.is_empty()
            {
                if !merged_inner_path.is_empty() {
                    merged_inner_path.push('/');
                }
                merged_inner_path.push_str(&static_route_suffix);
            }
        };

        let target_name = target_name.map(Into::into);
        debug_assert!(target_name.as_ref().map(|s| !s.is_empty()).unwrap_or(true));

        ParsedRustdocParams {
            doc_targets,
            default_target,
            target_name,
            merged_inner_path,
            inner,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn version(&self) -> &ReqVersion {
        &self.version
    }

    pub(crate) fn inner_path(&self) -> &str {
        self.inner_path.as_deref().unwrap_or_default()
    }

    pub(crate) fn doc_target(&self) -> Option<&str> {
        self.doc_target.as_deref()
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.original_uri
            .as_ref()
            .and_then(|uri| get_file_extension(uri.path()))
    }

    pub(crate) fn page_kind(&self) -> Option<&PageKind> {
        self.page_kind.as_ref()
    }

    fn path_for_rustdoc_url(&self) -> String {
        if matches!(self.page_kind, Some(PageKind::Rustdoc)) {
            generate_rustdoc_path_for_url(
                None,
                None,
                self.doc_target.as_deref(),
                self.inner_path.as_deref(),
            )
        } else {
            generate_rustdoc_path_for_url(None, None, self.doc_target.as_deref(), None)
        }
    }

    pub(crate) fn rustdoc_url(&self) -> EscapedURI {
        generate_rustdoc_url(&self.name, &self.version, &self.path_for_rustdoc_url())
    }

    pub(crate) fn crate_details_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!("/crate/{}/{}", self.name, self.version))
    }

    pub(crate) fn platforms_partial_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!(
            "/crate/{}/{}/menus/platforms/{}",
            self.name,
            self.version,
            self.path_for_rustdoc_url()
        ))
    }

    pub(crate) fn releases_partial_url(&self) -> EscapedURI {
        EscapedURI::from_path(format!(
            "/crate/{}/{}/menus/releases/{}",
            self.name,
            self.version,
            self.path_for_rustdoc_url()
        ))
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
        let inner_path = if matches!(self.page_kind, Some(PageKind::Source)) {
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
    merged_inner_path: String,
}

impl ParsedRustdocParams {
    pub(crate) fn name(&self) -> &str {
        &self.inner.name
    }

    pub(crate) fn version(&self) -> &ReqVersion {
        &self.inner.version
    }

    pub(crate) fn with_version(self, version: impl Into<ReqVersion>) -> Self {
        self.update(|inner| inner.with_version(version))
    }

    pub(crate) fn with_doc_target(self, doc_target: impl Into<String>) -> Result<Self> {
        let doc_target = doc_target.into();

        if !self.doc_targets.iter().any(|s| s == &doc_target) {
            bail!("unknown doc target: {}", doc_target);
        }

        Ok(self.update(|inner| inner.with_doc_target(doc_target)))
    }

    pub(crate) fn with_inner_path(self, inner_path: impl Into<String>) -> Self {
        self.update(|inner| inner.with_inner_path(inner_path))
    }

    pub(crate) fn with_page_kind(self, page_kind: impl Into<PageKind>) -> Self {
        self.update(|inner| inner.with_page_kind(page_kind))
    }

    pub(crate) fn remove_page_kind(self) -> Self {
        self.update(|inner| inner.remove_page_kind())
    }

    /// generate a potential storage path where to find the file that is described by these params.
    ///
    /// This is the path _inside_ the ZIP file we create in the build process.
    pub(crate) fn storage_path(&self) -> String {
        let mut storage_path = self.path_for_rustdoc_url();

        if path_is_folder(&storage_path) {
            storage_path.push_str(INDEX_HTML);
        }

        storage_path
    }

    pub(crate) fn doc_target(&self) -> Option<&str> {
        self.inner.doc_target()
    }

    pub(crate) fn doc_target_or_default(&self) -> Option<&str> {
        self.doc_target().or(self.default_target.as_deref())
    }

    pub(crate) fn path_is_folder(&self) -> bool {
        // TODO: not sure if this works all the time?
        path_is_folder(self.inner.original_path())
    }

    pub(crate) fn file_extension(&self) -> Option<&str> {
        self.inner.file_extension()
    }

    pub(crate) fn inner_path(&self) -> &str {
        &self.merged_inner_path
    }

    /// check if we have a target component in the path, that matches the default
    /// target. This affects the geneated storage path, since default target docs are at the root,
    /// and the other target docs are in subfolders named after the target.
    pub(crate) fn target_is_default(&self) -> bool {
        self.default_target
            .as_deref()
            .map_or(false, |t| self.doc_target() == Some(t))
    }

    pub(crate) fn update<F>(self, f: F) -> Self
    where
        F: FnOnce(RustdocParams) -> RustdocParams,
    {
        trace!(?self, "update: before");
        let new_inner = f(self.inner);
        trace!(?new_inner, "update: data set");
        let result = new_inner.parse(self.default_target, self.target_name, self.doc_targets);
        trace!(?result, "update: parse");
        result
    }

    pub(crate) fn page_kind(&self) -> Option<&PageKind> {
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

    pub(crate) fn platforms_partial_url(&self) -> EscapedURI {
        self.inner.platforms_partial_url()
    }

    pub(crate) fn releases_partial_url(&self) -> EscapedURI {
        self.inner.releases_partial_url()
    }

    pub(crate) fn features_url(&self) -> EscapedURI {
        self.inner.features_url()
    }

    fn path_for_rustdoc_url(&self) -> String {
        if matches!(self.page_kind(), Some(PageKind::Rustdoc)) {
            generate_rustdoc_path_for_url(
                self.target_name.as_deref(),
                self.default_target.as_deref(),
                self.doc_target(),
                Some(self.inner_path()),
            )
        } else {
            generate_rustdoc_path_for_url(
                self.target_name.as_deref(),
                self.default_target.as_deref(),
                self.doc_target(),
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
    fn generate_fallback_search(&self) -> Option<String> {
        // we already split out the potentially leading target information in `Self::parse`.
        // So we have an optional target, and then the path.
        let components: Vec<_> = self
            .inner_path()
            .trim_start_matches('/')
            .split('/')
            .collect();

        let is_source_view = components.first() == Some(&"src");

        components
            .last()
            .and_then(|&last_component| {
                if last_component.is_empty() || last_component == INDEX_HTML {
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
            })
            .map(ToString::to_string)
    }

    pub(crate) fn generate_fallback_url(&self) -> EscapedURI {
        let rustdoc_url = self.clone().with_inner_path("").rustdoc_url();

        if let Some(search_item) = self.generate_fallback_search() {
            rustdoc_url.append_query_pair("search", search_item)
        } else {
            rustdoc_url
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

fn generate_rustdoc_path_for_url(
    target_name: Option<&str>,
    default_target: Option<&str>,
    doc_target: Option<&str>,
    inner_path: Option<&str>,
) -> String {
    // special case:
    // when we find an empty `inner_path` or when it's just "index.html", we assume we have to
    // default to `target_name/`.
    let inner_path = if let Some(target_name) = target_name {
        if let Some(inner_path) = inner_path
            && !inner_path.is_empty()
            && inner_path != INDEX_HTML
        {
            inner_path.to_owned()
        } else {
            format!("{}/", target_name)
        }
    } else {
        inner_path.unwrap_or_default().to_string()
    };

    let path = if let Some(doc_target) = doc_target {
        if let Some(default_target) = default_target
            && doc_target == default_target
        {
            // when we have a target url param and it matches the default target
            // we don't include it in the storage path.
            // Files for the default target are placed at the root of the archive.
            inner_path
        } else {
            // all non-default targets are in subfolders named after that target.
            format!("{}/{}", doc_target, inner_path)
        }
    } else {
        // without target in the url params, we can just use the path.
        inner_path
    };

    // for folders we might have `index.html` at the end.
    // We want to normalize the requests here, so a trailing `/index.html` will be cut off.
    if path == INDEX_HTML {
        "".into()
    } else if path.ends_with(FOLDER_AND_INDEX_HTML) {
        path.trim_end_matches(INDEX_HTML).to_string()
    } else {
        path
    }
}

fn path_is_folder(path: impl AsRef<str>) -> bool {
    let path = path.as_ref();
    path.is_empty() || path.ends_with('/')
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

    static KRATE: &str = "krate";
    static DEFAULT_TARGET: &str = "x86_64-unknown-linux-gnu";
    static OTHER_TARGET: &str = "x86_64-pc-windows-msvc";
    static UNKNOWN_TARGET: &str = "some-unknown-target";
    static TARGETS: &[&str] = &[DEFAULT_TARGET, OTHER_TARGET];

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
        RustdocParams::new(KRATE)
            .try_with_original_uri("/krate").unwrap()
            .with_page_kind(PageKind::Rustdoc);
        "just name"
    )]
    #[test_case(
        "/{name}/",
        RustdocParams::new(KRATE)
            .try_with_original_uri("/krate/").unwrap()
            .with_page_kind(PageKind::Rustdoc);
        "just name with trailing slash"
    )]
    #[test_case(
        "/{name}/{version}",
        RustdocParams::new(KRATE)
            .try_with_original_uri("/krate/latest").unwrap()
            .with_page_kind(PageKind::Rustdoc);
        "just name and version"
    )]
    #[test_case(
        "/{name}/{version}/{*path}",
        RustdocParams::new(KRATE)
            .try_with_original_uri("/krate/latest/static.html").unwrap()
            .with_inner_path("static.html")
            .with_page_kind(PageKind::Rustdoc);
        "name, version, path extract"
    )]
    #[test_case(
        "/{name}/{version}/{path}/static.html",
        RustdocParams::new(KRATE)
            .try_with_original_uri("/krate/latest/path_add/static.html").unwrap()
            .with_inner_path("path_add")
            .with_static_route_suffix("static.html")
            .with_page_kind(PageKind::Rustdoc);
        "name, version, path extract, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/clapproc%20%60macro.html",
        RustdocParams::new("clap")
            .try_with_original_uri("/clap/latest/clapproc%20%60macro.html").unwrap()
            .with_static_route_suffix("clapproc `macro.html")
            .with_page_kind(PageKind::Rustdoc);
        "name, version, static suffix with some urlencoding"
    )]
    #[test_case(
        "/{name}/{version}/static.html",
        RustdocParams::new(KRATE)
            .try_with_original_uri("/krate/latest/static.html").unwrap()
            .with_static_route_suffix("static.html")
            .with_page_kind(PageKind::Rustdoc);
        "name, version, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}",
        RustdocParams::new(KRATE)
            .try_with_version("1.2.3").unwrap()
            .try_with_original_uri(format!("/krate/1.2.3/{OTHER_TARGET}")).unwrap()
            .with_doc_target(OTHER_TARGET)
            .with_page_kind(PageKind::Rustdoc);
        "name, version, target"
    )]
    #[test_case(
        "/{name}/{version}/{target}/folder/something.html",
        RustdocParams::new(KRATE)
            .try_with_version("1.2.3").unwrap()
            .try_with_original_uri(format!("/krate/1.2.3/{OTHER_TARGET}/folder/something.html")).unwrap()
            .with_doc_target(OTHER_TARGET)
            .with_static_route_suffix("folder/something.html")
            .with_page_kind(PageKind::Rustdoc);
        "name, version, target, static suffix"
    )]
    #[test_case(
        "/{name}/{version}/{target}/",
        RustdocParams::new(KRATE)
            .try_with_version("1.2.3").unwrap()
            .try_with_original_uri(format!("/krate/1.2.3/{OTHER_TARGET}/")).unwrap()
            .with_doc_target(OTHER_TARGET)
            .with_page_kind(PageKind::Rustdoc);
        "name, version, target trailing slash"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{*path}",
        RustdocParams::new(KRATE)
            .try_with_version("1.2.3").unwrap()
            .try_with_original_uri(format!("/krate/1.2.3/{OTHER_TARGET}/some/path/to/a/file.html")).unwrap()
            .with_doc_target(OTHER_TARGET)
            .with_inner_path("some/path/to/a/file.html")
            .with_page_kind(PageKind::Rustdoc);
        "name, version, target, path"
    )]
    #[test_case(
        "/{name}/{version}/{target}/{path}/path/to/a/file.html",
        RustdocParams::new(KRATE)
            .try_with_version("1.2.3").unwrap()
            .try_with_original_uri(format!("/krate/1.2.3/{OTHER_TARGET}/path_add/path/to/a/file.html")).unwrap()
            .with_doc_target(OTHER_TARGET)
            .with_inner_path("path_add")
            .with_static_route_suffix("path/to/a/file.html")
            .with_page_kind(PageKind::Rustdoc);
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
        None, "", "krate/index.html";
        "super empty 1"
    )]
    #[test_case(
        Some(""), Some(""), false,
        None, "", "krate/index.html";
        "super empty 2"
    )]
    // test cases when no separate "target" component was present in the params
    #[test_case(
        None, Some("/"), true,
        None, "", "krate/index.html";
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
        None, Some(DEFAULT_TARGET), false,
        Some(DEFAULT_TARGET), "", "krate/index.html";
        "just target without trailing slash"
    )]
    #[test_case(
        None, Some(&format!("{DEFAULT_TARGET}/")), true,
        Some(DEFAULT_TARGET), "", "krate/index.html";
        "just default target with trailing slash"
    )]
    #[test_case(
        None, Some(&format!("{DEFAULT_TARGET}/one")), false,
        Some(DEFAULT_TARGET), "one", "one";
        "target + one without trailing slash"
    )]
    #[test_case(
        None, Some(&format!("{DEFAULT_TARGET}/one/")), true,
        Some(DEFAULT_TARGET), "one/", "one/index.html";
        "target + one target with trailing slash"
    )]
    #[test_case(
        None, Some(&format!("{UNKNOWN_TARGET}/one/")), true,
        None, &format!("{UNKNOWN_TARGET}/one/"), &format!("{UNKNOWN_TARGET}/one/index.html");
        "unknown target stays in path"
    )]
    #[test_case(
        None, Some(&format!("{DEFAULT_TARGET}/some/inner/path")), false,
        Some(DEFAULT_TARGET), "some/inner/path", "some/inner/path";
        "all without trailing slash"
    )]
    #[test_case(
        None, Some(&format!("{DEFAULT_TARGET}/some/inner/path/")), true,
        Some(DEFAULT_TARGET), "some/inner/path/", "some/inner/path/index.html";
        "all with trailing slash"
    )]
    // here we have a separate target path parameter, we check it and use it accordingly
    #[test_case(
        Some(DEFAULT_TARGET), None, false,
        Some(DEFAULT_TARGET), "", "krate/index.html";
        "actual target, that is default"
    )]
    #[test_case(
        Some(DEFAULT_TARGET), Some("inner/path.html"), false,
        Some(DEFAULT_TARGET), "inner/path.html", "inner/path.html";
        "actual target with path"
    )]
    #[test_case(
        Some(DEFAULT_TARGET), Some("inner/path/"), true,
        Some(DEFAULT_TARGET), "inner/path/", "inner/path/index.html";
        "actual target with path slash"
    )]
    #[test_case(
        Some(UNKNOWN_TARGET), None, true,
        None, &format!("{UNKNOWN_TARGET}/"), &format!("{UNKNOWN_TARGET}/index.html");
        "unknown target"
    )]
    #[test_case(
        Some(UNKNOWN_TARGET), None, false,
        None, UNKNOWN_TARGET, UNKNOWN_TARGET;
        "unknown target without trailing slash"
    )]
    #[test_case(
        Some(UNKNOWN_TARGET), Some("inner/path.html"), false,
        None, &format!("{UNKNOWN_TARGET}/inner/path.html"), &format!("{UNKNOWN_TARGET}/inner/path.html");
        "unknown target with path"
    )]
    #[test_case(
        Some(OTHER_TARGET), Some("inner/path.html"), false,
        Some(OTHER_TARGET), "inner/path.html", &format!("{OTHER_TARGET}/inner/path.html");
        "other target with path"
    )]
    #[test_case(
        Some(UNKNOWN_TARGET), Some("inner/path/"), true,
        None, &format!("{UNKNOWN_TARGET}/inner/path/"), &format!("{UNKNOWN_TARGET}/inner/path/index.html");
        "unknown target with path slash"
    )]
    #[test_case(
        Some(OTHER_TARGET), Some("inner/path/"), true,
        Some(OTHER_TARGET), "inner/path/", &format!("{OTHER_TARGET}/inner/path/index.html");
        "other target with path slash"
    )]
    #[test_case(
        Some(DEFAULT_TARGET), None, false,
        Some(DEFAULT_TARGET), "", "krate/index.html";
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

        let parsed = RustdocParams::new(KRATE)
            .with_version(ReqVersion::Latest)
            .with_maybe_doc_target(target)
            .with_maybe_inner_path(path)
            .try_with_original_uri(&dummy_path)
            .unwrap()
            .parse(DEFAULT_TARGET.into(), KRATE.into(), TARGETS.iter().cloned());

        assert_eq!(parsed.name(), KRATE);
        assert_eq!(parsed.version(), &ReqVersion::Latest);
        assert_eq!(parsed.doc_target(), expected_target);
        assert_eq!(parsed.inner_path(), expected_path);
        assert_eq!(parsed.storage_path(), expected_storage_path);
        assert_eq!(
            parsed.path_is_folder(),
            had_trailing_slash || dummy_path.ends_with('/') || dummy_path.is_empty()
        );
    }

    #[test_case("dummy/struct.WindowsOnly.html", Some("WindowsOnly"))]
    #[test_case("dummy/some_module/struct.SomeItem.html", Some("SomeItem"))]
    #[test_case("dummy/some_module/index.html", Some("some_module"))]
    #[test_case("dummy/some_module/", Some("some_module"))]
    #[test_case("src/folder1/folder2/logic.rs.html", Some("logic"))]
    #[test_case("src/non_source_file.rs", None)]
    #[test_case("html", None; "plain file without extension")]
    #[test_case("something.html", Some("html"); "plain file")]
    #[test_case("", None)]
    fn test_generate_fallback_search(path: &str, search: Option<&str>) {
        let mut params = RustdocParams::new("dummy")
            .try_with_version("0.4.0")
            .unwrap()
            // non-default target, target stays in the url
            .with_doc_target(OTHER_TARGET)
            .with_inner_path(path)
            .parse(
                DEFAULT_TARGET.into(),
                "dummy".into(),
                TARGETS.iter().cloned(),
            );

        assert_eq!(params.generate_fallback_search().as_deref(), search);
        assert_eq!(
            params.generate_fallback_url().to_string(),
            format!(
                "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/{}",
                search.map(|s| format!("?search={}", s)).unwrap_or_default()
            )
        );

        // change to default target, check url again
        params = params.with_doc_target(DEFAULT_TARGET).unwrap();

        assert_eq!(params.generate_fallback_search().as_deref(), search);
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
        let params = RustdocParams::new("dummy")
            .try_with_version("0.4.0")
            .unwrap()
            .with_inner_path("README.md")
            .with_page_kind(PageKind::Source)
            .try_with_original_uri("/crate/dummy/0.4.0/source/README.md")
            .unwrap()
            .parse(
                DEFAULT_TARGET.into(),
                "dummy".into(),
                TARGETS.iter().cloned(),
            );

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

    #[test_case(
        None, None, None, None => ""
    )]
    #[test_case(
        Some("target_name"), None, None, None => "target_name/"
    )]
    #[test_case(
        None, None, None, Some("path/index.html") => "path/";
        "cuts trailing /index.html"
    )]
    #[test_case(
        Some("target_name"), None,
        Some(DEFAULT_TARGET), Some("inner/path.html")
        => "x86_64-unknown-linux-gnu/inner/path.html";
        "default target, but we don't know about it, keeps target"
    )]
    #[test_case(
        Some("target_name"), None,
        Some(DEFAULT_TARGET), None
        => "x86_64-unknown-linux-gnu/target_name/";
        "default target, we don't know about it, without path"
    )]
    #[test_case(
        Some("target_name"), Some(DEFAULT_TARGET),
        Some(DEFAULT_TARGET), None
        => "target_name/";
        "default-target, without path, target_name is used to generate the inner path"
    )]
    #[test_case(
        Some("target_name"), Some(DEFAULT_TARGET),
        Some(DEFAULT_TARGET), Some("inner/path.html")
        => "inner/path.html";
        "default target, with path, target_name is ignored"
    )]
    #[test_case(
        None, Some(DEFAULT_TARGET),
        Some(DEFAULT_TARGET), Some("inner/path/index.html")
        => "inner/path/";
        "default target, with path as folder with index.html"
    )]
    #[test_case(
        None, Some(DEFAULT_TARGET),
        Some(DEFAULT_TARGET), Some("inner/path/")
        => "inner/path/";
        "default target, with path as folder"
    )]
    #[test_case(
        Some("target_name"), Some(DEFAULT_TARGET),
        Some(OTHER_TARGET), None
        => "x86_64-pc-windows-msvc/target_name/";
        "non-default-target, without path, target_name is used to generate the inner path"
    )]
    #[test_case(
        Some("target_name"), Some(DEFAULT_TARGET),
        Some(OTHER_TARGET), Some("inner/path.html")
        => "x86_64-pc-windows-msvc/inner/path.html";
        "non-default target, with path, target_name is ignored"
    )]
    fn test_generate_rustdoc_path_for_url(
        target_name: Option<&str>,
        default_target: Option<&str>,
        doc_target: Option<&str>,
        inner_path: Option<&str>,
    ) -> String {
        generate_rustdoc_path_for_url(target_name, default_target, doc_target, inner_path)
    }

    #[test]
    fn test_case_1() {
        let params = RustdocParams::new("dummy")
            .try_with_version("0.2.0")
            .unwrap()
            .with_inner_path("struct.Dummy.html")
            .with_doc_target("dummy")
            .with_page_kind(PageKind::Rustdoc)
            .try_with_original_uri("/dummy/0.2.0/dummy/struct.Dummy.html")
            .unwrap()
            .parse(Some(DEFAULT_TARGET), Some("dummy"), TARGETS.iter().cloned());

        assert!(params.doc_target().is_none());
        assert_eq!(params.inner_path(), "dummy/struct.Dummy.html");
        assert_eq!(params.storage_path(), "dummy/struct.Dummy.html");

        let params = params.with_doc_target(DEFAULT_TARGET).unwrap();
        assert_eq!(params.doc_target(), Some(DEFAULT_TARGET));
        assert_eq!(params.inner_path(), "dummy/struct.Dummy.html");
        assert_eq!(params.storage_path(), "dummy/struct.Dummy.html");

        let params = params.with_doc_target(OTHER_TARGET).unwrap();
        assert_eq!(params.doc_target(), Some(OTHER_TARGET));
        assert_eq!(
            params.storage_path(),
            format!("{OTHER_TARGET}/dummy/struct.Dummy.html")
        );
        assert_eq!(
            params.storage_path(),
            format!("{OTHER_TARGET}/dummy/struct.Dummy.html")
        );
    }

    #[test_case(
        "/",
        None, None,
        None, ""
        ; "no target, no path"
    )]
    #[test_case(
        &format!("/{DEFAULT_TARGET}"),
        Some(DEFAULT_TARGET), None,
        Some(DEFAULT_TARGET), "";
        "existing target, no path"
    )]
    #[test_case(
        &format!("/{UNKNOWN_TARGET}"),
        Some(UNKNOWN_TARGET), None,
        None, UNKNOWN_TARGET;
        "unknown target, no path"
    )]
    #[test_case(
        &format!("/{UNKNOWN_TARGET}/"),
        Some(UNKNOWN_TARGET), Some("something/file.html"),
        None, &format!("{UNKNOWN_TARGET}/something/file.html");
        "unknown target, with path, trailling slash is kept"
    )]
    #[test_case(
        &format!("/{UNKNOWN_TARGET}/"),
        Some(UNKNOWN_TARGET), None,
        None, &format!("{UNKNOWN_TARGET}/");
        "unknown target, no path, trailling slash is kept"
    )]
    fn test_with_fixed_target_and_path(
        original_uri: &str,
        target: Option<&str>,
        path: Option<&str>,
        expected_target: Option<&str>,
        expected_path: &str,
    ) {
        let params = RustdocParams::new(KRATE)
            .try_with_version("0.4.0")
            .unwrap()
            .try_with_original_uri(original_uri)
            .unwrap()
            .with_maybe_doc_target(target)
            .with_maybe_inner_path(path)
            .with_fixed_target_and_path(DEFAULT_TARGET.into(), TARGETS.iter().cloned());

        dbg!(&params);

        assert_eq!(params.doc_target(), expected_target);
        assert_eq!(params.inner_path(), expected_path);
    }

    #[test]
    fn test_validate_unknown_doc_target() {
        let params = RustdocParams::new(KRATE)
            .try_with_version("0.4.0")
            .unwrap()
            // this works, because we don't have the doc-target list here
            .try_with_original_uri(format!("/{UNKNOWN_TARGET}/"))
            .unwrap()
            .with_doc_target(UNKNOWN_TARGET);

        assert_eq!(params.doc_target(), Some(UNKNOWN_TARGET));

        // now, parsing the params into parsed-params
        // also works with the unknown target, but it's moved to the path
        let params = params.parse(DEFAULT_TARGET.into(), KRATE.into(), TARGETS.iter().cloned());

        assert!(params.doc_target().is_none());
        assert_eq!(params.inner_path(), &format!("{UNKNOWN_TARGET}/"));

        // this breaks!
        assert!(params.clone().with_doc_target(UNKNOWN_TARGET).is_err());

        dbg!(&params);
        let params = params
            .with_doc_target(OTHER_TARGET)
            .expect("should succeed");
        dbg!(&params);

        assert_eq!(params.doc_target(), Some(OTHER_TARGET));

        // reason is:
        // when we see an unknown target in the params, we move it to the inner_path
        // when we then overwrite the now empty target with a new, valid target,
        // the path is still there.
        assert_eq!(params.inner_path(), format!("{UNKNOWN_TARGET}/"));
    }

    #[test_case(
        None, None,
        None, None
        => "";
        "empty"
    )]
    #[test_case(
        None, None,
        None, Some("folder/index.html")
        => "folder/";
        "just folder index.html will be removed"
    )]
    #[test_case(
        None, None,
        None, Some(INDEX_HTML)
        => "";
        "just root index.html will be removed"
    )]
    #[test_case(
        None, Some(DEFAULT_TARGET),
        Some(DEFAULT_TARGET), None
        => "";
        "just default target"
    )]
    #[test_case(
        None, Some(DEFAULT_TARGET),
        Some(OTHER_TARGET), None
        => format!("{OTHER_TARGET}/");
        "just other target"
    )]
    #[test_case(
        Some(KRATE), Some(DEFAULT_TARGET),
        Some(DEFAULT_TARGET), None
        => format!("{KRATE}/");
        "full with default target, target name is used"
    )]
    #[test_case(
        Some(KRATE), Some(DEFAULT_TARGET),
        Some(OTHER_TARGET), None
        => format!("{OTHER_TARGET}/{KRATE}/");
        "full with other target, target name is used"
    )]
    #[test_case(
        Some(KRATE), Some(DEFAULT_TARGET),
        Some(DEFAULT_TARGET), Some("inner/something.html")
        => "inner/something.html";
        "full with default target, target name is ignored"
    )]
    #[test_case(
        Some(KRATE), Some(DEFAULT_TARGET),
        Some(OTHER_TARGET), Some("inner/something.html")
        => format!("{OTHER_TARGET}/inner/something.html");
        "full with other target, target name is ignored"
    )]
    fn test_rustdoc_path_for_url(
        target_name: Option<&str>,
        default_target: Option<&str>,
        doc_target: Option<&str>,
        inner_path: Option<&str>,
    ) -> String {
        generate_rustdoc_path_for_url(target_name, default_target, doc_target, inner_path)
    }
}
