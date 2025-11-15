use opentelemetry::{
    InstrumentationScope,
    metrics::{InstrumentProvider, Meter, MeterProvider},
};
use std::sync::Arc;

/// A no-op instance of a `MetricProvider`
/// for now, copy/past from opentelemetry-sdk
/// see
/// https://github.com/open-telemetry/opentelemetry-rust/pull/3111
#[derive(Debug, Default)]
pub struct NoopMeterProvider {
    _private: (),
}

impl NoopMeterProvider {
    /// Create a new no-op meter provider.
    pub fn new() -> Self {
        NoopMeterProvider { _private: () }
    }
}

impl MeterProvider for NoopMeterProvider {
    fn meter_with_scope(&self, _scope: InstrumentationScope) -> Meter {
        Meter::new(Arc::new(NoopMeter::new()))
    }
}

/// A no-op instance of a `Meter`
#[derive(Debug, Default)]
pub(crate) struct NoopMeter {
    _private: (),
}

impl NoopMeter {
    /// Create a new no-op meter core.
    pub(crate) fn new() -> Self {
        NoopMeter { _private: () }
    }
}

impl InstrumentProvider for NoopMeter {}
