use crate::Config;
use anyhow::Result;
use opentelemetry_otlp::{Protocol, WithExportConfig as _};
use opentelemetry_resource_detectors::{OsResourceDetector, ProcessResourceDetector};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

/// Keep the transport runtime alive until all queued spans have been exported.
pub struct TraceGuard {
    provider: SdkTracerProvider,
    runtime: Option<Runtime>,
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
        // Logging guards also drop inside async main; dropping a Runtime there
        // would panic. Shutdown the dedicated runtime without blocking instead.
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
    // Some binaries initialize logging before creating their application runtime.
    // A separate runtime also keeps tonic exports running during blocking shutdown.
    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("otel-traces")
        .enable_all()
        .build()?;
    let provider = {
        let _entered = runtime.enter();
        opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.to_string())
            .with_protocol(Protocol::Grpc)
            .with_timeout(Duration::from_secs(3))
            .build()
            .map(|exporter| {
                SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    // The SDK honors OTEL_TRACES_SAMPLER[_ARG], OTEL_SERVICE_NAME,
                    // and OTEL_RESOURCE_ATTRIBUTES through its defaults.
                    .with_resource(
                        Resource::builder()
                            .with_detector(Box::new(OsResourceDetector))
                            .with_detector(Box::new(ProcessResourceDetector))
                            .build(),
                    )
                    .build()
            })
    };
    match provider {
        Ok(provider) => Ok(Some(TraceGuard {
            provider,
            runtime: Some(runtime),
        })),
        Err(err) => {
            runtime.shutdown_background();
            Err(err.into())
        }
    }
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
