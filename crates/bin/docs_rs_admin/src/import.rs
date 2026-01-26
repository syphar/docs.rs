use anyhow::Result;
use docs_rs_database::releases::{initialize_build, initialize_crate, initialize_release};
use docs_rs_storage::AsyncStorage;
use docs_rs_types::{KrateName, Version};

const DOCS_RS: &str = "https://docs.rs/";

pub(crate) async fn import_test_release(
    conn: &mut sqlx::PgConnection,
    storage: &AsyncStorage,
    name: &KrateName,
    version: &Version,
) -> Result<()> {
    let crate_id = initialize_crate(&mut *conn, name).await?;
    let release_id = initialize_release(&mut *conn, crate_id, version).await?;
    let build_id = initialize_build(&mut *conn, release_id).await?;

    // TODO:
    // * check rustdoc status via json
    // * delete release if exists
    // * download crate tar gz from crates.io, convert into source archive with index
    // * download rustdoc archive from docs.rs, create archive index, upload both
    // * download JSON builds from docs.rs (which?)
    // * finish_release ( try to find all info needed somewhere)
    // * finish_build ( try to find all info needed somewhere)

    todo!();
}
