use anyhow::{Result, bail};
use async_tar::Archive;
use docs_rs_cargo_metadata::CargoMetadata;
use docs_rs_database::releases::{
    finish_build, finish_release, initialize_build, initialize_crate, initialize_release,
};
use docs_rs_registry_api::RegistryApi;
use docs_rs_repository_stats::RepositoryStatsUpdater;
use docs_rs_rustdoc_json::{
    RUSTDOC_JSON_COMPRESSION_ALGORITHMS, RustdocJsonFormatVersion,
    read_format_version_from_rustdoc_json,
};
use docs_rs_storage::{
    AsyncStorage, compression::wrap_reader_for_decompression, file_list_to_json,
    rustdoc_archive_path, source_archive_path,
};
use docs_rs_storage::{compress, decompress, rustdoc_json_path};
use docs_rs_types::{BuildStatus, KrateName, ReqVersion, Version};
use docs_rs_utils::{BUILD_VERSION, spawn_blocking};
use docsrs_metadata::{BuildTargets, Metadata};
use futures_util::StreamExt as _;
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tokio::{
    fs,
    io::AsyncReadExt as _,
    io::{self, AsyncSeekExt, AsyncWriteExt},
    process::Command,
};
use tracing::{debug, info, instrument};
use walkdir::WalkDir;

const DOCS_RS: &str = "https://docs.rs";
const DEFAULT_TARGET: &str = "x86_64-unknown-linux-gnu";

