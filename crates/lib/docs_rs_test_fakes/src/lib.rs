<<<<<<< HEAD
mod build;
=======
mod github_stats;
>>>>>>> main
mod legacy;

pub use build::FakeBuild;
pub use docs_rs_registry_api::{CrateOwner, OwnerKind};
<<<<<<< HEAD
pub use legacy::{FakeGithubStats, FakeRelease, fake_release_that_failed_before_build};
=======
pub use github_stats::FakeGithubStats;
pub use legacy::{FakeBuild, FakeRelease, fake_release_that_failed_before_build};
>>>>>>> main
