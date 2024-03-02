use std::{env, str::FromStr};
use tracing_subscriber::{filter::Directive, prelude::*, EnvFilter};

pub fn initialize() -> Option<sentry::ClientInitGuard> {
    let tracing_registry = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            EnvFilter::builder()
                .with_default_directive(Directive::from_str("docs_rs=info").unwrap())
                .with_env_var("DOCSRS_LOG")
                .from_env_lossy(),
        );

    if let Ok(sentry_dsn) = env::var("SENTRY_DSN") {
        tracing::subscriber::set_global_default(tracing_registry.with(
            sentry_tracing::layer().event_filter(|md| {
                if md.fields().field("reported_to_sentry").is_some() {
                    sentry_tracing::EventFilter::Ignore
                } else {
                    sentry_tracing::default_event_filter(md)
                }
            }),
        ))
        .unwrap();

        Some(sentry::init((
            sentry_dsn,
            sentry::ClientOptions {
                release: Some(crate::BUILD_VERSION.into()),
                attach_stacktrace: true,
                traces_sample_rate: env::var("SENTRY_TRACES_SAMPLE_RATE")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0),
                ..Default::default()
            }
            .add_integration(sentry_panic::PanicIntegration::default()),
        )))
    } else {
        tracing::subscriber::set_global_default(tracing_registry).unwrap();
        None
    }
}
