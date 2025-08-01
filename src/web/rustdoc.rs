//! rustdoc handler

use crate::{
    AsyncStorage, Config, InstanceMetrics, RUSTDOC_STATIC_STORAGE_PREFIX,
    db::Pool,
    storage::{
        CompressionAlgorithm, RustdocJsonFormatVersion, StreamingBlob,
        compression::compression_from_file_extension, rustdoc_archive_path, rustdoc_json_path,
    },
    utils,
    web::{
        MetaData, ReqVersion, axum_cached_redirect, axum_parse_uri_with_params,
        cache::CachePolicy,
        crate_details::CrateDetails,
        csp::Csp,
        encode_url_path,
        error::{AxumNope, AxumResult, EscapedURI},
        extractors::{DbConnection, Path},
        file::StreamingFile,
        match_version,
        page::{
            TemplateData,
            templates::{RenderBrands, RenderRegular, RenderSolid, filters},
        },
    },
};
use anyhow::{Context as _, anyhow};
use askama::Template;
use axum::{
    body::Body,
    extract::{Extension, Query},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response as AxumResponse},
};
use http::{HeaderValue, header};
use semver::Version;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};
use tracing::{Instrument, debug, error, info_span, instrument, trace};

use super::extractors::PathFileExtension;

pub(crate) struct OfficialCrateDescription {
    pub(crate) name: &'static str,
    pub(crate) href: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) static DOC_RUST_LANG_ORG_REDIRECTS: LazyLock<HashMap<&str, OfficialCrateDescription>> =
    LazyLock::new(|| {
        HashMap::from([
            (
                "alloc",
                OfficialCrateDescription {
                    name: "alloc",
                    href: "https://doc.rust-lang.org/stable/alloc/",
                    description: "Rust alloc library",
                },
            ),
            (
                "liballoc",
                OfficialCrateDescription {
                    name: "alloc",
                    href: "https://doc.rust-lang.org/stable/alloc/",
                    description: "Rust alloc library",
                },
            ),
            (
                "core",
                OfficialCrateDescription {
                    name: "core",
                    href: "https://doc.rust-lang.org/stable/core/",
                    description: "Rust core library",
                },
            ),
            (
                "libcore",
                OfficialCrateDescription {
                    name: "core",
                    href: "https://doc.rust-lang.org/stable/core/",
                    description: "Rust core library",
                },
            ),
            (
                "proc_macro",
                OfficialCrateDescription {
                    name: "proc_macro",
                    href: "https://doc.rust-lang.org/stable/proc_macro/",
                    description: "Rust proc_macro library",
                },
            ),
            (
                "libproc_macro",
                OfficialCrateDescription {
                    name: "proc_macro",
                    href: "https://doc.rust-lang.org/stable/proc_macro/",
                    description: "Rust proc_macro library",
                },
            ),
            (
                "proc-macro",
                OfficialCrateDescription {
                    name: "proc_macro",
                    href: "https://doc.rust-lang.org/stable/proc_macro/",
                    description: "Rust proc_macro library",
                },
            ),
            (
                "libproc-macro",
                OfficialCrateDescription {
                    name: "proc_macro",
                    href: "https://doc.rust-lang.org/stable/proc_macro/",
                    description: "Rust proc_macro library",
                },
            ),
            (
                "std",
                OfficialCrateDescription {
                    name: "std",
                    href: "https://doc.rust-lang.org/stable/std/",
                    description: "Rust standard library",
                },
            ),
            (
                "libstd",
                OfficialCrateDescription {
                    name: "std",
                    href: "https://doc.rust-lang.org/stable/std/",
                    description: "Rust standard library",
                },
            ),
            (
                "test",
                OfficialCrateDescription {
                    name: "test",
                    href: "https://doc.rust-lang.org/stable/test/",
                    description: "Rust test library",
                },
            ),
            (
                "libtest",
                OfficialCrateDescription {
                    name: "test",
                    href: "https://doc.rust-lang.org/stable/test/",
                    description: "Rust test library",
                },
            ),
            (
                "rustc",
                OfficialCrateDescription {
                    name: "rustc",
                    href: "https://doc.rust-lang.org/nightly/nightly-rustc/",
                    description: "rustc API",
                },
            ),
            (
                "rustdoc",
                OfficialCrateDescription {
                    name: "rustdoc",
                    href: "https://doc.rust-lang.org/nightly/nightly-rustc/rustdoc/",
                    description: "rustdoc API",
                },
            ),
        ])
    });

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RustdocRedirectorParams {
    name: String,
    version: Option<ReqVersion>,
    target: Option<String>,
}

/// try to serve a toolchain specific asset from the legacy location.
///
/// Newer rustdoc builds use a specific subfolder on the bucket,
/// a new `static-root-path` prefix (`/-/rustdoc.static/...`), which
/// is served via our `static_asset_handler`.
///
/// The legacy location is the root, both on the bucket & the URL
/// path, which is suboptimal since the route overlaps with other routes.
///
/// See also https://github.com/rust-lang/docs.rs/pull/1889
async fn try_serve_legacy_toolchain_asset(
    storage: Arc<AsyncStorage>,
    path: impl AsRef<str>,
) -> AxumResult<AxumResponse> {
    let path = path.as_ref().to_owned();
    // FIXME: this could be optimized: when a path doesn't exist
    // in storage, we don't need to recheck on every request.
    // Existing files are returned with caching headers, so
    // are cached by the CDN.
    // If cached, it doesn't need to be invalidated,
    // since new nightly versions will always put their
    // toolchain specific resources into the new folder,
    // which is reached via the new handler.
    Ok(StreamingFile::from_path(&storage, &path)
        .await
        .map(IntoResponse::into_response)?)
}

/// Handler called for `/:crate` and `/:crate/:version` URLs. Automatically redirects to the docs
/// or crate details page based on whether the given crate version was successfully built.
#[instrument(skip(storage, conn))]
pub(crate) async fn rustdoc_redirector_handler(
    Path(params): Path<RustdocRedirectorParams>,
    Extension(storage): Extension<Arc<AsyncStorage>>,
    mut conn: DbConnection,
    Query(query_pairs): Query<HashMap<String, String>>,
    uri: Uri,
) -> AxumResult<impl IntoResponse> {
    #[instrument]
    fn redirect_to_doc(
        query_pairs: &HashMap<String, String>,
        url_str: String,
        cache_policy: CachePolicy,
        path_in_crate: Option<&str>,
    ) -> AxumResult<impl IntoResponse> {
        let mut queries: BTreeMap<String, String> = BTreeMap::new();
        if let Some(path) = path_in_crate {
            queries.insert("search".into(), path.into());
        }
        queries.extend(query_pairs.to_owned());
        trace!("redirect to doc");
        Ok(axum_cached_redirect(
            axum_parse_uri_with_params(&url_str, queries)?,
            cache_policy,
        )?)
    }

    // global static assets for older builds are served from the root, which ends up
    // in this handler as `params.name`.
    if let Some((_, extension)) = params.name.rsplit_once('.') {
        if ["css", "js", "png", "svg", "woff", "woff2"]
            .binary_search(&extension)
            .is_ok()
        {
            return try_serve_legacy_toolchain_asset(storage, params.name)
                .instrument(info_span!("serve static asset"))
                .await;
        }
    }

    if let Some((_, extension)) = uri.path().rsplit_once('.') {
        if extension == "ico" {
            // redirect all ico requests
            // originally from:
            // https://github.com/rust-lang/docs.rs/commit/f3848a34c391841a2516a9e6ad1f80f6f490c6d0
            return Ok(axum_cached_redirect(
                "/-/static/favicon.ico",
                CachePolicy::ForeverInCdnAndBrowser,
            )?
            .into_response());
        }
    }

    let (crate_name, path_in_crate) = match params.name.split_once("::") {
        Some((krate, path)) => (krate.to_string(), Some(path.to_string())),
        None => (params.name.to_string(), None),
    };

    if let Some(description) = DOC_RUST_LANG_ORG_REDIRECTS.get(crate_name.as_str()) {
        return Ok(redirect_to_doc(
            &query_pairs,
            description.href.to_string(),
            CachePolicy::ForeverInCdnAndStaleInBrowser,
            path_in_crate.as_deref(),
        )?
        .into_response());
    }

    // it doesn't matter if the version that was given was exact or not, since we're redirecting
    // anyway
    let matched_release = match_version(
        &mut conn,
        &crate_name,
        &params.version.clone().unwrap_or_default(),
    )
    .await?
    .into_exactly_named();
    trace!(?matched_release, "matched version");
    let crate_name = matched_release.name.clone();

    // we might get requests to crate-specific JS/CSS files here.
    if let Some(ref target) = params.target {
        if target.ends_with(".js") || target.ends_with(".css") {
            // this URL is actually from a crate-internal path, serve it there instead
            return async {
                let krate = CrateDetails::from_matched_release(&mut conn, matched_release).await?;

                match storage
                    .stream_rustdoc_file(
                        &crate_name,
                        &krate.version.to_string(),
                        krate.latest_build_id,
                        target,
                        krate.archive_storage,
                    )
                    .await
                {
                    Ok(blob) => Ok(StreamingFile(blob).into_response()),
                    Err(err) => {
                        if !matches!(err.downcast_ref(), Some(AxumNope::ResourceNotFound))
                            && !matches!(
                                err.downcast_ref(),
                                Some(crate::storage::PathNotFoundError)
                            )
                        {
                            debug!(?target, ?err, "got error serving file");
                        }
                        // FIXME: we sometimes still get requests for toolchain
                        // specific static assets under the crate/version/ path.
                        // This is fixed in rustdoc, but pending a rebuild for
                        // docs that were affected by this bug.
                        // https://github.com/rust-lang/docs.rs/issues/1979
                        if target.starts_with("search-") || target.starts_with("settings-") {
                            try_serve_legacy_toolchain_asset(storage, target).await
                        } else {
                            Err(err.into())
                        }
                    }
                }
            }
            .instrument(info_span!("serve asset for crate"))
            .await;
        }
    }

    let matched_release = matched_release.into_canonical_req_version();

    if matched_release.rustdoc_status() {
        let target_name = matched_release
            .target_name()
            .expect("when rustdoc_status is true, target name exists");
        let mut target = params.target.as_deref();
        if target == Some("index.html") || target == Some(target_name) {
            target = None;
        }

        let url_str = if let Some(target) = target {
            format!(
                "/{crate_name}/{}/{target}/{}/",
                matched_release.req_version, target_name
            )
        } else {
            format!(
                "/{crate_name}/{}/{}/",
                matched_release.req_version, target_name
            )
        };

        let cache = if matched_release.is_latest_url() {
            CachePolicy::ForeverInCdn
        } else {
            CachePolicy::ForeverInCdnAndStaleInBrowser
        };

        Ok(redirect_to_doc(
            &query_pairs,
            encode_url_path(&url_str),
            cache,
            path_in_crate.as_deref(),
        )?
        .into_response())
    } else {
        Ok(axum_cached_redirect(
            EscapedURI::new(
                &format!("/crate/{crate_name}/{}", matched_release.req_version),
                uri.query(),
            )
            .as_str(),
            CachePolicy::ForeverInCdn,
        )?
        .into_response())
    }
}

