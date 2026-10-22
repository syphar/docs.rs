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

pub async fn init_from_environment() -> anyhow::Result<Guard> {
    init_with_config(
        &Config::from_environment()?,
        &docs_rs_opentelemetry::Config::from_environment()?,
    )
    .await
}

pub async fn init_with_config(
    config: &Config,
    opentelemetry_config: &docs_rs_opentelemetry::Config,
) -> anyhow::Result<Guard> {
    let log_formatter = match config.format {
        LogFormat::Full => tracing_subscriber::fmt::layer().boxed(),
        LogFormat::Compact => tracing_subscriber::fmt::layer().compact().boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer().pretty().boxed(),
        LogFormat::Json => tracing_subscriber::fmt::layer().json().boxed(),
    };

    let trace_guard = docs_rs_opentelemetry::get_tracer_provider(opentelemetry_config).await?;
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