/// import an existing crate release build from docs.rs into the
/// local database & storage.
///
/// CAVEATS:
/// * is currently only tested for newer releases, since there are some hacks in place.
/// * to find the needed rustdoc-static files, we have to scan all the HTML files for certain paths.
///   For bigger releases this might take some time.
/// * we assume when the normal target build is successfull, we also have a valid rustdoc json file,
///   and we'll ignore any rustdoc JSON files related to failed targets.
/// * build logs are fake, but are created.
///
/// SECURITY:
/// we execute `cargo metadata` on the downloaded source code, so
/// this function MUST NOT be used with untrusted crate names/versions.
#[instrument(skip_all, fields(name=%name, version=%version))]
pub(crate) async fn import_test_release(
    conn: &mut sqlx::PgConnection,
    storage: &AsyncStorage,
    registry_api: &RegistryApi,
    repository_stats: &RepositoryStatsUpdater,
    name: &KrateName,
    version: &ReqVersion,
) -> Result<()> {
    let status = fetch_rustdoc_status(name, version).await?;
    if !status.doc_status {
        bail!("No rustdoc available for {name} {version}");
    }
    let version = status.version;

    let crate_id = initialize_crate(&mut *conn, name).await?;
    let release_id = initialize_release(&mut *conn, crate_id, &version).await?;
    let build_id = initialize_build(&mut *conn, release_id).await?;

    // FIXME: if any errors happen here, use update_build_with_error, don't `finish_release`

    info!("download & inspect source from crates.io...");
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

    let mut algs = HashSet::new();
    let (source_files_list, source_size) = {
        info!("writing source files to storage...");
        let (files_list, new_alg) = storage
            .store_all_in_archive(&source_archive_path(name, &version), &source_dir)
            .await?;

        algs.insert(new_alg);
        let source_size: u64 = files_list.iter().map(|info| info.size).sum();
        (files_list, source_size)
    };

    let registry_data = registry_api.get_release_data(name, &version).await?;

    let rustdoc_dir = {
        info!("download & extract rustdoc archive...");
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

    info!("find successfull build targets...");
    let BuildTargets {
        default_target,
        other_targets,
    } = docsrs_metadata.targets_for_host(true, DEFAULT_TARGET);
    let mut targets = vec![default_target];

    // from the "outside" we have no way to find the list of "successful targets".
    let mut potential_other_targets: HashSet<String> =
        other_targets.iter().map(|t| t.to_string()).collect();
    potential_other_targets.extend(fetch_target_list().await?.into_iter());
    potential_other_targets.remove(default_target);

    for t in &potential_other_targets {
        if rustdoc_dir.path().join(t).is_dir() {
            // non-default targets lead to a subdirectory in rustdoc
            targets.push(t);
        }
    }

    // FIXME: add fake build logs for JSON & normal for all targets in the metadata.

    info!("finding used rustdoc static files in HTML files...");
    let mut static_files = find_rustdoc_static_urls(&rustdoc_dir).await?;
    // these files aren't referenced directly in the HTML code, but their imports
    // are generated through JS.
    // Since these are statically known and barely change, I can just add them here.
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
            debug!("static file already exists in storage: {}", &key);
            continue;
        }

        storage
            .store_one(key, download(format!("https://docs.rs{path}")).await?)
            .await?;
    }

    info!("writing rustdoc files to storage...");
    let (rustdoc_file_list, new_alg) = storage
        .store_all_in_archive(&rustdoc_archive_path(name, &version), &rustdoc_dir)
        .await?;
    let documentation_size: u64 = rustdoc_file_list.iter().map(|info| info.size).sum();
    algs.insert(new_alg);

    info!("loading repository stats...");
    let repository_id = repository_stats
        .load_repository(cargo_metadata.root())
        .await?;

    for target in &targets {
        info!("copying rustdoc json for target {target}...");

        let json_compression = RUSTDOC_JSON_COMPRESSION_ALGORITHMS[0];
        let rustdoc_json = decompress(
            // FIXME: worth using async decompress here?
            &*download(format!(
                "https://docs.rs/crate/{name}/{version}/{target}/json.{}",
                json_compression.file_extension()
            ))
            .await?,
            json_compression,
            usize::MAX,
        )?;
        if rustdoc_json.is_empty() || rustdoc_json[0] != b'{' {
            bail!("invalid rustdoc json for {name} {version} {target}");
        }

        let format_version = spawn_blocking({
            let rustdoc_json = rustdoc_json.clone();
            move || read_format_version_from_rustdoc_json(&*rustdoc_json)
        })
        .await?;

        for alg in RUSTDOC_JSON_COMPRESSION_ALGORITHMS {
            // FIXME: worth using async compress here?
            let compressed_json = compress(&*rustdoc_json, *alg)?;

            for format_version in [format_version, RustdocJsonFormatVersion::Latest] {
                let path = rustdoc_json_path(name, &version, target, format_version, Some(*alg));
                storage
                    .store_one_uncompressed(&path, compressed_json.clone())
                    .await?;
            }
        }
    }

    info!("finish release & build");
    finish_release(
        &mut *conn,
        crate_id,
        release_id,
        cargo_metadata.root(),
        &source_dir,
        default_target,
        file_list_to_json(source_files_list),
        targets.iter().map(|t| t.to_string()).collect(),
        &registry_data,
        true,
        false, // FIXME: real has_examples?
        algs,
        repository_id,
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
    debug!("fetching rustdoc status...");
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
    debug!("downloading...");

    Ok(reqwest::get(url)
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

#[instrument]
async fn download_to_temp_file(url: impl reqwest::IntoUrl + fmt::Debug) -> Result<fs::File> {
    debug!("downloading to temp file..");

    let response = reqwest::get(url).await?.error_for_status()?;

    // NOTE: even after being convert to a `tokio::fs::File`, this kind of temporary file
    // will be cleaned up by the OS, when the last handle is closed.
    let mut file = fs::File::from_std(spawn_blocking(|| Ok(tempfile::tempfile()?)).await?);

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
    }

    file.sync_all().await?;
    file.seek(std::io::SeekFrom::Start(0)).await?;
    Ok(file)
}

async fn fetch_target_list() -> Result<HashSet<String>> {
    let output = Command::new("rustc")
        .arg("--print")
        .arg("target-list")
        .output()
        .await?;

    if !output.status.success() {
        bail!(
            "`rustc --print target-list` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout)?;

    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

#[instrument]
async fn download_and_extract_source(name: &KrateName, version: &Version) -> Result<SourceDir> {
    debug!("downloading source");
    let crate_archive = download_to_temp_file(format!(
        "https://static.crates.io/crates/{name}/{name}-{version}.crate"
    ))
    .await?;

    let temp_dir = spawn_blocking(|| Ok(tempfile::tempdir()?)).await?;

    debug!("unpacking source archive");
    {
        let mut file = io::BufReader::new(crate_archive);
        let mut decompressed =
            wrap_reader_for_decompression(&mut file, docs_rs_types::CompressionAlgorithm::Gzip);
        let archive = Archive::new(&mut decompressed);
        archive.unpack(&temp_dir).await?;
    }

    let source_path = temp_dir.path().join(format!("{name}-{version}"));
    if !source_path.is_dir() {
        bail!(
            "broken crate archive, missing source directory {:?}",
            source_path
        );
    };

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
        LazyLock::new(|| Regex::new(r#""(/-/rustdoc\.static/[^"]+)""#).unwrap());

    let root_dir = root_dir.as_ref();
    let html_files = spawn_blocking({
        let root_dir = root_dir.to_path_buf();
        move || {
            let mut files = Vec::new();
            for entry in WalkDir::new(&root_dir).follow_links(false).into_iter() {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type().is_file()
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
                {
                    files.push(path.to_path_buf());
                }
            }
            Ok(files)
        }
    })
    .await?;

    let mut urls = HashSet::new();

    let mut file_stream =
        futures_util::stream::iter(html_files.into_iter().map(|path| async move {
            let mut file = fs::File::open(&path).await?;
            let mut contents = String::new();
            file.read_to_string(&mut contents).await?;

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