#[derive(Template)]
#[template(path = "rustdoc/topbar.html")]
#[derive(Debug, Clone)]
pub struct RustdocPage {
    pub latest_path: String,
    pub permalink_path: String,
    pub inner_path: String,
    // true if we are displaying the latest version of the crate, regardless
    // of whether the URL specifies a version number or the string "latest."
    pub is_latest_version: bool,
    // true if the URL specifies a version using the string "latest."
    pub is_latest_url: bool,
    pub is_prerelease: bool,
    pub krate: CrateDetails,
    pub metadata: MetaData,
    pub current_target: String,
}

impl RustdocPage {
    async fn into_response(
        self: &Arc<Self>,
        template_data: Arc<TemplateData>,
        metrics: Arc<InstanceMetrics>,
        rustdoc_html: StreamingBlob,
        max_parse_memory: usize,
    ) -> AxumResult<AxumResponse> {
        let is_latest_url = self.is_latest_url;

        Ok((
            StatusCode::OK,
            (!is_latest_url).then_some([("X-Robots-Tag", "noindex")]),
            Extension(if is_latest_url {
                CachePolicy::ForeverInCdn
            } else {
                CachePolicy::ForeverInCdnAndStaleInBrowser
            }),
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(mime::TEXT_HTML_UTF_8.as_ref()),
            )],
            Body::from_stream(utils::rewrite_rustdoc_html_stream(
                template_data,
                rustdoc_html.content,
                max_parse_memory,
                self.clone(),
                metrics,
            )),
        )
            .into_response())
    }

    pub(crate) fn use_direct_platform_links(&self) -> bool {
        !self.latest_path.contains("/target-redirect/")
    }
}

#[derive(Clone, Deserialize, Debug)]
pub(crate) struct RustdocHtmlParams {
    pub(crate) name: String,
    pub(crate) version: ReqVersion,
    // both target and path are only used for matching the route.
    // The actual path is read from the request `Uri` because
    // we have some static filenames directly in the routes.
    pub(crate) target: Option<String>,
    pub(crate) path: Option<String>,
}

/// Serves documentation generated by rustdoc.
///
/// This includes all HTML files for an individual crate, as well as the `search-index.js`, which is
/// also crate-specific.
#[allow(clippy::too_many_arguments)]
#[instrument(skip_all)]
pub(crate) async fn rustdoc_html_server_handler(
    Path(params): Path<RustdocHtmlParams>,
    Extension(metrics): Extension<Arc<InstanceMetrics>>,
    Extension(templates): Extension<Arc<TemplateData>>,
    Extension(pool): Extension<Pool>,
    Extension(storage): Extension<Arc<AsyncStorage>>,
    Extension(config): Extension<Arc<Config>>,
    Extension(csp): Extension<Arc<Csp>>,
    uri: Uri,
) -> AxumResult<AxumResponse> {
    // since we directly use the Uri-path and not the extracted params from the router,
    // we have to percent-decode the string here.
    let original_path = percent_encoding::percent_decode(uri.path().as_bytes())
        .decode_utf8()
        .map_err(|err| AxumNope::BadRequest(err.into()))?;

    let mut req_path: Vec<&str> = original_path.split('/').collect();
    // Remove the empty start, the name and the version from the path
    req_path.drain(..3).for_each(drop);

    // Pages generated by Rustdoc are not ready to be served with a CSP yet.
    csp.suppress(true);

    // Convenience function to allow for easy redirection
    #[instrument]
    fn redirect(
        name: &str,
        vers: &Version,
        path: &[&str],
        cache_policy: CachePolicy,
        uri: &Uri,
    ) -> AxumResult<AxumResponse> {
        trace!("redirect");
        Ok(axum_cached_redirect(
            EscapedURI::new(
                &format!("/{}/{}/{}", name, vers, path.join("/")),
                uri.query(),
            )
            .as_str(),
            cache_policy,
        )?
        .into_response())
    }

    trace!("match version");
    let mut conn = pool.get_async().await?;

    // Check the database for releases with the requested version while doing the following:
    // * If no matching releases are found, return a 404 with the underlying error
    // Then:
    // * If both the name and the version are an exact match, return the version of the crate.
    // * If there is an exact match, but the requested crate name was corrected (dashes vs. underscores), redirect to the corrected name.
    // * If there is a semver (but not exact) match, redirect to the exact version.
    let matched_release = match_version(&mut conn, &params.name, &params.version)
        .await?
        .into_exactly_named_or_else(|corrected_name, req_version| {
            AxumNope::Redirect(
                EscapedURI::new(
                    &format!("/{}/{}/{}", corrected_name, req_version, req_path.join("/")),
                    uri.query(),
                ),
                CachePolicy::NoCaching,
            )
        })?
        .into_canonical_req_version_or_else(|version| {
            AxumNope::Redirect(
                EscapedURI::new(
                    &format!("/{}/{}/{}", &params.name, version, req_path.join("/")),
                    None,
                ),
                CachePolicy::ForeverInCdn,
            )
        })?;

    if !matched_release.rustdoc_status() {
        return Ok(axum_cached_redirect(
            format!("/crate/{}/{}", params.name, params.version),
            CachePolicy::ForeverInCdn,
        )?
        .into_response());
    }

    let krate = CrateDetails::from_matched_release(&mut conn, matched_release).await?;

    // if visiting the full path to the default target, remove the target from the path
    // expects a req_path that looks like `[/:target]/.*`
    if req_path.first().copied()
        == Some(
            krate
                .metadata
                .default_target
                .as_ref()
                .expect("when we have docs, this is always filled"),
        )
    {
        return redirect(
            &params.name,
            &krate.version,
            &req_path[1..],
            CachePolicy::ForeverInCdn,
            &uri,
        );
    }

    // Create the path to access the file from
    let mut storage_path = req_path.join("/");
    if storage_path.ends_with('/') {
        req_path.pop(); // get rid of empty string
        storage_path.push_str("index.html");
        req_path.push("index.html");
    }

    trace!(?storage_path, ?req_path, "try fetching from storage");

    // Attempt to load the file from the database
    let blob = match storage
        .stream_rustdoc_file(
            &params.name,
            &krate.version.to_string(),
            krate.latest_build_id,
            &storage_path,
            krate.archive_storage,
        )
        .await
    {
        Ok(file) => file,
        Err(err) => {
            if !matches!(err.downcast_ref(), Some(AxumNope::ResourceNotFound))
                && !matches!(err.downcast_ref(), Some(crate::storage::PathNotFoundError))
            {
                debug!("got error serving {}: {}", storage_path, err);
            }

            {
                // If it fails, we try again with /index.html at the end
                let mut storage_path = storage_path.clone();
                storage_path.push_str("/index.html");

                let mut req_path = req_path.clone();
                req_path.push("index.html");

                if storage
                    .rustdoc_file_exists(
                        &params.name,
                        &krate.version.to_string(),
                        krate.latest_build_id,
                        &storage_path,
                        krate.archive_storage,
                    )
                    .await?
                {
                    return redirect(
                        &params.name,
                        &krate.version,
                        &req_path,
                        CachePolicy::ForeverInCdn,
                        &uri,
                    );
                }
            }

            if req_path.first().is_some_and(|p| p.contains('-')) {
                // This is a target, not a module; it may not have been built.
                // Redirect to the default target and show a search page instead of a hard 404.
                return Ok(axum_cached_redirect(
                    encode_url_path(&format!(
                        "/crate/{}/{}/target-redirect/{}",
                        params.name,
                        params.version,
                        req_path.join("/")
                    )),
                    CachePolicy::ForeverInCdn,
                )?
                .into_response());
            }

            if storage_path
                == format!(
                    "{}/index.html",
                    krate.target_name.expect(
                        "we check rustdoc_status = true above, and with docs we have target_name"
                    )
                )
            {
                error!(
                    krate = params.name,
                    version = krate.version.to_string(),
                    original_path = original_path.as_ref(),
                    storage_path,
                    "Couldn't find crate documentation root on storage.
                        Something is wrong with the build."
                )
            }

            return Err(AxumNope::ResourceNotFound);
        }
    };

    // Serve non-html files directly
    if !storage_path.ends_with(".html") {
        trace!(?storage_path, "serve asset");

        // default asset caching behaviour is `Cache::ForeverInCdnAndBrowser`.
        // This is an edge-case when we serve invocation specific static assets under `/latest/`:
        // https://github.com/rust-lang/docs.rs/issues/1593
        return Ok(StreamingFile(blob).into_response());
    }

    let latest_release = krate.latest_release()?;

    // Get the latest version of the crate
    let latest_version = latest_release.version.clone();
    let is_latest_version = latest_version == krate.version;
    let is_prerelease = !(krate.version.pre.is_empty());

    // The path within this crate version's rustdoc output
    let (target, inner_path) = {
        let mut inner_path = req_path.clone();

        let target = if inner_path.len() > 1
            && krate
                .metadata
                .doc_targets
                .as_ref()
                .expect("with rustdoc_status=true we always have doc_targets")
                .iter()
                .any(|s| s == inner_path[0])
        {
            inner_path.remove(0)
        } else {
            ""
        };

        (target, inner_path.join("/"))
    };

    // Find the path of the latest version for the `Go to latest` and `Permalink` links
    let mut current_target = String::new();
    let target_redirect = if latest_release.build_status.is_success() {
        current_target = if target.is_empty() {
            krate
                .metadata
                .default_target
                .as_ref()
                .expect("with docs we always have a default_target")
                .clone()
        } else {
            target.to_owned()
        };
        format!("/target-redirect/{current_target}/{inner_path}")
    } else {
        "".to_string()
    };

    let query_string = if let Some(query) = uri.query() {
        format!("?{query}")
    } else {
        "".to_string()
    };

    let permalink_path = format!(
        "/{}/{}/{}{}",
        params.name, latest_version, inner_path, query_string
    );

    let latest_path = format!(
        "/crate/{}/latest{}{}",
        params.name, target_redirect, query_string
    );

    metrics
        .recently_accessed_releases
        .record(krate.crate_id, krate.release_id, target);

    // Build the page of documentation,
    let page = Arc::new(RustdocPage {
        latest_path,
        permalink_path,
        inner_path,
        is_latest_version,
        is_latest_url: params.version.is_latest(),
        is_prerelease,
        metadata: krate.metadata.clone(),
        krate,
        current_target,
    });
    page.into_response(templates, metrics, blob, config.max_parse_memory)
        .await
}

