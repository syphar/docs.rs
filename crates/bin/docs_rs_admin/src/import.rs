use anyhow::{Context as _, Result, bail};
use async_tar::Archive;
use docs_rs_cargo_metadata::{CargoMetadata, MetadataPackage};
use docs_rs_database::releases::{
    finish_build, finish_release, initialize_build, initialize_crate, initialize_release,
};
use docs_rs_mimes as mimes;
use docs_rs_registry_api::RegistryApi;
use docs_rs_storage::{
    AsyncStorage, BlobUpload,
    archive_index::{self, ARCHIVE_INDEX_FILE_EXTENSION},
    compress_async,
    compression::wrap_reader_for_decompression,
    file_list_to_json, rustdoc_archive_path, source_archive_path,
};
use docs_rs_types::{BuildStatus, CompressionAlgorithm, KrateName, ReqVersion, Version};
use docs_rs_utils::{BUILD_VERSION, spawn_blocking};
use docsrs_metadata::{BuildTargets, DEFAULT_TARGETS, HOST_TARGET, Metadata};
use futures_util::StreamExt as _;
use serde::Deserialize;
use std::fmt;
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
/// SECURITY:
/// we execute `cargo metadata` on the downloaded source code, so
/// this function MUST NOT be used with untrusted crate names/versions.
#[instrument(skip(conn, storage, registry_api))]
pub(crate) async fn import_test_release(
    conn: &mut sqlx::PgConnection,
    storage: &AsyncStorage,
    registry_api: &RegistryApi,
    name: &KrateName,
    version: &ReqVersion,
) -> Result<()> {
    // TODO:
    // * download JSON builds from docs.rs (which?)
    // * find used rustc version somehow?
    // * add_essential_files for the needed nightly version?

    let status = fetch_rustdoc_status(&name, version).await?;
    if !status.doc_status {
        bail!("No rustdoc available for {name} {version}");
    }

    let version = status.version;

    // potential improvement: full delete of the release before import.

    let crate_id = initialize_crate(&mut *conn, name).await?;
    let release_id = initialize_release(&mut *conn, crate_id, &version).await?;
    let build_id = initialize_build(&mut *conn, release_id).await?;

    let source_dir = download_and_extract_source(name, &version).await?;
    dbg!(&source_dir);

    // FIXME: spawn_blocking for sync stuff?
    let cargo_metadata = CargoMetadata::load_from_host_path(&source_dir)?;
    let docsrs_metadata = Metadata::from_crate_root(&source_dir)?;
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

    {
        let mut zip_content = {
            let mut rustdoc_archive =
                download_file(format!("https://docs.rs/crate/{name}/{version}/download")).await?;

            let mut buf = Vec::new();
            rustdoc_archive.read_to_end(&mut buf).await?;
            buf
        };

        let archive_path = rustdoc_archive_path(name, &version);

        let remote_index_path = format!("{}.{ARCHIVE_INDEX_FILE_EXTENSION}", &archive_path);
        let index_compression = CompressionAlgorithm::default();
        let compressed_index_content = {
            let local_index_path =
                spawn_blocking(|| Ok(tempfile::NamedTempFile::new()?.into_temp_path())).await?;

            archive_index::create(
                &mut std::io::Cursor::new(&mut zip_content),
                &local_index_path,
            )
            .await?;

            let mut buf: Vec<u8> = Vec::new();
            compress_async(
                &mut io::BufReader::new(fs::File::open(&local_index_path).await?),
                &mut buf,
                index_compression,
            )
            .await?;
            buf
        };

        storage
            .store_blobs(vec![
                BlobUpload {
                    path: archive_path.to_string(),
                    mime: mimes::APPLICATION_ZIP.clone(),
                    content: zip_content,
                    compression: None,
                },
                BlobUpload {
                    path: remote_index_path,
                    mime: mime::APPLICATION_OCTET_STREAM,
                    content: compressed_index_content,
                    compression: Some(index_compression),
                },
            ])
            .await?;
    }

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
        None, // FIXMED: repository_id: Option<i32>,
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
        None, // FIXME: documentation size
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
async fn download_file(url: impl reqwest::IntoUrl + fmt::Debug) -> Result<fs::File> {
    info!("downloading file");

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
    let crate_archive = download_file(format!(
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
