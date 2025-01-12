use anyhow::{Context as _, Error, Result};
use docs_rs::context::BinContext;
use docs_rs::web::{build_axum_app, page::TemplateData, shutdown_signal};
use docs_rs::Context;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;
use tracing_log::LogTracer;

fn main() -> Result<()> {
    // set the global log::logger for backwards compatibility
    // through rustwide.
    rustwide::logging::init_with(LogTracer::new());

    let _sentry_guard = docs_rs::logging::initialize_logging();

    let axum_addr: SocketAddr = env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:3000".into())
        .parse()
        .context("Failed to parse socket address")?;

    let context = BinContext::new();
    let template_data = Arc::new(TemplateData::new(context.config()?.render_threads)?);

    info!(
        "Starting web server on `{}:{}`",
        axum_addr.ip(),
        axum_addr.port()
    );

    // initialize the storage and the repo-updater in sync context
    // so it can stay sync for now and doesn't fail when they would
    // be initialized while starting the server below.
    context.storage()?;
    context.repository_stats_updater()?;

    context.runtime()?.block_on(async {
        let app = build_axum_app(&context, template_data)
            .await?
            .into_make_service();
        let listener = tokio::net::TcpListener::bind(axum_addr)
            .await
            .context("error binding socket for metrics web server")?;

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        Ok::<(), Error>(())
    })?;

    Ok(())
}
