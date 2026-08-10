use crate::config::Config;
use codex_features::Feature;
use codex_login::default_client::originator;
use codex_otel::OtelExporter;
use codex_otel::OtelProvider;
use codex_otel::OtelSettings;
use std::error::Error;

/// Build an OpenTelemetry provider from the app Config.
///
/// Returns `None` when OTEL export is disabled.
pub fn build_provider(
    config: &Config,
    service_version: &str,
    service_name_override: Option<&str>,
    _default_analytics_enabled: bool,
) -> Result<Option<OtelProvider>, Box<dyn Error>> {
    // Config validation rejects user-supplied exporters. Keep this final
    // entrypoint guard as well, so callers holding a programmatically-mutated
    // Config still cannot regain direct OTLP or Statsig authority.
    let exporter = OtelExporter::None;
    let trace_exporter = OtelExporter::None;
    let metrics_exporter = OtelExporter::None;

    let originator = originator();
    let service_name = service_name_override.unwrap_or(originator.value.as_str());
    let runtime_metrics = config.features.enabled(Feature::RuntimeMetrics);

    OtelProvider::from(&OtelSettings {
        service_name: service_name.to_string(),
        service_version: service_version.to_string(),
        codex_home: config.codex_home.to_path_buf(),
        environment: config.otel.environment.to_string(),
        exporter,
        trace_exporter,
        metrics_exporter,
        runtime_metrics,
        span_attributes: config.otel.span_attributes.clone(),
        tracestate: config.otel.tracestate.clone(),
    })
}

pub fn record_process_start(otel: Option<&OtelProvider>, originator: &str) {
    let Some(metrics) = otel.and_then(OtelProvider::metrics) else {
        return;
    };
    let _ = codex_otel::record_process_start_once(metrics, originator);
}

pub fn install_sqlite_telemetry(otel: Option<&OtelProvider>, originator: &str) {
    let Some(metrics) = otel.and_then(OtelProvider::metrics) else {
        return;
    };
    let telemetry = codex_rollout::sqlite_telemetry_recorder(metrics.clone(), originator);
    let _ = codex_state::install_process_db_telemetry(telemetry);
}
