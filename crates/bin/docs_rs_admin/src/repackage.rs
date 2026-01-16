use std::collections::HashSet;

use anyhow::Result;
use docs_rs_storage::{
    AsyncStorage, FileEntry, add_path_into_remote_archive, rustdoc_archive_path,
    source_archive_path,
};
use docs_rs_types::{CompressionAlgorithm, KrateName, ReleaseId, Version};
use docs_rs_utils::spawn_blocking;
use futures_util::StreamExt as _;
use tokio::{fs, io};

async fn repackage(
    conn: &mut sqlx::PgConnection,
    storage: &AsyncStorage,
    name: &KrateName,
    version: &Version,
) -> Result<()> {
    let rustdoc_prefix = format!("rustdoc/{name}/{version}/");
    let sources_prefix = format!("sources/{name}/{version}/");

    let mut algs: HashSet<CompressionAlgorithm> = HashSet::new();

    let mut source_size: Option<u64> = None;
    let mut documentation_size: Option<u64> = None;

    {
        let (rustdoc_file_list, alg) = repackage_path(
            storage,
            &rustdoc_prefix,
            &rustdoc_archive_path(&name, version),
        )
        .await?;

        documentation_size = Some(rustdoc_file_list.iter().map(|info| info.size).sum::<u64>());
        algs.insert(alg);
    }

    {
        let (source_file_list, alg) = repackage_path(
            storage,
            &sources_prefix,
            &source_archive_path(&name, version),
        )
        .await?;
        source_size = Some(source_file_list.iter().map(|info| info.size).sum());
        algs.insert(alg);
    }

    let rid = sqlx::query!(
        r#"SELECT id as "release_id: ReleaseId"
         FROM releases
         WHERE crate_name = $1 AND version = $2;"#,
        name as _,
        version as _,
    )
    .fetch_one(&mut *conn)
    .await?;

    sqlx::query!(
        r#"UPDATE builds AS b
           SET documentation_size = $1
           FROM (
               SELECT id
               FROM builds
               WHERE
                    rid = $2 AND
                    build_status = 'success'
               ORDER BY build_finished DESC
               LIMIT 1
         ) latest
         WHERE b.id = latest.id;
        "#,
        documentation_size.map(|s| s as i64),
        rid as _,
    )
    .execute(&mut *conn)
    .await?;

    sqlx::query!(
        r#"
        UPDATE releases
        SET source_size = $2
        WHERE id = $1;
    "#,
        rid as _,
        source_size.map(|s| s as i64),
    )
    .execute(conn)
    .await?;

    sqlx::query!("DELETE FROM compression_rels WHERE release = $1;", rid as _,)
        .execute(&mut *conn)
        .await?;

    for alg in algs {
        sqlx::query!(
            "INSERT INTO compression_rels (release, algorithm)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING;",
            rid as _,
            &(alg as i32)
        )
        .execute(&mut *conn)
        .await?;
    }

    storage.delete_prefix(&rustdoc_prefix).await?;
    storage.delete_prefix(&sources_prefix).await?;

    Ok(())
}

