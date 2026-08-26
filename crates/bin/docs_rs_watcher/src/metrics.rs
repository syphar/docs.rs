use docs_rs_opentelemetry::AnyMeterProvider;
use opentelemetry::{
    KeyValue,
    metrics::{Counter, Histogram},
};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub(crate) enum EventSource {
    Git,
    Sqs,
}

impl EventSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Sqs => "sqs",
        }
    }
}

#[derive(Debug)]
pub(crate) struct WatcherMetrics {
    events_received_total: Counter<u64>,
    poll_errors_total: Counter<u64>,
    processing_errors_total: Counter<u64>,
    changes_applied_total: Counter<u64>,
    event_processing_time: Histogram<f64>,
    event_lag: Histogram<f64>,
}

impl WatcherMetrics {
    pub(crate) fn new(meter_provider: &AnyMeterProvider) -> Self {
        let meter = meter_provider.meter("watcher");
        const PREFIX: &str = "docsrs.watcher";
        Self {
            events_received_total: meter
                .u64_counter(format!("{PREFIX}.events_received_total"))
                .with_unit("1")
                .build(),
            poll_errors_total: meter
                .u64_counter(format!("{PREFIX}.poll_errors_total"))
                .with_unit("1")
                .build(),
            processing_errors_total: meter
                .u64_counter(format!("{PREFIX}.processing_errors_total"))
                .with_unit("1")
                .build(),
            changes_applied_total: meter
                .u64_counter(format!("{PREFIX}.changes_applied_total"))
                .with_unit("1")
                .build(),
            event_processing_time: meter
                .f64_histogram(format!("{PREFIX}.event_processing_time"))
                .with_boundaries(vec![
                    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
                    45.0, 55.0, 60.0, 65.0, 90.0, 120.0,
                ])
                .with_unit("s")
                .build(),
            event_lag: meter
                .f64_histogram(format!("{PREFIX}.event_lag"))
                .with_boundaries(vec![
                    0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0, 3600.0,
                ])
                .with_unit("s")
                .build(),
        }
    }

    pub(crate) fn record_change_applied(&self, source: EventSource, kind: &'static str) {
        self.changes_applied_total.add(
            1,
            &[
                KeyValue::new("source", source.as_str()),
                KeyValue::new("type", kind),
            ],
        );
    }

    pub(crate) fn record_event_processing_time(
        &self,
        source: EventSource,
        kind: &'static str,
        success: bool,
        duration: Duration,
    ) {
        let result = if success { "ok" } else { "err" };
        self.event_processing_time.record(
            duration.as_secs_f64(),
            &[
                KeyValue::new("source", source.as_str()),
                KeyValue::new("type", kind),
                KeyValue::new("result", result),
            ],
        );
    }

    pub(crate) fn record_processing_error(&self, source: EventSource, kind: &'static str) {
        self.processing_errors_total.add(
            1,
            &[
                KeyValue::new("source", source.as_str()),
                KeyValue::new("type", kind),
            ],
        );
    }

    pub(crate) fn record_events_received(&self, source: EventSource, count: usize) {
        metrics
            .events_received_total
            .add(count as u64, &[KeyValue::new("source", source.as_str())]);
    }
}
