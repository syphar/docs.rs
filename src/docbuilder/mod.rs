mod limits;
mod rustwide_builder;

pub(crate) use self::limits::Limits;
pub(crate) use self::rustwide_builder::DocCoverage;
pub use self::rustwide_builder::{
    BuildPackageSummary, PackageKind, RUSTDOC_JSON_COMPRESSION_ALGORITHMS, RustwideBuilder,
};
