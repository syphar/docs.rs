use anyhow::{Context as _, Result, bail};
use async_tar::Archive;
use docs_rs_cargo_metadata::CargoMetadata;
use docs_rs_database::releases::{
    finish_build, finish_release, initialize_build, initialize_crate, initialize_release,
};
use docs_rs_registry_api::RegistryApi;
use docs_rs_repository_stats::RepositoryStatsUpdater;
use docs_rs_storage::{
    AsyncStorage, compression::wrap_reader_for_decompression, file_list_to_json,
    rustdoc_archive_path, source_archive_path,
};
use docs_rs_types::{BuildStatus, KrateName, ReqVersion, Version};
use docs_rs_utils::{BUILD_VERSION, spawn_blocking};
use docsrs_metadata::{BuildTargets, Metadata};
use futures_util::StreamExt as _;
use regex::Regex;
use serde::Deserialize;
use std::fmt;
use std::sync::LazyLock;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt as _;
use tokio::{
    fs,
    io::{self, AsyncSeekExt, AsyncWriteExt},
};
use tracing::{info, instrument};

const DOCS_RS: &str = "https://docs.rs";

/// import an existing crate release build from docs.rs into the
/// local database & storage.
///
/// CAVEATS:
/// * is currently only tested for newer releases, since there are some hacks in place.
/// * to find the needed rustdoc-static files, we have to scan all the HTML files for certain paths.
///   For bigger releases this might take some time.
///
/// SECURITY:
/// we execute `cargo metadata` on the downloaded source code, so
/// this function MUST NOT be used with untrusted crate names/versions.
#[instrument(skip(conn, storage, registry_api))]
pub(crate) async fn import_test_release(
    conn: &mut sqlx::PgConnection,
    storage: &AsyncStorage,
    registry_api: &RegistryApi,
    repository_stats: &RepositoryStatsUpdater,
    name: &KrateName,
    version: &ReqVersion,
) -> Result<()> {
    // TODO:
    // * download JSON builds from docs.rs (which?)
    // * find used rustc version somehow?

    let status = fetch_rustdoc_status(name, version).await?;
    if !status.doc_status {
        bail!("No rustdoc available for {name} {version}");
    }

    let version = status.version;

    // potential improvement: full delete of the release before import.

    let crate_id = initialize_crate(&mut *conn, name).await?;
    let release_id = initialize_release(&mut *conn, crate_id, &version).await?;
    let build_id = initialize_build(&mut *conn, release_id).await?;

    let source_dir = download_and_extract_source(name, &version).await?;

    let cargo_metadata = spawn_blocking({
        let source_dir = source_dir.source_path.clone();
        move || CargoMetadata::load_from_host_path(&source_dir)
    })
    .await?;
    let docsrs_metadata = spawn_blocking({
        let source_dir = source_dir.source_path.clone();
        move || Ok(Metadata::from_crate_root(&source_dir)?)
    })
    .await?;

    let BuildTargets {
        default_target,
        other_targets,
    } = docsrs_metadata.targets(true);
    let mut targets = vec![default_target];
    targets.extend(&other_targets);

    let mut algs = HashSet::new();
    let (source_files_list, source_size) = {
        info!("adding sources into database");
        let (files_list, new_alg) = storage
            .store_all_in_archive(&source_archive_path(name, &version), &source_dir)
            .await?;

        algs.insert(new_alg);
        let source_size: u64 = files_list.iter().map(|info| info.size).sum();
        (files_list, source_size)
    };

    let registry_data = registry_api.get_release_data(name, &version).await?;

    let rustdoc_dir = {
        let rustdoc_archive =
            download_to_temp_file(format!("https://docs.rs/crate/{name}/{version}/download"))
                .await?
                .into_std()
                .await;

        spawn_blocking(|| {
            let mut zip = zip::ZipArchive::new(rustdoc_archive)?;

            let temp_dir = tempfile::tempdir()?;
            zip.extract(&temp_dir)?;
            Ok(temp_dir)
        })
        .await?
    };

    let mut static_files = find_rustdoc_static_urls(&rustdoc_dir).await?;
    static_files.remove("/-/rustdoc.static/${f}");
    static_files.extend(
        [
            "/-/rustdoc.static/FiraSans-Italic-81dc35de.woff2",
            "/-/rustdoc.static/FiraSans-Medium-e1aa3f0a.woff2",
            "/-/rustdoc.static/FiraSans-MediumItalic-ccf7e434.woff2",
            "/-/rustdoc.static/FiraSans-Regular-0fe48ade.woff2",
            "/-/rustdoc.static/SourceCodePro-Regular-8badfe75.ttf.woff2",
            "/-/rustdoc.static/SourceCodePro-Semibold-aa29a496.ttf.woff2",
            "/-/rustdoc.static/SourceSerif4-Regular-6b053e98.ttf.woff2",
        ]
        .iter()
        .map(|s| s.to_string()),
    );

    for path in &static_files {
        let key = format!(
            "{}{}",
            docs_rs_utils::RUSTDOC_STATIC_STORAGE_PREFIX,
            path.trim_start_matches("/-/rustdoc.static/")
        );

        if storage.exists(&key).await? {
            info!("static file already exists in storage: {}", &key);
            continue;
        }

        storage
            .store_one(key, download(format!("https://docs.rs{path}")).await?)
            .await?;
    }

    let (rustdoc_files_list, new_alg) = storage
        .store_all_in_archive(&rustdoc_archive_path(name, &version), &rustdoc_dir)
        .await?;
    let documentation_size: u64 = rustdoc_files_list.iter().map(|info| info.size).sum();
    algs.insert(new_alg);

    let repository_id = repository_stats
        .load_repository(cargo_metadata.root())
        .await?
        .ok_or_else(|| anyhow::anyhow!("failed to find repository for crate {name}",))?;

    finish_release(
        &mut *conn,
        crate_id,
        release_id,
        cargo_metadata.root(),
        &source_dir,
        default_target,
        file_list_to_json(source_files_list),
        targets.into_iter().map(|t| t.to_string()).collect(), // FIXME: this should be only successful targets
        &registry_data,
        true,
        false, // FIXME: real has_examples?
        algs,
        Some(repository_id),
        true,
        source_size,
    )
    .await?;

    finish_build(
        &mut *conn,
        build_id,
        "rustc 1.95.0-nightly (873d4682c 2026-01-25)",
        BUILD_VERSION,
        BuildStatus::Success,
        Some(documentation_size),
        None,
    )
    .await?;

    Ok(())
}