async fn repackage_path(
    storage: &AsyncStorage,
    prefix: &str,
    target_archive: &str,
) -> Result<(Vec<FileEntry>, CompressionAlgorithm)> {
    let tempdir = spawn_blocking(|| tempfile::tempdir().map_err(Into::into)).await?;

    // TODO: optimize: directly pack into zip , don't store locally first.

    let mut files: u64 = 0;
    let mut list = storage.list_prefix(&prefix).await;
    while let Some(entry) = list.next().await {
        let entry = entry?;
        let mut stream = storage.get_stream(&entry).await?;

        let target_path = tempdir.path().join(stream.path.trim_start_matches(&prefix));

        fs::create_dir_all(&target_path.parent().unwrap()).await?;
        {
            let mut output_file = fs::File::create(&target_path).await?;
            io::copy(&mut stream.content, &mut output_file).await?;
            output_file.sync_all().await?;
        }

        files += 1;
    }

    if files > 0 {
        let (file_list, alg) =
            add_path_into_remote_archive(storage, target_archive, &tempdir.path()).await?;

        fs::remove_dir_all(&tempdir).await?;

        Ok((file_list, alg))
    } else {
        Ok((Vec::new(), CompressionAlgorithm::Zstd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestEnvironment;
    use docs_rs_storage::{StorageKind, source_archive_path};
    use docs_rs_types::testing::{KRATE, V1};
    use pretty_assertions::assert_eq;

    // TODO:
    // * test with real S3 too so prefixes are handled properly

    #[tokio::test(flavor = "multi_thread")]
    async fn test_repackage_normal() -> Result<()> {
        let env = TestEnvironment::builder()
            .storage_config(docs_rs_storage::Config::test_config_with_kind(
                StorageKind::S3,
            )?)
            .build()
            .await?;

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

        // let rustdoc_prefix = format!("rustdoc/{KRATE}/{V1}");
        // let sources_prefix = format!("sources/{KRATE}/{V1}");

        // assert!(
        //     storage
        //         .exists(&format!("{rustdoc_prefix}/{HTML_PATH}"))
        //         .await?
        // );
        // assert!(
        //     storage
        //         .exists(&format!("{sources_prefix}/{SOURCE_PATH}"))
        //         .await?
        // );

        assert_eq!(
            storage
                .list_prefix("")
                .await
                .filter_map(|s| async { s.ok().clone() })
                .collect::<Vec<String>>()
                .await,
            vec![
                "build-logs/10000/x86_64-unknown-linux-gnu.txt",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_42.json.gz",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_42.json.zst",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_latest.json.gz",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_latest.json.zst",
                "rustdoc/krate/1.0.0/krate/index.html",
                "rustdoc/krate/1.0.0/some/path.html",
                "sources/krate/1.0.0/Cargo.toml",
                "sources/krate/1.0.0/another/source.rs",
            ]
        );

        // confirm the target archives really don't exist
        let rustdoc_archive = rustdoc_archive_path(&KRATE, &V1);
        let source_archive = source_archive_path(&KRATE, &V1);
        for path in &[&rustdoc_archive, &source_archive] {
            assert!(!storage.exists(path).await?);
        }

        repackage(&storage, &KRATE, &V1).await?;

        // afterwards it work with rustdoc archives.
        assert_eq!(
            String::from_utf8_lossy(
                &storage
                    .stream_rustdoc_file(&KRATE, &V1, None, HTML_PATH, true)
                    .await?
                    .materialize(usize::MAX)
                    .await?
                    .content
            ),
            HTML_CONTENT
        );

        // also with source archives.
        assert_eq!(
            String::from_utf8_lossy(
                &storage
                    .stream_source_file(&KRATE, &V1, None, SOURCE_PATH, true)
                    .await?
                    .materialize(usize::MAX)
                    .await?
                    .content
            ),
            SOURCE_CONTENT,
        );

        // all new files are these (`.zip`, `.zip.index`), old files are gone.
        assert_eq!(
            storage
                .list_prefix("")
                .await
                .filter_map(|s| async { s.ok().clone() })
                .collect::<Vec<String>>()
                .await,
            vec![
                "build-logs/10000/x86_64-unknown-linux-gnu.txt",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_42.json.gz",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_42.json.zst",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_latest.json.gz",
                "rustdoc-json/krate/1.0.0/x86_64-unknown-linux-gnu/krate_1.0.0_x86_64-unknown-linux-gnu_latest.json.zst",
                "rustdoc/krate/1.0.0.zip",
                "rustdoc/krate/1.0.0.zip.index",
                "sources/krate/1.0.0.zip",
                "sources/krate/1.0.0.zip.index",
            ]
        );

        Ok(())
    }
}
