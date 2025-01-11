use super::data::{Crate, Crates, Release, Releases};
use anyhow::Result;
use docs_rs::Index;
use rayon::iter::ParallelIterator;

pub(super) fn load(index: &Index) -> Result<Crates> {
    let repo_url = index
        .repository_url()
        .unwrap_or("https://github.com/rust-lang/crates.io-index");

    let mut index = crates_index::GitIndex::with_path(index.path(), repo_url)?;
    index.update()?;
    let mut result: Crates = index
        .crates_parallel()
        .map(|krate| {
            krate.map(|krate| {
                let mut releases: Releases = krate
                    .versions()
                    .iter()
                    .map(|version| Release {
                        version: version.version().into(),
                        yanked: Some(version.is_yanked()),
                    })
                    .collect();
                releases.sort_by(|lhs, rhs| lhs.version.cmp(&rhs.version));

                Crate {
                    name: krate.name().into(),
                    releases,
                }
            })
        })
        .collect::<Result<_, _>>()?;

    result.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

    Ok(result)
}
