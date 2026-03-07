use crate::{Config, handlers::build_axum_app, page::TemplateData};
use anyhow::{Context as _, Error, Result};
use docs_rs_context::Context;
use docs_rs_storage::AsyncStorage;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, signal, time};
use tracing::{error, info, instrument};

const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 3000);

#[instrument(skip_all)]
pub async fn run_web_server(
    addr: Option<SocketAddr>,
    config: Arc<Config>,
    context: Arc<Context>,
) -> Result<(), Error> {
    let template_data = Arc::new(TemplateData::new(config.render_threads)?);

    let axum_addr = addr.unwrap_or(DEFAULT_BIND);

    tracing::info!(
        "Starting web server on `{}:{}`",
        axum_addr.ip(),
        axum_addr.port()
    );

    let app = build_axum_app(config, context, template_data)
        .await?
        .into_make_service();
    let listener = TcpListener::bind(axum_addr)
        .await
        .context("error binding socket for web server")?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("signal received, starting graceful shutdown");
}

/// prunes the local cache directories for the webserver
///
/// This runs every hour, but also directly after webserver startup.
async fn background_cache_cleaner(storage: Arc<AsyncStorage>) -> ! {
    time::sleep(Duration::from_secs(60)).await;
    if let Err(err) = storage.prune_archive_index_cache().await {
        error!(
            ?err,
            "error running the initial archive index cache pruning"
        );
    }

    loop {
        time::sleep(Duration::from_secs(3600)).await;
        if let Err(err) = storage.prune_archive_index_cache().await {
            error!(
                ?err,
                "error running the initial archive index cache pruning"
            );
        }
    }
}
