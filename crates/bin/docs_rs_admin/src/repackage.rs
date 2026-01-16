use anyhow::{Context as _, Result};
use docs_rs_storage::{
    AsyncStorage, FileEntry, add_path_into_remote_archive, rustdoc_archive_path,
    source_archive_path,
};
use docs_rs_types::{CompressionAlgorithm, KrateName, Version};
use docs_rs_utils::spawn_blocking;
use futures_util::StreamExt as _;
use tokio::{fs, io};

async fn repackage(storage: &AsyncStorage, name: &KrateName, version: &Version) -> Result<()> {
    let rustdoc_prefix = format!("rustdoc/{name}/{version}");
    // let sources_prefix = format!("sources/{name}/{version}");

    repackage_path(
        storage,
        &rustdoc_prefix,
        &rustdoc_archive_path(&name, version),
    )
    .await?;

    // repackage_path(
    //     storage,
    //     &sources_prefix,
    //     &source_archive_path(&name, version),
    // )
    // .await?;

    // TODO:
    // * fill in `source_size` in release? ( new metric, not in old builds )
    // * fill in documentation_size in release / build ( new metric, not in old builds)
    // * release gets file_list from sources.
    // * releases gets algs from (source, doc)

    storage.delete_prefix(&rustdoc_prefix).await?;
    // storage.delete_prefix(&sources_prefix).await?;

    Ok(())
}

async fn repackage_path(
    storage: &AsyncStorage,
    prefix: &str,
    target_archive: &str,
) -> Result<(Vec<FileEntry>, CompressionAlgorithm)> {
    let prefix = format!("{}/", prefix.trim_end_matches('/'));
    let tempdir = spawn_blocking(|| tempfile::tempdir().map_err(Into::into)).await?;

    // TODO: optimize: directly pack into zip , don't store locally first.

    let mut list = storage.list_prefix(&prefix).await;
    while let Some(entry) = list.next().await {
        let entry = dbg!(entry?);
        let mut stream = storage
            .get_stream(&entry)
            .await
            .context("error getting stream")?;

        let target_path = dbg!(tempdir.path().join(stream.path.trim_start_matches(&prefix)));

        fs::create_dir_all(&target_path.parent().unwrap())
            .await
            .context("error creating parent directory")?;
        {
            let mut output_file = fs::File::create(dbg!(&target_path))
                .await
                .context("error creating file")?;
            io::copy(&mut stream.content, &mut output_file)
                .await
                .context("error writing to file")?;
            output_file
                .sync_all()
                .await
                .context("error flushing file")?;
        }
    }

    {
        let mut dir = fs::read_dir(&tempdir.path()).await?;

        while let Some(entry) = dir.next_entry().await? {
            dbg!(&entry.path());
        }
    };

    let (file_list, alg) = add_path_into_remote_archive(storage, target_archive, &tempdir.path())
        .await
        .context("error adding into remote archive")?;

    fs::remove_dir_all(&tempdir)
        .await
        .context("error cleaning tempdir")?;

    Ok((file_list, alg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestEnvironment;
    use docs_rs_types::testing::{KRATE, V1};

    // TODO:
    // * test with real S3 too so prefixes are handled properly

    #[tokio::test(flavor = "multi_thread")]
    async fn test_repackage_normal() -> Result<()> {
        let env = TestEnvironment::new().await?;

        const HTML_PATH: &str = "some/path.html";
        const HTML_CONTENT: &str = "<html>content</html>";
        const SOURCE_PATH: &str = "another/source.rs";
        const SOURCE_CONTENT: &str = "fn main() {}";

        let rid = env
            .fake_release()
            .await
            .name(&KRATE)
            .archive_storage(false)
            .rustdoc_file_with(HTML_PATH, HTML_CONTENT.as_bytes())
            .source_file(SOURCE_PATH, SOURCE_CONTENT.as_bytes())
            .version(V1)
            .create()
            .await?;

        let storage = env.storage()?;

        // confirm we can fetch the files via old file-based storage.
        assert_eq!(
            storage
                .stream_rustdoc_file(&KRATE, &V1, None, HTML_PATH, false)
                .await?
                .materialize(usize::MAX)
                .await?
                .content,
            HTML_CONTENT.as_bytes()
        );

        assert_eq!(
            storage
                .stream_source_file(&KRATE, &V1, None, SOURCE_PATH, false)
                .await?
                .materialize(usize::MAX)
                .await?
                .content,
            SOURCE_CONTENT.as_bytes()
        );

        // confirm the target archives really don't exist
        let rustdoc_archive = rustdoc_archive_path(&KRATE, &V1);
        let source_archive = source_archive_path(&KRATE, &V1);
        for path in &[&rustdoc_archive, &source_archive] {
            assert!(!storage.exists(path).await?);
        }
        dbg!(storage.list_prefix("").await.collect::<Vec<_>>().await);

        repackage(&storage, &KRATE, &V1).await?;

        dbg!(storage.list_prefix("").await.collect::<Vec<_>>().await);

        // afterwards it work with archives.
        assert!(
            String::from_utf8_lossy(
                &storage
                    .stream_rustdoc_file(&KRATE, &V1, None, HTML_PATH, true)
                    .await?
                    .materialize(usize::MAX)
                    .await?
                    .content
            )
            .contains(HTML_CONTENT)
        );

        // assert_eq!(
        //     &storage
        //         .stream_source_file(&KRATE, &V1, None, SOURCE_PATH, true)
        //         .await?
        //         .materialize(usize::MAX)
        //         .await?
        //         .content,
        //     SOURCE_CONTENT.as_bytes()
        // );

        // TODO: check if the release & build records were updated.

        Ok(())
    }
}
