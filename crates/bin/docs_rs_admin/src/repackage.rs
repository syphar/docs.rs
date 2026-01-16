use anyhow::Result;
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
    let sources_prefix = format!("sources/{name}/{version}");

    repackage_path(
        storage,
        &rustdoc_prefix,
        &rustdoc_archive_path(&name, version),
    )
    .await?;

    repackage_path(
        storage,
        &sources_prefix,
        &source_archive_path(&name, version),
    )
    .await?;

    // TODO:
    // * fill in `source_size` in release? ( new metric, not in old builds )
    // * fill in documentation_size in release / build ( new metric, not in old builds)
    // * release gets file_list from sources.
    // * releases gets algs from (source, doc)

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

    let mut list = storage.list_prefix(prefix).await;

    let mut to_delete = Vec::new();

    while let Some(entry) = list.next().await {
        let entry = entry?;

        let mut stream = storage.get_stream(&entry).await?;
        to_delete.push(stream.path.clone());

        let target_path = tempdir.path().join(stream.path);

        // TODO: optimize: directly pack into zip etc.
        {
            let mut output_file = fs::File::create(&target_path).await?;
            io::copy(&mut stream.content, &mut output_file).await?;
            output_file.sync_all().await?;
        }
    }

    let (file_list, alg) =
        add_path_into_remote_archive(storage, target_archive, &tempdir.path()).await?;

    fs::remove_dir_all(&tempdir).await?;

    Ok((file_list, alg))
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO:
    // * test with real S3 too so prefixes are handled properly
}
