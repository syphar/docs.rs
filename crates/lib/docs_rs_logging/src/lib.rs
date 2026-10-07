mod config;
pub mod log_format;
#[cfg(feature = "testing")]
pub mod testing;

pub use config::Config;
pub use log_format::LogFormat;

use docs_rs_config::AppConfig as _;
use opentelemetry::trace::TracerProvider as _;
use sentry::{
    TransactionContext, integrations::panic as sentry_panic,
    integrations::tracing as sentry_tracing,
};
use tracing_subscriber::prelude::*;

/// defines the transaction name to be used for our rustwide builder.
///
/// We want to trace _all_ builds, while we want to apply a
/// sampling ratio to web requests.
///
/// From what I see right now, the transaction name or op is the only way
/// to distinguish build transactions from web requests.
pub const BUILD_PACKAGE_TRANSACTION_NAME: &str = "docbuilder.build_package";

pub struct Guard {
    #[allow(dead_code)]
    sentry_guard: Option<sentry::ClientInitGuard>,
    #[allow(dead_code)]
    trace_guard: Option<docs_rs_opentelemetry::TraceGuard>,
}

pub fn init_from_environment() -> anyhow::Result<Guard> {
    init_with_config(&Config::from_environment()?)
}

pub fn init_with_config(config: &Config) -> anyhow::Result<Guard> {
    let log_formatter = match config.format {
        LogFormat::Full => tracing_subscriber::fmt::layer().boxed(),
        LogFormat::Compact => tracing_subscriber::fmt::layer().compact().boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer().pretty().boxed(),
        LogFormat::Json => tracing_subscriber::fmt::layer().json().boxed(),
    };

    let trace_guard = config
        .opentelemetry
        .as_ref()
        .map(docs_rs_opentelemetry::get_tracer_provider)
        .transpose()?;
    let otel_layer = trace_guard.as_ref().map(|guard| {
        tracing_opentelemetry::layer().with_tracer(guard.provider().tracer("docs_rs"))
    });

    let tracing_registry = tracing_subscriber::registry()
        .with(otel_layer)
        .with(log_formatter)
        .with(config.filter.clone());

    let sentry_guard = if let Some(sentry_config) = &config.sentry {
        tracing::subscriber::set_global_default(
            tracing_registry.with(
                sentry_tracing::layer()
                    // Include HTTP trace-span fields (route, path, and response status) on child
                    // error events, making failures reported by tower-http actionable in Sentry.
                    .enable_span_attributes()
                    .event_filter(|md| {
                        if md.fields().field("reported_to_sentry").is_some() {
                            sentry_tracing::EventFilter::Ignore
                        } else {
                            sentry_tracing::default_event_filter(md)
                        }
                    }),
            ),
        )?;

        let sample_rate = sentry_config.traces_sample_rate;
        let traces_sampler = move |ctx: &TransactionContext| -> f32 {
            if let Some(sampled) = ctx.sampled() {
                // if the transaction was already marked as "to be sampled" by
                // the JS/frontend SDK, we want to sample it in the backend too.
                return if sampled { 1.0 } else { 0.0 };
            }

            if ctx.name() == BUILD_PACKAGE_TRANSACTION_NAME {
                // record all transactions for builds
                1.
            } else {
                sample_rate
            }
        };

        Some(sentry::init((
            sentry_config.dsn.clone(),
            sentry::ClientOptions::new()
                .release(docs_rs_utils::BUILD_VERSION)
                .attach_stacktrace(true)
                .traces_sampler(traces_sampler)
                .add_integration(sentry_panic::PanicIntegration::default()),
        )))
    } else {
        tracing::subscriber::set_global_default(tracing_registry)?;
        None
    };

    Ok(Guard {
        sentry_guard,
        trace_guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::{Value, trace::SpanKind};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider};

    #[test]
    fn exports_parent_child_spans_and_updated_fields_alongside_sentry() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")))
            .with(sentry_tracing::layer().enable_span_attributes());

        tracing::subscriber::with_default(subscriber, || {
            let request = tracing::info_span!(
                "http.request",
                otel.name = "GET /{name}",
                otel.kind = "server",
                http.response.status_code = tracing::field::Empty
            );
            let _entered = request.enter();
            {
                let child = tracing::info_span!("archive_index_copy", bytes = 1024_i64);
                let _entered = child.enter();
            }
            request.record("http.response.status_code", 200_i64);
        });
        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans.len(), 2);
        let request = spans
            .iter()
            .find(|span| span.name == "GET /{name}")
            .unwrap();
        let child = spans
            .iter()
            .find(|span| span.name == "archive_index_copy")
            .unwrap();
        assert_eq!(request.span_kind, SpanKind::Server);
        assert_eq!(child.parent_span_id, request.span_context.span_id());
        assert_eq!(
            child.span_context.trace_id(),
            request.span_context.trace_id()
        );
        assert!(
            request
                .attributes
                .iter()
                .any(
                    |attribute| attribute.key.as_str() == "http.response.status_code"
                        && attribute.value == Value::I64(200)
                )
        );
        assert!(
            child
                .attributes
                .iter()
                .any(|attribute| attribute.key.as_str() == "bytes"
                    && attribute.value == Value::I64(1024))
        );
        provider.shutdown().unwrap();
    }
}