/// Checks whether the given path exists.
/// The crate's `target_name` is used to confirm whether a platform triple is part of the path.
///
/// Note that path is overloaded in this context to mean both the path of a URL
/// and the file path of a static file in the DB.
///
/// `file_path` is assumed to have the following format:
/// `[/platform]/module/[kind.name.html|index.html]`
///
/// Returns a path that can be appended to `/crate/version/` to create a complete URL.
fn path_for_version(
    file_path: &[&str],
    crate_details: &CrateDetails,
) -> (String, HashMap<String, String>) {
    // check if req_path[3] is the platform choice or the name of the crate
    // Note we don't require the platform to have a trailing slash.
    let platform = if crate_details
        .metadata
        .doc_targets
        .as_ref()
        .expect("this method is only used when we have docs, so this field contains data")
        .iter()
        .any(|s| s == file_path[0])
        && !file_path.is_empty()
    {
        file_path[0]
    } else {
        ""
    };
    let is_source_view = if platform.is_empty() {
        // /{name}/{version}/src/{crate}/index.html
        file_path.first().copied() == Some("src")
    } else {
        // /{name}/{version}/{platform}/src/{crate}/index.html
        file_path.get(1).copied() == Some("src")
    };
    // this page doesn't exist in the latest version
    let last_component = *file_path.last().unwrap();
    let search_item = if last_component == "index.html" {
        // this is a module
        file_path.get(file_path.len() - 2).copied()
    // no trailing slash; no one should be redirected here but we handle it gracefully anyway
    } else if last_component == platform {
        // nothing to search for
        None
    } else if !is_source_view {
        // this is an item
        last_component.split('.').nth(1)
    } else {
        // if this is a Rust source file, try searching for the module;
        // else, don't try searching at all, we don't know how to find it
        last_component.strip_suffix(".rs.html")
    };
    let target_name = &crate_details
        .target_name
        .as_ref()
        .expect("this method is only used when we have docs, so this field contains data");
    let path = if platform.is_empty() {
        format!("{target_name}/")
    } else {
        format!("{platform}/{target_name}/")
    };

    let query_params = search_item
        .map(|i| HashMap::from_iter([("search".into(), i.into())]))
        .unwrap_or_default();

    (path, query_params)
}

#[instrument(skip_all)]
pub(crate) async fn target_redirect_handler(
    Path((name, req_version, req_path)): Path<(String, ReqVersion, String)>,
    mut conn: DbConnection,
    Extension(storage): Extension<Arc<AsyncStorage>>,
) -> AxumResult<impl IntoResponse> {
    let matched_release = match_version(&mut conn, &name, &req_version)
        .await?
        .into_canonical_req_version_or_else(|_| AxumNope::VersionNotFound)?;

    let crate_details = CrateDetails::from_matched_release(&mut conn, matched_release).await?;

    // this handler should only be used when we have docs.
    // So we can assume here that we always have a default_target.
    // the only case where this would be empty is when the build failed before calling rustdoc.
    let default_target = crate_details
        .metadata
        .default_target
        .as_ref()
        .ok_or_else(|| {
            error!("target_redirect_handler was called with release with missing default_target");
            AxumNope::VersionNotFound
        })?;

    // We're trying to find the storage location
    // for the requested path in the target-redirect.
    // *path always contains the target,
    // here we are dropping it when it's the
    // default target,
    // and add `/index.html` if we request
    // a folder.
    let storage_location_for_path = {
        let mut pieces: Vec<_> = req_path.split('/').map(str::to_owned).collect();

        if pieces.first() == Some(default_target) {
            pieces.remove(0);
        }

        if let Some(last) = pieces.last_mut() {
            if last.is_empty() {
                *last = "index.html".to_string();
            }
        }

        pieces.join("/")
    };

    let (redirect_path, query_args) = if storage
        .rustdoc_file_exists(
            &name,
            &crate_details.version.to_string(),
            crate_details.latest_build_id,
            &storage_location_for_path,
            crate_details.archive_storage,
        )
        .await?
    {
        // Simple case: page exists in the other target & version, so just change these
        (storage_location_for_path, HashMap::new())
    } else {
        let pieces: Vec<_> = storage_location_for_path.split('/').collect();
        path_for_version(&pieces, &crate_details)
    };

    Ok(axum_cached_redirect(
        axum_parse_uri_with_params(
            &encode_url_path(&format!("/{name}/{req_version}/{redirect_path}")),
            query_args,
        )?,
        if req_version.is_latest() {
            CachePolicy::ForeverInCdn
        } else {
            CachePolicy::ForeverInCdnAndStaleInBrowser
        },
    )?)
}

#[derive(Deserialize, Debug)]
pub(crate) struct BadgeQueryParams {
    version: Option<ReqVersion>,
}

#[instrument(skip_all)]
pub(crate) async fn badge_handler(
    Path(name): Path<String>,
    Query(query): Query<BadgeQueryParams>,
) -> AxumResult<impl IntoResponse> {
    let url = url::Url::parse(&format!(
        "https://img.shields.io/docsrs/{name}/{}",
        query.version.unwrap_or_default(),
    ))
    .context("could not parse URL")?;

    Ok((
        StatusCode::MOVED_PERMANENTLY,
        [(http::header::LOCATION, url.to_string())],
        Extension(CachePolicy::ForeverInCdnAndBrowser),
    ))
}

#[derive(Clone, Deserialize, Debug)]
pub(crate) struct JsonDownloadParams {
    pub(crate) name: String,
    pub(crate) version: ReqVersion,
    pub(crate) target: Option<String>,
    pub(crate) format_version: Option<String>,
}

#[instrument(skip_all)]
pub(crate) async fn json_download_handler(
    Path(params): Path<JsonDownloadParams>,
    mut conn: DbConnection,
    Extension(config): Extension<Arc<Config>>,
    Extension(storage): Extension<Arc<AsyncStorage>>,
    file_extension: Option<PathFileExtension>,
) -> AxumResult<impl IntoResponse> {
    // TODO: we could also additionally read the accept-encoding header here. But especially
    // in combination with priorities it's complex to parse correctly. So for now only
    // file extensions in the URL.
    let wanted_compression =
        if let Some(ext) = file_extension.map(|ext| ext.0) {
            Some(compression_from_file_extension(&ext).ok_or_else(|| {
                AxumNope::BadRequest(anyhow!("unknown compression file extension"))
            })?)
        } else {
            None
        };

    let matched_release = match_version(&mut conn, &params.name, &params.version)
        .await?
        .assume_exact_name()?;

    if !matched_release.rustdoc_status() {
        // without docs we'll never have JSON docs too
        return Err(AxumNope::ResourceNotFound);
    }

    let krate = CrateDetails::from_matched_release(&mut conn, matched_release).await?;

    let target = if let Some(wanted_target) = params.target {
        if krate
            .metadata
            .doc_targets
            .as_ref()
            .expect("we are checking rustdoc_status() above, so we always have metadata")
            .iter()
            .any(|s| s == &wanted_target)
        {
            wanted_target
        } else {
            return Err(AxumNope::TargetNotFound);
        }
    } else {
        krate
            .metadata
            .default_target
            .as_ref()
            .expect("we are checking rustdoc_status() above, so we always have metadata")
            .to_string()
    };

    let wanted_format_version = if let Some(request_format_version) = params.format_version {
        // axum doesn't support extension suffixes in the route yet, not as parameter, and not
        // statically, when combined with a parameter (like `.../{format_version}.gz`).
        // This is solved in matchit 0.8.6, but not yet in axum:
        // https://github.com/ibraheemdev/matchit/issues/17
        // https://github.com/tokio-rs/axum/pull/3143
        //
        // Because of this we have cases where `format_version` also contains a file extension
        // suffix like `.zstd`. `wanted_compression` is already extracted above, so we only
        // need to strip the extension from the `format_version` before trying to parse it.
        let stripped_format_version = if let Some(wanted_compression) = wanted_compression {
            request_format_version
                .strip_suffix(&format!(".{}", wanted_compression.file_extension()))
                .expect("should exist")
        } else {
            &request_format_version
        };

        stripped_format_version
            .parse::<RustdocJsonFormatVersion>()
            .context("can't parse format version")?
    } else {
        RustdocJsonFormatVersion::Latest
    };

    let wanted_compression = wanted_compression.unwrap_or_default();

    let storage_path = rustdoc_json_path(
        &krate.name,
        &krate.version.to_string(),
        &target,
        wanted_format_version,
        Some(wanted_compression),
    );

    let redirect = |storage_path: &str| {
        super::axum_cached_redirect(
            format!("{}/{}", config.s3_static_root_path, storage_path),
            CachePolicy::ForeverInCdn,
        )
    };

    if storage.exists(&storage_path).await? {
        Ok(redirect(&storage_path)?)
    } else {
        // we have old files on the bucket where we stored zstd compressed files,
        // with content-encoding=zstd & just a `.json` file extension.
        // As a fallback, we redirect to that, if zstd was requested (which is also the default).
        if wanted_compression == CompressionAlgorithm::Zstd {
            let storage_path = rustdoc_json_path(
                &krate.name,
                &krate.version.to_string(),
                &target,
                wanted_format_version,
                None,
            );

            if storage.exists(&storage_path).await? {
                // we have an old file with a `.json` extension,
                // redirect to that as fallback
                return Ok(redirect(&storage_path)?);
            }
        }

        Err(AxumNope::ResourceNotFound)
    }
}

#[instrument(skip_all)]
pub(crate) async fn download_handler(
    Path((name, req_version)): Path<(String, ReqVersion)>,
    mut conn: DbConnection,
    Extension(storage): Extension<Arc<AsyncStorage>>,
    Extension(config): Extension<Arc<Config>>,
) -> AxumResult<impl IntoResponse> {
    let version = match_version(&mut conn, &name, &req_version)
        .await?
        .assume_exact_name()?
        .into_version();

    let archive_path = rustdoc_archive_path(&name, &version.to_string());

    // not all archives are set for public access yet, so we check if
    // the access is set and fix it if needed.
    let archive_is_public = match storage
        .get_public_access(&archive_path)
        .await
        .context("reading public access for archive")
    {
        Ok(is_public) => is_public,
        Err(err) => {
            if matches!(err.downcast_ref(), Some(crate::storage::PathNotFoundError)) {
                return Err(AxumNope::ResourceNotFound);
            } else {
                return Err(AxumNope::InternalError(err));
            }
        }
    };

    if !archive_is_public {
        storage.set_public_access(&archive_path, true).await?;
    }

    Ok(super::axum_cached_redirect(
        format!("{}/{}", config.s3_static_root_path, archive_path),
        CachePolicy::ForeverInCdn,
    )?)
}

