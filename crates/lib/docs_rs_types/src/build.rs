#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Build {
    id: BuildId,
    rustc_version: Option<String>,
    docsrs_version: Option<String>,
    build_status: BuildStatus,
    build_time: Option<DateTime<Utc>>,
    errors: Option<String>,
}