#[derive(Debug, Deserialize)]
struct RustdocStatus {
    doc_status: bool,
    version: Version,
}

#[derive(Debug)]
struct SourceDir {
    _temp_dir: tempfile::TempDir,
    source_path: PathBuf,
}

impl AsRef<Path> for SourceDir {
    fn as_ref(&self) -> &Path {
        &self.source_path
    }
}

#[instrument]
async fn fetch_rustdoc_status(name: &KrateName, version: &ReqVersion) -> Result<RustdocStatus> {
    Ok(
        reqwest::get(&format!("{DOCS_RS}/crate/{name}/{version}/status.json"))
            .await?
            .error_for_status()?
            .json()
            .await?,
    )
}

#[instrument]
async fn download(url: impl reqwest::IntoUrl + fmt::Debug) -> Result<Vec<u8>> {
    info!("downloading...");

    Ok(reqwest::get(url)
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

#[instrument]
async fn download_to_temp_file(url: impl reqwest::IntoUrl + fmt::Debug) -> Result<fs::File> {
    info!("downloading to temp file..");

    // NOTE: even after being convert to a `tokio::fs::File`, this kind of temporary file
    // will be cleaned up by the us, when the last handle is closed.
    let mut file = fs::File::from_std(spawn_blocking(|| Ok(tempfile::tempfile()?)).await?);
    let response = reqwest::get(url).await?.error_for_status()?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading from response stream")?;
        file.write_all(&chunk)
            .await
            .context("error writing to temp file")?;
    }

    file.sync_all().await.context("error on fsync")?;
    file.seek(std::io::SeekFrom::Start(0)).await?;
    Ok(file)
}

#[instrument]
async fn download_and_extract_source(name: &KrateName, version: &Version) -> Result<SourceDir> {
    info!("downloading source");
    let crate_archive = download_to_temp_file(format!(
        "https://static.crates.io/crates/{name}/{name}-{version}.crate"
    ))
    .await?;

    let temp_dir = spawn_blocking(|| Ok(tempfile::tempdir()?)).await?;

    info!("unpacking source archive");
    {
        let mut file = io::BufReader::new(crate_archive);
        let mut decompressed =
            wrap_reader_for_decompression(&mut file, docs_rs_types::CompressionAlgorithm::Gzip);
        let archive = Archive::new(&mut decompressed);
        archive.unpack(&temp_dir).await?;
    }

    let source_path = temp_dir.path().join(format!("{name}-{version}"));
    debug_assert!(
        source_path.is_dir(),
        "expected source path to be a directory"
    );

    Ok(SourceDir {
        source_path,
        _temp_dir: temp_dir,
    })
}

#[instrument]
async fn find_rustdoc_static_urls(
    root_dir: impl AsRef<Path> + fmt::Debug,
) -> Result<HashSet<String>> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(/-/rustdoc\.static/[^"]+)"#).unwrap());

    let root_dir = root_dir.as_ref();
    let mut dirs = vec![root_dir.to_path_buf()];
    let mut html_files = Vec::new();

    while let Some(dir) = dirs.pop() {
        let mut entries = fs::read_dir(&dir)
            .await
            .with_context(|| format!("reading directory {dir:?}"))?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                dirs.push(path);
                continue;
            }

            if file_type.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
            {
                html_files.push(path);
            }
        }
    }

    let mut urls = HashSet::new();

    let mut file_stream =
        futures_util::stream::iter(html_files.into_iter().map(|path| async move {
            let mut file = fs::File::open(&path)
                .await
                .with_context(|| format!("opening file {path:?}"))?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .await
                .with_context(|| format!("reading file {path:?}"))?;

            let matches = RE
                .captures_iter(&contents)
                .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
                .collect::<Vec<_>>();
            Ok::<_, anyhow::Error>(matches)
        }))
        .buffer_unordered(16);

    while let Some(matches) = file_stream.next().await {
        urls.extend(matches?);
    }

    Ok(urls)
}
