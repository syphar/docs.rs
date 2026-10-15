use crate::{Config, resource};
use anyhow::Result;
use opentelemetry_otlp::{Protocol, WithExportConfig as _};
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::time::Duration;
use tokio::runtime;

/// Keep the exporter runtime alive until queued spans have been flushed.
pub struct TraceGuard {
    provider: SdkTracerProvider,
    runtime: Option<runtime::Runtime>,
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
        // Guards may drop inside async main, where blocking runtime shutdown panics.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Export traces when the shared OpenTelemetry endpoint is configured.
pub fn get_tracer_provider(config: &Config) -> Result<Option<TraceGuard>> {
    let Some(endpoint) = &config.endpoint else {
        return Ok(None);
    };

    let runtime = runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("otel-traces")
        .enable_all()
        .build()?;
    let exporter = {
        let _entered = runtime.enter();
        opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.to_string())
            .with_protocol(Protocol::Grpc)
            .with_timeout(Duration::from_secs(3))
            .build()
    };
    let exporter = match exporter {
        Ok(exporter) => exporter,
        Err(err) => {
            runtime.shutdown_background();
            return Err(err.into());
        }
    };

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        // The SDK honors OTEL_TRACES_SAMPLER[_ARG] through its defaults.
        .with_resource(resource())
        .build();

    Ok(Some(TraceGuard {
        provider,
        runtime: Some(runtime),
    }))
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

    #[test]
    fn initializes_and_shuts_down_without_an_application_runtime() {
        drop(get_tracer_provider(&config()).unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shuts_down_inside_an_application_runtime() {
        drop(get_tracer_provider(&config()).unwrap());
    }
}
