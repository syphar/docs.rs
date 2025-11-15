# TODO



## DONE
* pub(crate) queued_builds: IntCounter,
* pub(crate) total_builds: IntCounter,
* pub(crate) successful_builds: IntCounter,
* pub(crate) failed_builds: IntCounter,
* pub(crate) non_library_builds: IntCounter,
* pub(crate) documentation_size: prometheus::Histogram,
* pub(crate) build_time: prometheus::Histogram,
* pub(crate) cdn_invalidation_time: prometheus::HistogramVec,
* pub(crate) cdn_queue_time: prometheus::HistogramVec,
* idle_db_connections: IntGauge,
* used_db_connections: IntGauge,
* max_db_connections: IntGauge,
* pub(crate) failed_db_connections: IntCounter,
* pub(crate) uploaded_files_total: IntCounter,
* pub(crate) html_rewrite_ooms: IntCounter,
* pub(crate) im_feeling_lucky_searches: IntCounter,



## this could be log-based metrics?
* pub(crate) recent_crates: IntGaugeVec["duration"],
* pub(crate) recent_versions: IntGaugeVec["duration"],
* pub(crate) recent_platforms: IntGaugeVec["duration"],
* pub(crate) recently_accessed_releases: RecentlyAccessedReleases,

## replace web metrics?
https://github.com/ttys3/axum-otel-metrics

/// The traffic of various docs.rs routes
pub(crate) routes_visited: IntCounterVec["route"],
/// The response times of various docs.rs routes
pub(crate) response_time: HistogramVec["route"],


## covered by datadog itself

### process check
threads & open-fd could be https://docs.datadoghq.com/integrations/process/

/// The number of currently opened file descriptors
open_file_descriptors: IntGauge,
/// The number of threads being used by docs.rs
running_threads: IntGauge,

unclear if it can also support _multiple_ processes to watch on one machine,
though that's only a problem for the old server.
