pub mod manifest;

use crate::source_archive::manifest::{FileEntry, Manifest};
use anyhow::{Context as _, Result, bail, ensure};
use async_compression::tokio::bufread::DeflateDecoder;
use async_http_range_reader::AsyncHttpRangeReader;
use async_zip::base::read::stream::ZipFileReader;
use futures_util::TryStreamExt as _;
use reqwest::{
    StatusCode, Url,
    header::{HeaderMap, HeaderName, RANGE},
};
use tokio::io::{self, AsyncWrite, AsyncWriteExt as _};
use tokio_util::{compat::TokioAsyncReadCompatExt as _, io::StreamReader};
use tracing::{debug, field, instrument};

pub static X_CACHE: HeaderName = HeaderName::from_static("x-cache");

fn is_cache_hit(hm: &HeaderMap) -> bool {
    hm.get(&X_CACHE)
        .and_then(|hv| hv.to_str().ok())
        .map(|hv| hv.contains("HIT"))
        .unwrap_or(false)
}

pub struct SourceArchive {
    manifest: Manifest,
    zip_url: Url,
    client: reqwest::Client,
}

/// Number of bytes requested initially and buffered during sequential ZIP reads.
const ZIP_READ_BLOCK_SIZE: usize = 8192;

/// Construct a source archive URL relative to the registry's static host.
fn source_archive_url(
    mut base_url: Url,
    name: &str,
    version: &str,
    extension: &str,
) -> Result<Url> {
    base_url.set_path("crates/");
    Ok(base_url.join(&format!("{name}/{name}-{version}.{extension}"))?)
}

