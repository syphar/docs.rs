mod build;
mod legacy;

pub use build::FakeBuild;
pub use docs_rs_registry_api::{CrateOwner, OwnerKind};
pub use legacy::{FakeGithubStats, FakeRelease, fake_release_that_failed_before_build};
