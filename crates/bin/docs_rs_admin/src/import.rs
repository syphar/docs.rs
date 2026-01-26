use anyhow::{Context as _, Result, bail};
use docs_rs_database::releases::{
    finish_release, initialize_build, initialize_crate, initialize_release,
};
use docs_rs_storage::AsyncStorage;
use docs_rs_storage::compression::wrap_reader_for_decompression;
use docs_rs_types::{KrateName, ReqVersion, Version};
use flate2::read::GzDecoder;
use futures_util::StreamExt as _;
use serde::Deserialize;
use std::fs::File;
use std::path::Path;
use std::process::Stdio;
use tar::Archive;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::task::spawn_blocking;
use tokio::{fs, io};

const DOCS_RS: &str = "https://docs.rs";

pub(crate) async fn import_test_release(
    conn: &mut sqlx::PgConnection,
    storage: &AsyncStorage,
    name: &KrateName,
    version: &ReqVersion,
) -> Result<()> {
    let status = fetch_rustdoc_status(&name, version).await?;
    if !status.doc_status {
        bail!("No rustdoc available for {name} {version}");
    }

    let version = status.version;

    // potential improvement: full delete of the release before import.

    let crate_id = initialize_crate(&mut *conn, name).await?;
    let release_id = initialize_release(&mut *conn, crate_id, &version).await?;
    let build_id = initialize_build(&mut *conn, release_id).await?;

    // TODO:
    // * download crate tar gz from crates.io, convert into source archive with index
    // * download rustdoc archive from docs.rs, create archive index, upload both
    // * download JSON builds from docs.rs (which?)
    // * finish_release ( try to find all info needed somewhere)
    // * finish_build ( try to find all info needed somewhere)
    // finish_release(
    //     &mut *conn, crate_id,
    //     release_id,
    //     // metadata_pkg: &MetadataPackage,
    //     // source_dir: &Path,
    //     // default_target: &str,
    //     // source_files: Value,
    //     // doc_targets: Vec<String>,
    //     // registry_data: &ReleaseData,
    //     // has_docs: bool,
    //     // has_examples: bool,
    //     // compression_algorithms: impl IntoIterator<Item = CompressionAlgorithm>,
    //     // repository_id: Option<i32>,
    //     // archive_storage: bool,
    //     // source_size: u64,
    // )
    // .await?;

    todo!();
}

#[derive(Debug, Deserialize)]
struct RustdocStatus {
    doc_status: bool,
    version: Version,
}

async fn fetch_rustdoc_status(name: &KrateName, version: &ReqVersion) -> Result<RustdocStatus> {
    Ok(
        reqwest::get(&format!("{DOCS_RS}/crate/{name}/{version}/status.json"))
            .await?
            .error_for_status()?
            .json()
            .await?,
    )
}

async fn download_and_extract_source(
    name: &KrateName,
    version: &Version,
    target_dir: impl AsRef<Path>,
) -> Result<()> {
    let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");

    let target_dir = target_dir.as_ref();

    if !target_dir.exists() {
        bail!("target_dir does not exist: {}", target_dir.display());
    }

    let mut curl = Command::new("curl")
        .args(["-fsSL", &url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let curl_stdout = curl
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "failed to take curl stdout"))?;

    let mut tar = Command::new("tar")
        .arg("-xz")
        .arg("--strip-components=1")
        .arg("-C")
        .arg(target_dir)
        .args(["-f", "-"])
        .stdin(Stdio::from(curl_stdout))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let tar_out = tar.wait_with_output().await?;
    let curl_out = curl.wait_with_output().await?;

    if !tar_out.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "tar failed ({}): {}",
                tar_out.status,
                String::from_utf8_lossy(&tar_out.stderr)
            ),
        ));
    }

    if !curl_out.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "curl failed ({}): {}",
                curl_out.status,
                String::from_utf8_lossy(&curl_out.stderr)
            ),
        ));
    }

    Ok(())
}
