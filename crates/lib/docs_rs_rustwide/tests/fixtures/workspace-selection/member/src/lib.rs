#[cfg(not(feature = "member-docs"))]
compile_error!("the selected package's docs.rs features must be enabled");

/// A type provided by an unpublished sibling dependency.
pub use workspace_sibling::LocalDependency;