/// Serves shared resources used by rustdoc-generated documentation.
///
/// This serves files from S3, and is pointed to by the `--static-root-path` flag to rustdoc.
#[instrument(skip_all)]
pub(crate) async fn static_asset_handler(
    Path(path): Path<String>,
    Extension(storage): Extension<Arc<AsyncStorage>>,
) -> AxumResult<impl IntoResponse> {
    let storage_path = format!("{RUSTDOC_STATIC_STORAGE_PREFIX}{path}");

    Ok(StreamingFile::from_path(&storage, &storage_path).await?)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Config,
        docbuilder::RUSTDOC_JSON_COMPRESSION_ALGORITHMS,
        registry_api::{CrateOwner, OwnerKind},
        storage::compression::file_extension_for,
        test::*,
        utils::Dependency,
        web::{cache::CachePolicy, encode_url_path},
    };
    use anyhow::Context;
    use chrono::{NaiveDate, Utc};
    use kuchikiki::traits::TendrilSink;
    use reqwest::StatusCode;
    use std::collections::BTreeMap;
    use test_case::test_case;
    use tracing::info;

    async fn try_latest_version_redirect(
        path: &str,
        web: &axum::Router,
        config: &Config,
    ) -> Result<Option<String>, anyhow::Error> {
        web.assert_success(path).await?;
        let response = web.get(path).await?;
        response.assert_cache_control(CachePolicy::ForeverInCdnAndStaleInBrowser, config);
        let data = response.text().await?;
        info!(
            "fetched path {} and got content {}\nhelp: if this is missing the header, remember to add <html><head></head><body></body></html>",
            path, data
        );
        let dom = kuchikiki::parse_html().one(data);

        if let Some(elem) = dom
            .select("form > ul > li > a.warn")
            .expect("invalid selector")
            .next()
        {
            let link = elem.attributes.borrow().get("href").unwrap().to_string();
            let response = web.get(&link).await?;
            response.assert_cache_control(CachePolicy::ForeverInCdn, config);
            assert!(response.status().is_success() || response.status().is_redirection());
            Ok(Some(link))
        } else {
            Ok(None)
        }
    }

    async fn latest_version_redirect(
        path: &str,
        web: &axum::Router,
        config: &Config,
    ) -> Result<String, anyhow::Error> {
        try_latest_version_redirect(path, web, config)
            .await?
            .with_context(|| anyhow::anyhow!("no redirect found for {}", path))
    }

    #[test_case(true)]
    #[test_case(false)]
    // https://github.com/rust-lang/docs.rs/issues/2313
    fn help_html(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("krate")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("help.html")
                .create()
                .await?;
            let web = env.web_app().await;
            web.assert_success_cached(
                "/krate/0.1.0/help.html",
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            Ok(())
        });
    }

    #[test_case(true)]
    #[test_case(false)]
    // regression test for https://github.com/rust-lang/docs.rs/issues/552
    fn settings_html(archive_storage: bool) {
        async_wrapper(|env| async move {
            // first release works, second fails
            env.fake_release()
                .await
                .name("buggy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("settings.html")
                .rustdoc_file("scrape-examples-help.html")
                .rustdoc_file("directory_1/index.html")
                .rustdoc_file("directory_2.html/index.html")
                .rustdoc_file("all.html")
                .rustdoc_file("directory_3/.gitignore")
                .rustdoc_file("directory_4/empty_file_no_ext")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("buggy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .build_result_failed()
                .create()
                .await?;
            let web = env.web_app().await;
            web.assert_success_cached("/", CachePolicy::ShortInCdnAndBrowser, &env.config())
                .await?;
            web.assert_success_cached(
                "/crate/buggy/0.1.0",
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            web.assert_success_cached(
                "/buggy/0.1.0/directory_1/index.html",
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            web.assert_success_cached(
                "/buggy/0.1.0/directory_2.html/index.html",
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            web.assert_success_cached(
                "/buggy/0.1.0/directory_3/.gitignore",
                CachePolicy::ForeverInCdnAndBrowser,
                &env.config(),
            )
            .await?;
            web.assert_success_cached(
                "/buggy/0.1.0/settings.html",
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            web.assert_success_cached(
                "/buggy/0.1.0/scrape-examples-help.html",
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            web.assert_success_cached(
                "/buggy/0.1.0/all.html",
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            web.assert_success_cached(
                "/buggy/0.1.0/directory_4/empty_file_no_ext",
                CachePolicy::ForeverInCdnAndBrowser,
                &env.config(),
            )
            .await?;
            Ok(())
        });
    }

    #[test_case(true)]
    #[test_case(false)]
    fn default_target_redirects_to_base(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .create()
                .await?;

            let web = env.web_app().await;
            // no explicit default-target
            let base = "/dummy/0.1.0/dummy/";
            web.assert_success_cached(
                base,
                CachePolicy::ForeverInCdnAndStaleInBrowser,
                &env.config(),
            )
            .await?;
            web.assert_redirect_cached(
                "/dummy/0.1.0/x86_64-unknown-linux-gnu/dummy/",
                base,
                CachePolicy::ForeverInCdn,
                &env.config(),
            )
            .await?;

            web.assert_success("/dummy/latest/dummy/").await?;

            // set an explicit target that requires cross-compile
            let target = "x86_64-pc-windows-msvc";
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .default_target(target)
                .create()
                .await?;
            let base = "/dummy/0.2.0/dummy/";
            web.assert_success(base).await?;
            web.assert_redirect("/dummy/0.2.0/x86_64-pc-windows-msvc/dummy/", base)
                .await?;

            // set an explicit target without cross-compile
            // also check that /:crate/:version/:platform/all.html doesn't panic
            let target = "x86_64-unknown-linux-gnu";
            env.fake_release()
                .await
                .name("dummy")
                .version("0.3.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .rustdoc_file("all.html")
                .default_target(target)
                .create()
                .await?;
            let base = "/dummy/0.3.0/dummy/";
            web.assert_success(base).await?;
            web.assert_redirect("/dummy/0.3.0/x86_64-unknown-linux-gnu/dummy/", base)
                .await?;
            web.assert_redirect(
                "/dummy/0.3.0/x86_64-unknown-linux-gnu/all.html",
                "/dummy/0.3.0/all.html",
            )
            .await?;
            web.assert_redirect("/dummy/0.3.0/", base).await?;
            web.assert_redirect("/dummy/0.3.0/index.html", base).await?;
            Ok(())
        });
    }

    #[test]
    fn latest_url() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .rustdoc_file("dummy/index.html")
                .create()
                .await?;

            let resp = env
                .web_app()
                .await
                .get("/dummy/latest/dummy/")
                .await?
                .error_for_status()?;
            resp.assert_cache_control(CachePolicy::ForeverInCdn, &env.config());
            let body = resp.text().await?;
            assert!(body.contains("<a href=\"/crate/dummy/latest/source/\""));
            assert!(body.contains("<a href=\"/crate/dummy/latest\""));
            assert!(body.contains("<a href=\"/dummy/0.1.0/dummy/index.html\""));
            Ok(())
        })
    }

    #[test]
    fn cache_headers_on_version() {
        async_wrapper(|env| async move {
            env.override_config(|config| {
                config.cache_control_stale_while_revalidate = Some(2592000);
            });

            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .rustdoc_file("dummy/index.html")
                .create()
                .await?;

            let web = env.web_app().await;

            {
                let resp = web.get("/dummy/latest/dummy/").await?;
                resp.assert_cache_control(CachePolicy::ForeverInCdn, &env.config());
            }

            {
                let resp = web.get("/dummy/0.1.0/dummy/").await?;
                resp.assert_cache_control(
                    CachePolicy::ForeverInCdnAndStaleInBrowser,
                    &env.config(),
                );
            }
            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn go_to_latest_version(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/blah/index.html")
                .rustdoc_file("dummy/blah/blah.html")
                .rustdoc_file("dummy/struct.will-be-deleted.html")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/blah/index.html")
                .rustdoc_file("dummy/blah/blah.html")
                .create()
                .await?;

            let web = env.web_app().await;

            // check it works at all
            let redirect =
                latest_version_redirect("/dummy/0.1.0/dummy/", &web, &env.config()).await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-unknown-linux-gnu/dummy/index.html"
            );

            // check it keeps the subpage
            let redirect =
                latest_version_redirect("/dummy/0.1.0/dummy/blah/", &web, &env.config()).await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-unknown-linux-gnu/dummy/blah/index.html"
            );
            let redirect =
                latest_version_redirect("/dummy/0.1.0/dummy/blah/blah.html", &web, &env.config())
                    .await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-unknown-linux-gnu/dummy/blah/blah.html"
            );

            // check it also works for deleted pages
            let redirect = latest_version_redirect(
                "/dummy/0.1.0/dummy/struct.will-be-deleted.html",
                &web,
                &env.config(),
            )
            .await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-unknown-linux-gnu/dummy/struct.will-be-deleted.html"
            );

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn go_to_latest_version_keeps_platform(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .add_platform("x86_64-pc-windows-msvc")
                .rustdoc_file("dummy/struct.Blah.html")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .add_platform("x86_64-pc-windows-msvc")
                .create()
                .await?;

            let web = env.web_app().await;

            let redirect = latest_version_redirect(
                "/dummy/0.1.0/x86_64-pc-windows-msvc/dummy/index.html",
                &web,
                &env.config(),
            )
            .await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-pc-windows-msvc/dummy/index.html"
            );

            let redirect = latest_version_redirect(
                "/dummy/0.1.0/x86_64-pc-windows-msvc/dummy/",
                &web,
                &env.config(),
            )
            .await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-pc-windows-msvc/dummy/index.html"
            );

            let redirect = latest_version_redirect(
                "/dummy/0.1.0/x86_64-pc-windows-msvc/dummy/struct.Blah.html",
                &web,
                &env.config(),
            )
            .await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-pc-windows-msvc/dummy/struct.Blah.html"
            );

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn redirect_latest_goes_to_crate_if_build_failed(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .build_result_failed()
                .create()
                .await?;

            let web = env.web_app().await;
            let redirect =
                latest_version_redirect("/dummy/0.1.0/dummy/", &web, &env.config()).await?;
            assert_eq!(redirect, "/crate/dummy/latest");

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn redirect_latest_does_not_go_to_yanked_versions(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.1")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .yanked(true)
                .create()
                .await?;

            let web = env.web_app().await;
            let redirect =
                latest_version_redirect("/dummy/0.1.0/dummy/", &web, &env.config()).await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-unknown-linux-gnu/dummy/index.html"
            );

            let redirect =
                latest_version_redirect("/dummy/0.2.1/dummy/", &web, &env.config()).await?;
            assert_eq!(
                redirect,
                "/crate/dummy/latest/target-redirect/x86_64-unknown-linux-gnu/dummy/index.html"
            );

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn yanked_release_shows_warning_in_nav(archive_storage: bool) {
        async fn has_yanked_warning(path: &str, web: &axum::Router) -> Result<bool, anyhow::Error> {
            web.assert_success(path).await?;
            let data = web.get(path).await?.text().await?;
            Ok(kuchikiki::parse_html()
                .one(data)
                .select("form > ul > li > .warn")
                .expect("invalid selector")
                .any(|el| el.text_contents().contains("yanked")))
        }

        async_wrapper(|env| async move {
            let web = env.web_app().await;

            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .yanked(true)
                .create()
                .await?;

            assert!(has_yanked_warning("/dummy/0.1.0/dummy/", &web).await?);

            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .yanked(true)
                .create()
                .await?;

            assert!(has_yanked_warning("/dummy/0.1.0/dummy/", &web).await?);

            Ok(())
        })
    }

    #[test]
    fn badges_are_urlencoded() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("zstd")
                .version("0.5.1+zstd.1.4.4")
                .create()
                .await?;

            let frontend = env.web_app().await;
            let response = frontend
                .assert_redirect_cached_unchecked(
                    "/zstd/badge.svg",
                    "https://img.shields.io/docsrs/zstd/latest",
                    CachePolicy::ForeverInCdnAndBrowser,
                    &env.config(),
                )
                .await?;
            assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn crate_name_percent_decoded_redirect(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("fake-crate")
                .version("0.0.1")
                .archive_storage(archive_storage)
                .rustdoc_file("fake_crate/index.html")
                .create()
                .await?;

            let web = env.web_app().await;
            web.assert_redirect("/fake%2Dcrate", "/fake-crate/latest/fake_crate/")
                .await?;

            Ok(())
        });
    }

    #[test_case(true)]
    #[test_case(false)]
    fn base_redirect_handles_mismatched_separators(archive_storage: bool) {
        async_wrapper(|env| async move {
            let rels = [
                ("dummy-dash", "0.1.0"),
                ("dummy-dash", "0.2.0"),
                ("dummy_underscore", "0.1.0"),
                ("dummy_underscore", "0.2.0"),
                ("dummy_mixed-separators", "0.1.0"),
                ("dummy_mixed-separators", "0.2.0"),
            ];

            for (name, version) in &rels {
                env.fake_release()
                    .await
                    .name(name)
                    .version(version)
                    .archive_storage(archive_storage)
                    .rustdoc_file(&(name.replace('-', "_") + "/index.html"))
                    .create()
                    .await?;
            }

            let web = env.web_app().await;

            web.assert_redirect("/dummy_dash", "/dummy-dash/latest/dummy_dash/")
                .await?;
            web.assert_redirect("/dummy_dash/*", "/dummy-dash/latest/dummy_dash/")
                .await?;
            web.assert_redirect("/dummy_dash/0.1.0", "/dummy-dash/0.1.0/dummy_dash/")
                .await?;
            web.assert_redirect(
                "/dummy-underscore",
                "/dummy_underscore/latest/dummy_underscore/",
            )
            .await?;
            web.assert_redirect(
                "/dummy-underscore/*",
                "/dummy_underscore/latest/dummy_underscore/",
            )
            .await?;
            web.assert_redirect(
                "/dummy-underscore/0.1.0",
                "/dummy_underscore/0.1.0/dummy_underscore/",
            )
            .await?;
            web.assert_redirect(
                "/dummy-mixed_separators",
                "/dummy_mixed-separators/latest/dummy_mixed_separators/",
            )
            .await?;
            web.assert_redirect(
                "/dummy_mixed_separators/*",
                "/dummy_mixed-separators/latest/dummy_mixed_separators/",
            )
            .await?;
            web.assert_redirect(
                "/dummy-mixed-separators/0.1.0",
                "/dummy_mixed-separators/0.1.0/dummy_mixed_separators/",
            )
            .await?;

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn specific_pages_do_not_handle_mismatched_separators(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy-dash")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy_dash/index.html")
                .create()
                .await?;

            env.fake_release()
                .await
                .name("dummy_mixed-separators")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy_mixed_separators/index.html")
                .create()
                .await?;

            let web = env.web_app().await;

            web.assert_success("/dummy-dash/0.1.0/dummy_dash/index.html")
                .await?;
            web.assert_redirect_unchecked(
                "/crate/dummy_mixed-separators",
                "/crate/dummy_mixed-separators/latest",
            )
            .await?;

            web.assert_redirect(
                "/dummy_dash/0.1.0/dummy_dash/index.html",
                "/dummy-dash/0.1.0/dummy_dash/index.html",
            )
            .await?;

            assert_eq!(
                dbg!(web.get("/crate/dummy_mixed_separators/latest").await?).status(),
                StatusCode::NOT_FOUND
            );

            Ok(())
        })
    }

    #[test]
    fn nonexistent_crate_404s() {
        async_wrapper(|env| async move {
            assert_eq!(
                env.web_app().await.get("/dummy").await?.status(),
                StatusCode::NOT_FOUND
            );

            Ok(())
        })
    }

    #[test]
    fn no_target_target_redirect_404s() {
        async_wrapper(|env| async move {
            assert_eq!(
                env.web_app()
                    .await
                    .get("/crate/dummy/0.1.0/target-redirect")
                    .await?
                    .status(),
                StatusCode::NOT_FOUND
            );

            assert_eq!(
                env.web_app()
                    .await
                    .get("/crate/dummy/0.1.0/target-redirect/")
                    .await?
                    .status(),
                StatusCode::NOT_FOUND
            );

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn platform_links_go_to_current_path(archive_storage: bool) {
        async fn get_platform_links(
            path: &str,
            web: &axum::Router,
        ) -> Result<Vec<(String, String, String)>, anyhow::Error> {
            web.assert_success(path).await?;
            let data = web.get(path).await?.text().await?;
            let dom = kuchikiki::parse_html().one(data);
            Ok(dom
                .select(r#"a[aria-label="Platform"] + ul li a"#)
                .expect("invalid selector")
                .map(|el| {
                    let attributes = el.attributes.borrow();
                    let url = attributes.get("href").expect("href").to_string();
                    let rel = attributes.get("rel").unwrap_or("").to_string();
                    let name = el.text_contents();
                    (name, url, rel)
                })
                .collect())
        }
        async fn assert_platform_links(
            web: &axum::Router,
            path: &str,
            links: &[(&str, &str)],
        ) -> Result<(), anyhow::Error> {
            let mut links: BTreeMap<_, _> = links.iter().copied().collect();

            for (platform, link, rel) in get_platform_links(path, web).await? {
                assert_eq!(rel, "nofollow");
                web.assert_redirect(&link, links.remove(platform.as_str()).unwrap())
                    .await?;
            }

            assert!(links.is_empty());

            Ok(())
        }

        async_wrapper(|env| async move {
            let web = env.web_app().await;

            // no explicit default-target
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .rustdoc_file("dummy/struct.Dummy.html")
                .add_target("x86_64-unknown-linux-gnu")
                .create()
                .await?;

            assert_platform_links(
                &web,
                "/dummy/0.1.0/dummy/",
                &[("x86_64-unknown-linux-gnu", "/dummy/0.1.0/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.1.0/dummy/index.html",
                &[("x86_64-unknown-linux-gnu", "/dummy/0.1.0/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.1.0/dummy/struct.Dummy.html",
                &[(
                    "x86_64-unknown-linux-gnu",
                    "/dummy/0.1.0/dummy/struct.Dummy.html",
                )],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/",
                &[("x86_64-unknown-linux-gnu", "/dummy/latest/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/index.html",
                &[("x86_64-unknown-linux-gnu", "/dummy/latest/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/struct.Dummy.html",
                &[(
                    "x86_64-unknown-linux-gnu",
                    "/dummy/latest/dummy/struct.Dummy.html",
                )],
            )
            .await?;

            // set an explicit target that requires cross-compile
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .rustdoc_file("dummy/struct.Dummy.html")
                .default_target("x86_64-pc-windows-msvc")
                .create()
                .await?;

            assert_platform_links(
                &web,
                "/dummy/0.2.0/dummy/",
                &[("x86_64-pc-windows-msvc", "/dummy/0.2.0/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.2.0/dummy/index.html",
                &[("x86_64-pc-windows-msvc", "/dummy/0.2.0/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.2.0/dummy/struct.Dummy.html",
                &[(
                    "x86_64-pc-windows-msvc",
                    "/dummy/0.2.0/dummy/struct.Dummy.html",
                )],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/",
                &[("x86_64-pc-windows-msvc", "/dummy/latest/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/index.html",
                &[("x86_64-pc-windows-msvc", "/dummy/latest/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/struct.Dummy.html",
                &[(
                    "x86_64-pc-windows-msvc",
                    "/dummy/latest/dummy/struct.Dummy.html",
                )],
            )
            .await?;

            // set an explicit target without cross-compile
            env.fake_release()
                .await
                .name("dummy")
                .version("0.3.0")
                .archive_storage(archive_storage)
                .rustdoc_file("dummy/index.html")
                .rustdoc_file("dummy/struct.Dummy.html")
                .default_target("x86_64-unknown-linux-gnu")
                .create()
                .await?;

            assert_platform_links(
                &web,
                "/dummy/0.3.0/dummy/",
                &[("x86_64-unknown-linux-gnu", "/dummy/0.3.0/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.3.0/dummy/index.html",
                &[("x86_64-unknown-linux-gnu", "/dummy/0.3.0/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.3.0/dummy/struct.Dummy.html",
                &[(
                    "x86_64-unknown-linux-gnu",
                    "/dummy/0.3.0/dummy/struct.Dummy.html",
                )],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/",
                &[("x86_64-unknown-linux-gnu", "/dummy/latest/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/index.html",
                &[("x86_64-unknown-linux-gnu", "/dummy/latest/dummy/index.html")],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/dummy/struct.Dummy.html",
                &[(
                    "x86_64-unknown-linux-gnu",
                    "/dummy/latest/dummy/struct.Dummy.html",
                )],
            )
            .await?;

            // multiple targets
            env.fake_release()
                .await
                .name("dummy")
                .version("0.4.0")
                .archive_storage(archive_storage)
                .rustdoc_file("settings.html")
                .rustdoc_file("dummy/index.html")
                .rustdoc_file("dummy/struct.Dummy.html")
                .rustdoc_file("dummy/struct.DefaultOnly.html")
                .rustdoc_file("x86_64-pc-windows-msvc/settings.html")
                .rustdoc_file("x86_64-pc-windows-msvc/dummy/index.html")
                .rustdoc_file("x86_64-pc-windows-msvc/dummy/struct.Dummy.html")
                .rustdoc_file("x86_64-pc-windows-msvc/dummy/struct.WindowsOnly.html")
                .default_target("x86_64-unknown-linux-gnu")
                .add_target("x86_64-pc-windows-msvc")
                .create()
                .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/settings.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/settings.html",
                    ),
                    ("x86_64-unknown-linux-gnu", "/dummy/0.4.0/settings.html"),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/latest/settings.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/latest/x86_64-pc-windows-msvc/settings.html",
                    ),
                    ("x86_64-unknown-linux-gnu", "/dummy/latest/settings.html"),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/dummy/",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/index.html",
                    ),
                    ("x86_64-unknown-linux-gnu", "/dummy/0.4.0/dummy/index.html"),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/index.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/index.html",
                    ),
                    ("x86_64-unknown-linux-gnu", "/dummy/0.4.0/dummy/index.html"),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/dummy/index.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/index.html",
                    ),
                    ("x86_64-unknown-linux-gnu", "/dummy/0.4.0/dummy/index.html"),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/dummy/struct.DefaultOnly.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/?search=DefaultOnly",
                    ),
                    (
                        "x86_64-unknown-linux-gnu",
                        "/dummy/0.4.0/dummy/struct.DefaultOnly.html",
                    ),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/dummy/struct.Dummy.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/struct.Dummy.html",
                    ),
                    (
                        "x86_64-unknown-linux-gnu",
                        "/dummy/0.4.0/dummy/struct.Dummy.html",
                    ),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/struct.Dummy.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/struct.Dummy.html",
                    ),
                    (
                        "x86_64-unknown-linux-gnu",
                        "/dummy/0.4.0/dummy/struct.Dummy.html",
                    ),
                ],
            )
            .await?;

            assert_platform_links(
                &web,
                "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/struct.WindowsOnly.html",
                &[
                    (
                        "x86_64-pc-windows-msvc",
                        "/dummy/0.4.0/x86_64-pc-windows-msvc/dummy/struct.WindowsOnly.html",
                    ),
                    (
                        "x86_64-unknown-linux-gnu",
                        "/dummy/0.4.0/dummy/?search=WindowsOnly",
                    ),
                ],
            )
            .await?;

            Ok(())
        });
    }

    #[test]
    fn test_target_redirect_with_corrected_name() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("foo_ab")
                .version("0.0.1")
                .archive_storage(true)
                .create()
                .await?;

            let web = env.web_app().await;
            web.assert_redirect_unchecked(
                "/crate/foo-ab/0.0.1/target-redirect/x86_64-unknown-linux-gnu",
                "/foo-ab/0.0.1/foo_ab/",
            )
            .await?;
            // `-` becomes `_` but we keep the query arguments.
            web.assert_redirect_unchecked(
                "/foo-ab/0.0.1/foo_ab/?search=a",
                "/foo_ab/0.0.1/foo_ab/?search=a",
            )
            .await?;
            Ok(())
        })
    }

    #[test]
    fn test_target_redirect_not_found() {
        async_wrapper(|env| async move {
            let web = env.web_app().await;
            assert_eq!(
                web.get("/crate/fdsafdsafdsafdsa/0.1.0/target-redirect/x86_64-apple-darwin/")
                    .await?
                    .status(),
                StatusCode::NOT_FOUND,
            );
            Ok(())
        })
    }

    #[test]
    fn test_redirect_to_latest_302() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("1.0.0")
                .create()
                .await?;
            let web = env.web_app().await;
            let resp = web
                .assert_redirect("/dummy", "/dummy/latest/dummy/")
                .await?;
            assert_eq!(resp.status(), StatusCode::FOUND);
            assert!(resp.headers().get("Cache-Control").is_none());
            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_fully_yanked_crate_404s(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("1.0.0")
                .archive_storage(archive_storage)
                .yanked(true)
                .create()
                .await?;

            assert_eq!(
                env.web_app()
                    .await
                    .get("/crate/dummy/latest")
                    .await?
                    .status(),
                StatusCode::NOT_FOUND
            );

            assert_eq!(
                env.web_app().await.get("/dummy/").await?.status(),
                StatusCode::NOT_FOUND
            );

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_no_trailing_target_slash(archive_storage: bool) {
        // regression test for https://github.com/rust-lang/docs.rs/issues/856
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(archive_storage)
                .create()
                .await?;
            let web = env.web_app().await;
            web.assert_redirect(
                "/crate/dummy/0.1.0/target-redirect/x86_64-apple-darwin",
                "/dummy/0.1.0/dummy/",
            )
            .await?;
            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(archive_storage)
                .add_platform("x86_64-apple-darwin")
                .create()
                .await?;
            web.assert_redirect(
                "/crate/dummy/0.2.0/target-redirect/x86_64-apple-darwin",
                "/dummy/0.2.0/x86_64-apple-darwin/dummy/",
            )
            .await?;
            web.assert_redirect(
                "/crate/dummy/0.2.0/target-redirect/platform-that-does-not-exist",
                "/dummy/0.2.0/dummy/",
            )
            .await?;
            Ok(())
        })
    }

    #[test]
    fn test_redirect_crate_coloncolon_path() {
        async_wrapper(|env| async move {
            let web = env.web_app().await;
            env.fake_release()
                .await
                .name("some_random_crate")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("some_other_crate")
                .create()
                .await?;

            web.assert_redirect(
                "/some_random_crate::somepath",
                "/some_random_crate/latest/some_random_crate/?search=somepath",
            )
            .await?;
            web.assert_redirect(
                "/some_random_crate::some::path",
                "/some_random_crate/latest/some_random_crate/?search=some%3A%3Apath",
            )
            .await?;
            web.assert_redirect(
                "/some_random_crate::some::path?go_to_first=true",
                "/some_random_crate/latest/some_random_crate/?go_to_first=true&search=some%3A%3Apath",
            ).await?;

            web.assert_redirect_unchecked(
                "/std::some::path",
                "https://doc.rust-lang.org/stable/std/?search=some%3A%3Apath",
            )
            .await?;

            Ok(())
        })
    }

    #[test]
    // regression test for https://github.com/rust-lang/docs.rs/pull/885#issuecomment-655147643
    fn test_no_panic_on_missing_kind() {
        async_wrapper(|env| async move {
            let id = env
                .fake_release()
                .await
                .name("strum")
                .version("0.13.0")
                .create()
                .await?;

            let mut conn = env.async_db().await.async_conn().await;
            // https://stackoverflow.com/questions/18209625/how-do-i-modify-fields-inside-the-new-postgresql-json-datatype
            sqlx::query!(
                    r#"UPDATE releases SET dependencies = dependencies::jsonb #- '{0,2}' WHERE id = $1"#, id.0
            ).execute(&mut *conn).await?;

            let web = env.web_app().await;
            web.assert_success("/strum/0.13.0/strum/").await?;
            web.assert_success("/crate/strum/0.13.0").await?;
            Ok(())
        })
    }

    #[test]
    // regression test for https://github.com/rust-lang/docs.rs/pull/885#issuecomment-655154405
    fn test_readme_rendered_as_html() {
        async_wrapper(|env| async move {
            let readme = "# Overview";
            env.fake_release()
                .await
                .name("strum")
                .version("0.18.0")
                .readme(readme)
                .create()
                .await?;
            let page = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/crate/strum/0.18.0")
                    .await?
                    .text()
                    .await?,
            );
            let rendered = page.select_first("#main").expect("missing readme");
            println!("{}", rendered.text_contents());
            rendered
                .as_node()
                .select_first("h1")
                .expect("`# Overview` was not rendered as HTML");
            Ok(())
        })
    }

    #[test]
    // regression test for https://github.com/rust-lang/docs.rs/pull/885#issuecomment-655149288
    fn test_build_status_is_accurate() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("hexponent")
                .version("0.3.0")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("hexponent")
                .version("0.2.0")
                .build_result_failed()
                .create()
                .await?;
            let web = env.web_app().await;

            let status = |version| {
                let web = web.clone();
                async move {
                    let page = kuchikiki::parse_html()
                        .one(web.get("/crate/hexponent/0.3.0").await?.text().await?);
                    let selector = format!(r#"ul > li a[href="/crate/hexponent/{version}"]"#);
                    let anchor = page
                        .select(&selector)
                        .unwrap()
                        .find(|a| a.text_contents().trim().split(" ").next().unwrap() == version)
                        .unwrap();
                    let attributes = anchor.as_node().as_element().unwrap().attributes.borrow();
                    let classes = attributes.get("class").unwrap();
                    Ok::<_, anyhow::Error>(classes.split(' ').all(|c| c != "warn"))
                }
            };

            assert!(status("0.3.0").await?);
            assert!(!status("0.2.0").await?);
            Ok(())
        })
    }

    #[test]
    fn test_crate_release_version_and_date() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("hexponent")
                .version("0.3.0")
                .release_time(
                    NaiveDate::from_ymd_opt(2021, 1, 12)
                        .unwrap()
                        .and_hms_milli_opt(0, 0, 0, 0)
                        .unwrap()
                        .and_local_timezone(Utc)
                        .unwrap(),
                )
                .create()
                .await?;
            env.fake_release()
                .await
                .name("hexponent")
                .version("0.2.0")
                .release_time(
                    NaiveDate::from_ymd_opt(2020, 12, 1)
                        .unwrap()
                        .and_hms_milli_opt(0, 0, 0, 0)
                        .unwrap()
                        .and_local_timezone(Utc)
                        .unwrap(),
                )
                .create()
                .await?;
            let web = env.web_app().await;

            let status = |version, date| {
                let web = web.clone();
                async move {
                    let page = kuchikiki::parse_html()
                        .one(web.get("/crate/hexponent/0.3.0").await?.text().await?);
                    let selector = format!(r#"ul > li a[href="/crate/hexponent/{version}"]"#);
                    let full = format!("{version} ({date})");
                    Result::<bool, anyhow::Error>::Ok(page.select(&selector).unwrap().any(|a| {
                        eprintln!("++++++> {:?}", a.text_contents());
                        a.text_contents().trim() == full
                    }))
                }
            };

            assert!(status("0.3.0", "2021/01/12").await?);
            assert!(status("0.2.0", "2020/12/01").await?);
            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_no_trailing_rustdoc_slash(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("tokio")
                .version("0.2.21")
                .archive_storage(archive_storage)
                .rustdoc_file("tokio/time/index.html")
                .create()
                .await?;

            env.web_app()
                .await
                .assert_redirect(
                    "/tokio/0.2.21/tokio/time",
                    "/tokio/0.2.21/tokio/time/index.html",
                )
                .await?;

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_non_ascii(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("const_unit_poc")
                .version("1.0.0")
                .archive_storage(archive_storage)
                .rustdoc_file("const_unit_poc/units/constant.Ω.html")
                .create()
                .await?;
            env.web_app()
                .await
                .assert_success(&encode_url_path(
                    "/const_unit_poc/1.0.0/const_unit_poc/units/constant.Ω.html",
                ))
                .await?;
            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_latest_version_keeps_query(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("tungstenite")
                .version("0.10.0")
                .archive_storage(archive_storage)
                .rustdoc_file("tungstenite/index.html")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("tungstenite")
                .version("0.11.0")
                .archive_storage(archive_storage)
                .rustdoc_file("tungstenite/index.html")
                .create()
                .await?;
            assert_eq!(
                latest_version_redirect(
                    "/tungstenite/0.10.0/tungstenite/?search=String%20-%3E%20Message",
                    &env.web_app().await,
                    &env.config()
                )
                .await?,
                "/crate/tungstenite/latest/target-redirect/x86_64-unknown-linux-gnu/tungstenite/index.html?search=String%20-%3E%20Message",
            );
            Ok(())
        });
    }

    #[test_case(true)]
    #[test_case(false)]
    fn latest_version_works_when_source_deleted(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("pyo3")
                .version("0.2.7")
                .archive_storage(archive_storage)
                .source_file("src/objects/exc.rs", b"//! some docs")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("pyo3")
                .version("0.13.2")
                .create()
                .await?;
            let target_redirect = "/crate/pyo3/latest/target-redirect/x86_64-unknown-linux-gnu/src/pyo3/objects/exc.rs.html";
            let web = env.web_app().await;
            assert_eq!(
                latest_version_redirect(
                    "/pyo3/0.2.7/src/pyo3/objects/exc.rs.html",
                    &web,
                    &env.config(),
                )
                .await?,
                target_redirect
            );

            web.assert_redirect(target_redirect, "/pyo3/latest/pyo3/?search=exc")
                .await?;
            Ok(())
        })
    }

    fn parse_release_links_from_menu(body: &str) -> Vec<String> {
        kuchikiki::parse_html()
            .one(body)
            .select(r#"ul > li > a"#)
            .expect("invalid selector")
            .map(|elem| elem.attributes.borrow().get("href").unwrap().to_string())
            .collect()
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_version_link_goes_to_docs(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("hexponent")
                .version("0.3.0")
                .archive_storage(archive_storage)
                .rustdoc_file("hexponent/index.html")
                .create()
                .await?;
            env.fake_release()
                .await
                .name("hexponent")
                .version("0.3.1")
                .archive_storage(archive_storage)
                .rustdoc_file("hexponent/index.html")
                .rustdoc_file("hexponent/something.html")
                .create()
                .await?;

            // test rustdoc pages stay on the documentation
            let releases_response = env
                .web_app()
                .await
                .get("/crate/hexponent/0.3.1/menus/releases")
                .await?;
            assert!(releases_response.status().is_success());
            releases_response.assert_cache_control(CachePolicy::ForeverInCdn, &env.config());
            assert_eq!(
                parse_release_links_from_menu(&releases_response.text().await?),
                vec![
                    "/crate/hexponent/0.3.1/target-redirect/hexponent/index.html".to_owned(),
                    "/crate/hexponent/0.3.0/target-redirect/hexponent/index.html".to_owned(),
                ]
            );

            // test if target-redirect includes path
            let releases_response = env
                .web_app()
                .await
                .get("/crate/hexponent/0.3.1/menus/releases/hexponent/something.html")
                .await?;
            assert!(releases_response.status().is_success());
            releases_response.assert_cache_control(CachePolicy::ForeverInCdn, &env.config());
            assert_eq!(
                parse_release_links_from_menu(&releases_response.text().await?),
                vec![
                    "/crate/hexponent/0.3.1/target-redirect/hexponent/something.html".to_owned(),
                    "/crate/hexponent/0.3.0/target-redirect/hexponent/something.html".to_owned(),
                ]
            );

            // test /crate pages stay on /crate
            let page = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/crate/hexponent/0.3.0")
                    .await?
                    .text()
                    .await?,
            );
            let selector = r#"ul > li a[href="/crate/hexponent/0.3.1"]"#.to_string();
            assert_eq!(
                page.select(&selector).unwrap().count(),
                1,
                "link to /crate not found"
            );

            Ok(())
        })
    }

    #[test]
    fn test_repository_link_in_topbar_dropdown() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("testing")
                .repo("https://git.example.com")
                .version("0.1.0")
                .rustdoc_file("testing/index.html")
                .create()
                .await?;

            let dom = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/testing/0.1.0/testing/")
                    .await?
                    .text()
                    .await?,
            );

            assert_eq!(
                dom.select(r#"ul > li a[href="https://git.example.com"]"#)
                    .unwrap()
                    .count(),
                1,
            );

            Ok(())
        })
    }

    #[test]
    fn test_repository_link_in_topbar_dropdown_github() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("testing")
                .version("0.1.0")
                .rustdoc_file("testing/index.html")
                .github_stats("https://git.example.com", 123, 321, 333)
                .create()
                .await?;

            let dom = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/testing/0.1.0/testing/")
                    .await?
                    .text()
                    .await?,
            );

            assert_eq!(
                dom.select(r#"ul > li a[href="https://git.example.com"]"#)
                    .unwrap()
                    .count(),
                1,
            );

            Ok(())
        })
    }

    #[test]
    fn test_owner_links_with_team() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("testing")
                .version("0.1.0")
                .add_owner(CrateOwner {
                    login: "some-user".into(),
                    kind: OwnerKind::User,
                    avatar: "".into(),
                })
                .add_owner(CrateOwner {
                    login: "some-team".into(),
                    kind: OwnerKind::Team,
                    avatar: "".into(),
                })
                .create()
                .await?;

            let dom = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/testing/0.1.0/testing/")
                    .await?
                    .text()
                    .await?,
            );

            let owner_links: Vec<_> = dom
                .select(r#"#topbar-owners > li > a"#)
                .expect("invalid selector")
                .map(|el| {
                    let attributes = el.attributes.borrow();
                    let url = attributes.get("href").expect("href").trim().to_string();
                    let name = el.text_contents().trim().to_string();
                    (name, url)
                })
                .collect();

            assert_eq!(
                owner_links,
                vec![
                    (
                        "some-user".into(),
                        "https://crates.io/users/some-user".into()
                    ),
                    (
                        "some-team".into(),
                        "https://crates.io/teams/some-team".into()
                    ),
                ]
            );

            Ok(())
        })
    }

    #[test]
    fn test_dependency_optional_suffix() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("testing")
                .version("0.1.0")
                .rustdoc_file("testing/index.html")
                .add_dependency(
                    Dependency::new("optional-dep".to_string(), "1.2.3".to_string())
                        .set_optional(true),
                )
                .create()
                .await?;

            let dom = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/testing/0.1.0/testing/")
                    .await?
                    .text()
                    .await?,
            );
            assert!(
                dom.select(r#"a[href="/optional-dep/1.2.3"] > i[class="dependencies normal"] + i"#)
                    .expect("should have optional dependency")
                    .any(|el| { el.text_contents().contains("optional") })
            );
            let dom = kuchikiki::parse_html().one(
                env.web_app()
                    .await
                    .get("/crate/testing/0.1.0")
                    .await?
                    .text()
                    .await?,
            );
            assert!(
                dom.select(
                    r#"a[href="/crate/optional-dep/1.2.3"] > i[class="dependencies normal"] + i"#
                )
                .expect("should have optional dependency")
                .any(|el| { el.text_contents().contains("optional") })
            );
            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_missing_target_redirects_to_search(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("winapi")
                .version("0.3.9")
                .archive_storage(archive_storage)
                .rustdoc_file("winapi/macro.ENUM.html")
                .create()
                .await?;

            let web = env.web_app().await;
            web.assert_redirect(
                "/winapi/0.3.9/x86_64-unknown-linux-gnu/winapi/macro.ENUM.html",
                "/winapi/0.3.9/winapi/macro.ENUM.html",
            )
            .await?;

            web.assert_not_found("/winapi/0.3.9/winapi/struct.not_here.html")
                .await?;

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_redirect_source_not_rust(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("winapi")
                .version("0.3.8")
                .archive_storage(archive_storage)
                .source_file("src/docs.md", b"created by Peter Rabbit")
                .create()
                .await?;

            env.fake_release()
                .await
                .name("winapi")
                .version("0.3.9")
                .archive_storage(archive_storage)
                .create()
                .await?;

            let web = env.web_app().await;
            web.assert_success("/winapi/0.3.8/src/winapi/docs.md.html")
                .await?;
            // people can end up here from clicking "go to latest" while in source view
            web.assert_redirect(
                "/crate/winapi/0.3.9/target-redirect/src/winapi/docs.md.html",
                "/winapi/0.3.9/winapi/",
            )
            .await?;
            Ok(())
        })
    }

    #[test]
    fn noindex_nonlatest() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .rustdoc_file("dummy/index.html")
                .create()
                .await?;

            let web = env.web_app().await;

            assert!(
                web.get("/dummy/0.1.0/dummy/")
                    .await?
                    .headers()
                    .get("x-robots-tag")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("noindex")
            );

            assert!(
                web.get("/dummy/latest/dummy/")
                    .await?
                    .headers()
                    .get("x-robots-tag")
                    .is_none()
            );
            Ok(())
        })
    }

    #[test]
    fn download_unknown_version_404() {
        async_wrapper(|env| async move {
            let web = env.web_app().await;

            let response = web.get("/crate/dummy/0.1.0/download").await?;
            response.assert_cache_control(CachePolicy::NoCaching, &env.config());
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            Ok(())
        });
    }

    #[test]
    fn download_old_storage_version_404() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(false)
                .create()
                .await?;

            let web = env.web_app().await;

            let response = web.get("/crate/dummy/0.1.0/download").await?;
            response.assert_cache_control(CachePolicy::NoCaching, &env.config());
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            Ok(())
        });
    }

    #[test]
    fn download_semver() {
        async_wrapper(|env| async move {
            env.override_config(|config| {
                config.s3_static_root_path = "https://static.docs.rs".into()
            });
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .create()
                .await?;

            let web = env.web_app().await;

            web.assert_redirect_cached_unchecked(
                "/crate/dummy/0.1/download",
                "https://static.docs.rs/rustdoc/dummy/0.1.0.zip",
                CachePolicy::ForeverInCdn,
                &env.config(),
            )
            .await?;
            assert!(
                env.async_storage()
                    .await
                    .get_public_access("rustdoc/dummy/0.1.0.zip")
                    .await?
            );
            Ok(())
        });
    }

    #[test]
    fn download_specific_version() {
        async_wrapper(|env| async move {
            env.override_config(|config| {
                config.s3_static_root_path = "https://static.docs.rs".into()
            });
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .create()
                .await?;

            let web = env.web_app().await;
            let storage = env.async_storage().await;

            // disable public access to be sure that the handler will enable it
            storage
                .set_public_access("rustdoc/dummy/0.1.0.zip", false)
                .await?;

            web.assert_redirect_cached_unchecked(
                "/crate/dummy/0.1.0/download",
                "https://static.docs.rs/rustdoc/dummy/0.1.0.zip",
                CachePolicy::ForeverInCdn,
                &env.config(),
            )
            .await?;
            assert!(storage.get_public_access("rustdoc/dummy/0.1.0.zip").await?);
            Ok(())
        });
    }

    #[test]
    fn download_latest_version() {
        async_wrapper(|env| async move {
            env.override_config(|config| {
                config.s3_static_root_path = "https://static.docs.rs".into()
            });
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .create()
                .await?;

            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(true)
                .create()
                .await?;

            let web = env.web_app().await;

            web.assert_redirect_cached_unchecked(
                "/crate/dummy/latest/download",
                "https://static.docs.rs/rustdoc/dummy/0.2.0.zip",
                CachePolicy::ForeverInCdn,
                &env.config(),
            )
            .await?;
            assert!(
                env.async_storage()
                    .await
                    .get_public_access("rustdoc/dummy/0.2.0.zip")
                    .await?
            );
            Ok(())
        });
    }

    #[test_case("something.js")]
    #[test_case("something.css")]
    fn serve_release_specific_static_assets(name: &str) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .rustdoc_file_with(name, b"content")
                .create()
                .await?;

            let web = env.web_app().await;
            let response = web.get(&format!("/dummy/0.1.0/{name}")).await?;
            assert!(response.status().is_success());
            assert_eq!(response.text().await?, "content");

            Ok(())
        })
    }

    #[test_case("search-1234.js")]
    #[test_case("settings-1234.js")]
    fn fallback_to_root_storage_for_some_js_assets(path: &str) {
        // test workaround for https://github.com/rust-lang/docs.rs/issues/1979
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .create()
                .await?;

            let storage = env.async_storage().await;
            storage.store_one("asset.js", *b"content").await?;
            storage.store_one(path, *b"more_content").await?;

            let web = env.web_app().await;

            assert_eq!(
                web.get("/dummy/0.1.0/asset.js").await?.status(),
                StatusCode::NOT_FOUND
            );
            assert!(web.get("/asset.js").await?.status().is_success());

            assert!(web.get(&format!("/{path}")).await?.status().is_success());
            let response = web.get(&format!("/dummy/0.1.0/{path}")).await?;
            assert!(response.status().is_success());
            assert_eq!(response.text().await?, "more_content");

            Ok(())
        })
    }

    #[test]
    fn redirect_with_encoded_chars_in_path() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("clap")
                .version("2.24.0")
                .archive_storage(true)
                .create()
                .await?;
            let web = env.web_app().await;

            web.assert_redirect_cached_unchecked(
                "/clap/2.24.0/i686-pc-windows-gnu/clap/which%20is%20a%20part%20of%20%5B%60Display%60%5D",
                "/crate/clap/2.24.0/target-redirect/i686-pc-windows-gnu/clap/which%20is%20a%20part%20of%20[%60Display%60]",
                CachePolicy::ForeverInCdn,
                &env.config(),
            ).await?;

            Ok(())
        })
    }

    #[test]
    fn search_with_encoded_chars_in_path() {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("clap")
                .version("2.24.0")
                .archive_storage(true)
                .create()
                .await?;
            let web = env.web_app().await;

            web.assert_redirect_cached_unchecked(
                "/clap/latest/clapproc%20macro%20%60Parser%60%20not%20expanded:%20Cannot%20create%20expander%20for",
                "/clap/latest/clapproc%20macro%20%60Parser%60%20not%20expanded:%20Cannot%20create%20expander%20for/clap/",
                CachePolicy::ForeverInCdn,
                &env.config(),
            ).await?;

            Ok(())
        })
    }

    #[test_case("/something/1.2.3/some_path/", "/crate/something/1.2.3")]
    #[test_case("/something/latest/some_path/", "/crate/something/latest")]
    fn rustdoc_page_from_failed_build_redirects_to_crate(path: &str, expected: &str) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("something")
                .version("1.2.3")
                .archive_storage(true)
                .build_result_failed()
                .create()
                .await?;
            let web = env.web_app().await;

            web.assert_redirect_cached(path, expected, CachePolicy::ForeverInCdn, &env.config())
                .await?;

            Ok(())
        })
    }

    #[test_case(true)]
    #[test_case(false)]
    fn test_redirect_with_query_args(archive_storage: bool) {
        async_wrapper(|env| async move {
            env.fake_release()
                .await
                .name("fake")
                .version("0.0.1")
                .archive_storage(archive_storage)
                .rustdoc_file("fake/index.html")
                .binary(true) // binary => rustdoc_status = false
                .create()
                .await?;

            let web = env.web_app().await;
            web.assert_redirect("/fake?a=b", "/crate/fake/latest?a=b")
                .await?;

            Ok(())
        });
    }

    #[test_case(
        "latest/json",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Zstd
    )]
    #[test_case(
        "latest/json.gz",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Gzip
    )]
    #[test_case(
        "0.1/json",
        "0.1.0",
        "x86_64-unknown-linux-gnu",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Zstd;
        "semver"
    )]
    #[test_case(
        "0.1.0/json",
        "0.1.0",
        "x86_64-unknown-linux-gnu",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Zstd
    )]
    #[test_case(
        "latest/json/latest",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Zstd
    )]
    #[test_case(
        "latest/json/latest.gz",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Gzip
    )]
    #[test_case(
        "latest/json/42",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        RustdocJsonFormatVersion::Version(42),
        CompressionAlgorithm::Zstd
    )]
    #[test_case(
        "latest/i686-pc-windows-msvc/json",
        "0.2.0",
        "i686-pc-windows-msvc",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Zstd
    )]
    #[test_case(
        "latest/i686-pc-windows-msvc/json.gz",
        "0.2.0",
        "i686-pc-windows-msvc",
        RustdocJsonFormatVersion::Latest,
        CompressionAlgorithm::Gzip
    )]
    #[test_case(
        "latest/i686-pc-windows-msvc/json/42",
        "0.2.0",
        "i686-pc-windows-msvc",
        RustdocJsonFormatVersion::Version(42),
        CompressionAlgorithm::Zstd
    )]
    #[test_case(
        "latest/i686-pc-windows-msvc/json/42.gz",
        "0.2.0",
        "i686-pc-windows-msvc",
        RustdocJsonFormatVersion::Version(42),
        CompressionAlgorithm::Gzip
    )]
    #[test_case(
        "latest/i686-pc-windows-msvc/json/42.zst",
        "0.2.0",
        "i686-pc-windows-msvc",
        RustdocJsonFormatVersion::Version(42),
        CompressionAlgorithm::Zstd
    )]
    fn json_download(
        request_path_suffix: &str,
        redirect_version: &str,
        redirect_target: &str,
        redirect_format_version: RustdocJsonFormatVersion,
        redirect_compression: CompressionAlgorithm,
    ) {
        async_wrapper(|env| async move {
            env.override_config(|config| {
                config.s3_static_root_path = "https://static.docs.rs".into();
            });
            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .default_target("x86_64-unknown-linux-gnu")
                .add_target("i686-pc-windows-msvc")
                .create()
                .await?;

            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(true)
                .default_target("x86_64-unknown-linux-gnu")
                .add_target("i686-pc-windows-msvc")
                .create()
                .await?;

            let web = env.web_app().await;

            let compression_ext = file_extension_for(redirect_compression);

            web.assert_redirect_cached_unchecked(
                &format!("/crate/dummy/{request_path_suffix}"),
                &format!("https://static.docs.rs/rustdoc-json/dummy/{redirect_version}/{redirect_target}/\
                    dummy_{redirect_version}_{redirect_target}_{redirect_format_version}.json.{compression_ext}"),
                CachePolicy::ForeverInCdn,
                &env.config(),
            )
            .await?;
            Ok(())
        });
    }

    #[test_case("")]
    #[test_case(".zst")]
    fn test_json_download_fallback_to_old_files_without_compression_extension(ext: &str) {
        async_wrapper(|env| async move {
            env.override_config(|config| {
                config.s3_static_root_path = "https://static.docs.rs".into();
            });

            const NAME: &str = "dummy";
            const VERSION: &str = "0.1.0";
            const TARGET: &str = "x86_64-unknown-linux-gnu";
            const FORMAT_VERSION: RustdocJsonFormatVersion = RustdocJsonFormatVersion::Latest;

            env.fake_release()
                .await
                .name(NAME)
                .version(VERSION)
                .archive_storage(true)
                .default_target(TARGET)
                .create()
                .await?;

            let storage = env.async_storage().await;

            let zstd_blob = storage
                .get(
                    &rustdoc_json_path(
                        NAME,
                        VERSION,
                        TARGET,
                        FORMAT_VERSION,
                        Some(CompressionAlgorithm::Zstd),
                    ),
                    usize::MAX,
                )
                .await?;

            for compression in RUSTDOC_JSON_COMPRESSION_ALGORITHMS {
                let path =
                    rustdoc_json_path(NAME, VERSION, TARGET, FORMAT_VERSION, Some(*compression));
                storage.delete_prefix(&path).await?;
                assert!(!storage.exists(&path).await?);
            }
            storage
                .store_one(
                    &rustdoc_json_path(NAME, VERSION, TARGET, FORMAT_VERSION, None),
                    zstd_blob.content,
                )
                .await?;

            let web = env.web_app().await;

            web.assert_redirect_cached_unchecked(
                &format!("/crate/dummy/latest/json{ext}"),
                &format!(
                    "https://static.docs.rs/rustdoc-json/{NAME}/{VERSION}/{TARGET}/\
                    {NAME}_{VERSION}_{TARGET}_{FORMAT_VERSION}.json" // without .zstd
                ),
                CachePolicy::ForeverInCdn,
                &env.config(),
            )
            .await?;
            Ok(())
        });
    }

    #[test_case("0.1.0/json"; "rustdoc status false")]
    #[test_case("0.2.0/unknown-target/json"; "unknown target")]
    #[test_case("0.2.0/json/99"; "target file doesnt exist")]
    #[test_case("0.42.0/json"; "unknown version")]
    fn json_download_not_found(request_path_suffix: &str) {
        async_wrapper(|env| async move {
            env.override_config(|config| {
                config.s3_static_root_path = "https://static.docs.rs".into();
            });

            env.fake_release()
                .await
                .name("dummy")
                .version("0.1.0")
                .archive_storage(true)
                .default_target("x86_64-unknown-linux-gnu")
                .add_target("i686-pc-windows-msvc")
                .binary(true) // binary => rustdoc_status = false
                .create()
                .await?;

            env.fake_release()
                .await
                .name("dummy")
                .version("0.2.0")
                .archive_storage(true)
                .default_target("x86_64-unknown-linux-gnu")
                .add_target("i686-pc-windows-msvc")
                .create()
                .await?;

            let web = env.web_app().await;

            let response = web
                .get(&format!("/crate/dummy/{request_path_suffix}"))
                .await?;

            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            Ok(())
        });
    }
}
