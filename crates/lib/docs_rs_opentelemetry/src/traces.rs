use anyhow::{Context as _, Result, bail};
use docs_rs_env_vars::{env, maybe_env};
use opentelemetry_otlp::{Protocol, WithExportConfig as _};
use opentelemetry_resource_detectors::{OsResourceDetector, ProcessResourceDetector};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use url::Url;

#[derive(Debug)]
pub struct TraceConfig {
    pub endpoint: Url,
}

impl TraceConfig {
    /// Traces are opt-in, independently of the existing metrics exporter.
    pub fn from_environment() -> Result<Option<Self>> {
        match env("OTEL_TRACES_EXPORTER", "none".to_string())?.as_str() {
            "none" => Ok(None),
            "otlp" => {
                let endpoint = maybe_env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")?
                    .or(maybe_env("OTEL_EXPORTER_OTLP_ENDPOINT")?)
                    .context("OTLP traces require OTEL_EXPORTER_OTLP_ENDPOINT or OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")?;
                Ok(Some(Self { endpoint }))
            }
            exporter => bail!("unsupported OTEL_TRACES_EXPORTER: {exporter}"),
        }
    }
}

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

pub fn get_tracer_provider(config: &TraceConfig) -> Result<TraceGuard> {
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
            .with_endpoint(config.endpoint.to_string())
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
        Ok(provider) => Ok(TraceGuard {
            provider,
            runtime: Some(runtime),
        }),
        Err(err) => {
            runtime.shutdown_background();
            Err(err.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TraceConfig {
        TraceConfig {
            endpoint: "http://127.0.0.1:4317".parse().unwrap(),
        }
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
