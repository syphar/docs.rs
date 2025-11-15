# TODO


pub(crate) cdn_invalidation_time: prometheus::HistogramVec,
pub(crate) cdn_queue_time: prometheus::HistogramVec,

/// The number of idle database connections
idle_db_connections: IntGauge,
/// The number of used database connections
used_db_connections: IntGauge,
/// The maximum number of database connections
max_db_connections: IntGauge,
/// Number of attempted and failed connections to the database
pub(crate) failed_db_connections: IntCounter,

/// The number of currently opened file descriptors
open_file_descriptors: IntGauge,
/// The number of threads being used by docs.rs
running_threads: IntGauge,

/// The traffic of various docs.rs routes
pub(crate) routes_visited: IntCounterVec["route"],
/// The response times of various docs.rs routes
pub(crate) response_time: HistogramVec["route"],


/// Number of files uploaded to the storage backend
pub(crate) uploaded_files_total: IntCounter,

/// The number of attempted files that failed due to a memory limit
pub(crate) html_rewrite_ooms: IntCounter,

/// the number of "I'm feeling lucky" searches for crates
pub(crate) im_feeling_lucky_searches: IntCounter,


## DONE
* pub(crate) queued_builds: IntCounter,
* pub(crate) total_builds: IntCounter,
* pub(crate) successful_builds: IntCounter,
* pub(crate) failed_builds: IntCounter,
* pub(crate) non_library_builds: IntCounter,
* pub(crate) documentation_size: prometheus::Histogram,
* pub(crate) build_time: prometheus::Histogram,

## this could be log-based metrics?
* pub(crate) recent_crates: IntGaugeVec["duration"],
* pub(crate) recent_versions: IntGaugeVec["duration"],
* pub(crate) recent_platforms: IntGaugeVec["duration"],
* pub(crate) recently_accessed_releases: RecentlyAccessedReleases,
