use crate::{Config, resource};
use anyhow::{Context as _, Result};
use opentelemetry_otlp::{Protocol, WithExportConfig as _};
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::time::Duration;
use tokio::runtime;

/// Flush queued spans while the application runtime is still alive.
pub struct TraceGuard {
    provider: SdkTracerProvider,
}

impl TraceGuard {
    pub fn provider(&self) -> &SdkTracerProvider {
        &self.provider
    }
}

impl Drop for TraceGuard {
    fn drop(&mut self) {
        if let Err(err) = self.provider.shutdown() {
            eprintln!("failed to shut down OpenTelemetry traces: {err}");
        }
    }
}

/// Export traces when the shared OpenTelemetry endpoint is configured.
pub fn get_tracer_provider(config: &Config) -> Result<Option<TraceGuard>> {
    let Some(endpoint) = &config.endpoint else {
        return Ok(None);
    };

    runtime::Handle::try_current()
        .context("OpenTelemetry traces exporter requires an entered Tokio runtime")?;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint.to_string())
        .with_protocol(Protocol::Grpc)
        .with_timeout(Duration::from_secs(3))
        .build()?;

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        // The SDK honors OTEL_TRACES_SAMPLER[_ARG] through its defaults.
        .with_resource(resource())
        .build();

    Ok(Some(TraceGuard { provider }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            endpoint: Some("http://127.0.0.1:4317".parse().unwrap()),
        }
    }

    #[test]
    fn disables_traces_without_an_endpoint() {
        assert!(
            get_tracer_provider(&Config { endpoint: None })
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shuts_down_inside_an_application_runtime() {
        drop(get_tracer_provider(&config()).unwrap());
    }
}