impl SourceArchive {
    /// Fetch the first ZIP entry without loading the archive inventory or central directory.
    #[instrument(skip_all, fields(%base_url, %name, %version, cache_hit=field::Empty))]
    pub(crate) async fn fetch_cargo_toml(
        client: reqwest::Client,
        base_url: Url,
        name: &str,
        version: &str,
    ) -> Result<Option<Vec<u8>>> {
        let zip_url = source_archive_url(base_url, name, version, "zip")?;
        let response = client
            .get(zip_url.clone())
            .header(RANGE, format!("bytes=0-{}", ZIP_READ_BLOCK_SIZE - 1))
            .send()
            .await?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
        ) {
            return Ok(None);
        }
        let response = response.error_for_status()?;
        ensure!(
            response.status() == StatusCode::PARTIAL_CONTENT,
            "source archive server did not honor the range request"
        );
        tracing::Span::current().record("cache_hit", is_cache_hit(response.headers()));
        ensure!(
            response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("bytes 0-")),
            "source archive range response does not start at byte zero"
        );

        let reader =
            AsyncHttpRangeReader::from_range_response(client, response, zip_url, HeaderMap::new())
                .await?;
        let reader = tokio::io::BufReader::with_capacity(ZIP_READ_BLOCK_SIZE, reader);
        let mut first = ZipFileReader::new(reader.compat())
            .next_with_entry()
            .await?
            .context("source archive contains no first entry")?;
        ensure!(
            first
                .reader()
                .entry()
                .filename()
                .as_str()?
                .eq_ignore_ascii_case("Cargo.toml"),
            "first source archive entry is not Cargo.toml"
        );
        ensure!(
            !first.reader().entry().data_descriptor(),
            "Cargo.toml uses an unsupported ZIP data descriptor"
        );
        let expected_size = first.reader().entry().uncompressed_size();
        let mut contents = Vec::new();
        first
            .reader_mut()
            .read_to_end_checked(&mut contents)
            .await?;
        ensure!(
            contents.len() as u64 == expected_size,
            "Cargo.toml uncompressed size mismatch"
        );
        Ok(Some(contents))
    }

    #[instrument(skip_all, fields( %base_url, %name, %version, cache_hit=field::Empty))]
    pub(crate) async fn load(
        client: reqwest::Client,
        base_url: Url,
        name: &str,
        version: &str,
    ) -> Result<Option<Self>> {
        let index_url = source_archive_url(base_url.clone(), name, version, "zip.json")?;

        debug!(%index_url, "fetching source archive manifest");
        let response = client.get(index_url.clone()).send().await?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
        ) {
            return Ok(None);
        }
        let response = response.error_for_status()?;

        tracing::Span::current().record("cache_hit", is_cache_hit(response.headers()));

        Ok(Some(Self {
            manifest: response.json().await?,
            zip_url: source_archive_url(base_url, name, version, "zip")?,
            client,
        }))
    }

    pub fn entries(&self) -> impl Iterator<Item = &FileEntry> {
        self.manifest.files.iter()
    }

    pub fn by_name(&self, path: impl AsRef<str>) -> Option<&FileEntry> {
        let path = path.as_ref();
        self.manifest.files.iter().find(|e| e.path == path)
    }

    #[instrument(skip_all, fields(zip_url=%self.zip_url, path=%entry.path, cache_hit=field::Empty))]
    pub async fn fetch<W>(&self, entry: &FileEntry, writer: &mut W) -> Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let range_start = entry.data_offset;
        let range_end = entry.data_offset + entry.compressed_size - 1;

        debug!(range_start, range_end, "fetching file from source archive");
        let response = self
            .client
            .get(self.zip_url.clone())
            .header(RANGE, format!("bytes={range_start}-{range_end}",))
            .send()
            .await?
            .error_for_status()?;

        tracing::Span::current().record("cache_hit", is_cache_hit(response.headers()));

        let stream = response.bytes_stream().map_err(std::io::Error::other);
        let mut reader = StreamReader::new(stream);

        match entry.compression.as_str() {
            "deflate" => {
                let mut decoder = DeflateDecoder::new(reader);
                io::copy(&mut decoder, writer).await?;
            }
            "store" => {
                io::copy(&mut reader, writer).await?;
            }
            compression => bail!("unsupported zip compression: {}", compression),
        }

        writer.flush().await?;

        Ok(())
    }

    pub async fn fetch_bytes(&self, entry: &FileEntry) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.fetch(entry, &mut buf).await?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::static_test_env::{TestStaticCratesIo, create_test_source_archive};
    use docs_rs_types::testing::{KRATE, V0_1};

    fn client() -> reqwest::Client {
        reqwest::Client::builder().build().unwrap()
    }

    /// Exercise prefix-only reads and automatic continuation without a JSON inventory.
    #[test_case::test_case("Cargo.toml", 32, false; "small")]
    #[test_case::test_case("cargo.toml", 32, false; "lowercase")]
    #[test_case::test_case("Cargo.toml", 32768, false; "multiple_ranges")]
    #[test_case::test_case("src/lib.rs", 32, true; "wrong_first_entry")]
    #[test_case::test_case("Cargo.toml", 32, true; "bad_crc")]
    #[tokio::test]
    async fn test_fetch_cargo_toml(path: &str, size: usize, invalid: bool) -> anyhow::Result<()> {
        // Deterministic pseudo-random data keeps the large entry larger than one block.
        let mut state = 42u32;
        let contents: Vec<u8> = (0..size)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let (_, mut zip) = create_test_source_archive([
            (path, contents.as_slice()),
            ("other.txt", b"other".as_slice()),
        ])?;
        if invalid && path == "Cargo.toml" {
            zip[14] ^= 1;
        }
        let mut server = mockito::Server::new_async().await;
        let manifest = server
            .mock("GET", "/crates/krate/krate-0.1.0.zip.json")
            .expect(0)
            .create_async()
            .await;
        let initial_end = (ZIP_READ_BLOCK_SIZE - 1).min(zip.len() - 1);
        let initial = server
            .mock("GET", "/crates/krate/krate-0.1.0.zip")
            .match_header("range", "bytes=0-8191")
            .with_status(206)
            .with_header(
                "content-range",
                &format!("bytes 0-{initial_end}/{}", zip.len()),
            )
            .with_body(&zip[..=initial_end])
            .expect(1)
            .create_async()
            .await;
        let len = zip.len();
        let continuation = server
            .mock("GET", "/crates/krate/krate-0.1.0.zip")
            .match_header(
                "range",
                mockito::Matcher::Regex("^bytes=[1-9][0-9]*-[0-9]+$".into()),
            )
            .with_status(206)
            .with_header_from_request("content-range", move |request| {
                let range = request.header("range")[0].to_str().unwrap();
                let (start, end) = range
                    .strip_prefix("bytes=")
                    .unwrap()
                    .split_once('-')
                    .unwrap();
                format!("bytes {start}-{end}/{len}")
            })
            .with_body_from_request(move |request| {
                let range = request.header("range")[0].to_str().unwrap();
                let (start, end) = range
                    .strip_prefix("bytes=")
                    .unwrap()
                    .split_once('-')
                    .unwrap();
                zip[start.parse::<usize>().unwrap()..=end.parse::<usize>().unwrap()].to_vec()
            })
            .expect_at_least(usize::from(size > ZIP_READ_BLOCK_SIZE))
            .expect_at_most(if size > ZIP_READ_BLOCK_SIZE { 10 } else { 0 })
            .create_async()
            .await;
        let result =
            SourceArchive::fetch_cargo_toml(client(), Url::parse(&server.url())?, "krate", "0.1.0")
                .await;
        if invalid {
            assert!(result.is_err());
        } else {
            assert_eq!(result?.unwrap(), contents);
        }
        manifest.assert_async().await;
        initial.assert_async().await;
        continuation.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch() -> anyhow::Result<()> {
        let (manifest, zip) = create_test_source_archive([
            ("src/main.rs", "src/main.rs"),
            ("Cargo.toml", "Cargo.toml"),
        ])?;

        let test_env = TestStaticCratesIo::new().await?;
        test_env.add(&KRATE, &V0_1, manifest, zip).await?;

        let source_archive = SourceArchive::load(client(), test_env.url().await, "krate", "0.1.0")
            .await?
            .expect("not found");

        {
            let info = source_archive.by_name("src/main.rs").expect("should exist");
            assert_eq!(source_archive.fetch_bytes(info).await?, b"src/main.rs");
        }

        {
            let info = source_archive.by_name("Cargo.toml").expect("should exist");
            assert_eq!(source_archive.fetch_bytes(info).await?, b"Cargo.toml");
        }

        Ok(())
    }
}
