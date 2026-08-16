//! Prototype MCP server.
#![recursion_limit = "256"]
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::sync::Arc;

use whisply_arg0::Arg0DispatchPaths;
use whisply_core::config::ConfigBuilder;
use whisply_core::resolve_installation_id;
use whisply_exec_server::EnvironmentManager;
use whisply_exec_server::ExecServerRuntimePaths;
use whisply_login::default_client::set_default_client_residency_requirement;
use whisply_utils_cli::CliConfigOverrides;

use rmcp::model::ClientNotification;
use rmcp::model::ClientRequest;
use rmcp::model::JsonRpcMessage;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::io::{self};
use tokio::sync::mpsc;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod active_turn_registry;
mod codex_tool_config;
mod codex_tool_runner;
mod exec_approval;
mod extension_event_sink;
pub(crate) mod message_processor;
mod outgoing_message;
mod patch_approval;

use crate::message_processor::MessageProcessor;
use crate::outgoing_message::OutgoingJsonRpcMessage;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;

pub use crate::codex_tool_config::CodexToolCallParam;
pub use crate::codex_tool_config::CodexToolCallReplyParam;
pub use crate::exec_approval::ExecApprovalElicitRequestParams;
pub use crate::exec_approval::ExecApprovalResponse;
pub use crate::patch_approval::PatchApprovalElicitRequestParams;
pub use crate::patch_approval::PatchApprovalResponse;

/// Size of the bounded channels used to communicate between tasks. The value
/// is a balance between throughput and memory usage – 128 messages should be
/// plenty for an interactive CLI.
const CHANNEL_CAPACITY: usize = 128;
const DEFAULT_ANALYTICS_ENABLED: bool = true;
const OTEL_SERVICE_NAME: &str = "codex_mcp_server";

type IncomingMessage = JsonRpcMessage<ClientRequest, Value, ClientNotification>;

pub async fn run_main(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
    strict_config: bool,
) -> IoResult<()> {
    // Parse CLI overrides once and derive the base Config eagerly so later
    // components do not need to work with raw TOML values.
    let cli_kv_overrides = cli_config_overrides.parse_overrides().map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("error parsing -c overrides: {e}"),
        )
    })?;
    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .strict_config(strict_config)
        .build()
        .await
        .map_err(|e| {
            std::io::Error::new(ErrorKind::InvalidData, format!("error loading config: {e}"))
        })?;
    config.auth_config().validate()?;
    set_default_client_residency_requirement(config.enforce_residency.value());
    let otel = whisply_core::otel_init::build_provider(
        &config,
        env!("CARGO_PKG_VERSION"),
        Some(OTEL_SERVICE_NAME),
        DEFAULT_ANALYTICS_ENABLED,
    )
    .map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("error loading otel config: {e}"),
        )
    })?;
    whisply_core::otel_init::record_process_start(otel.as_ref(), OTEL_SERVICE_NAME);
    whisply_core::otel_init::install_sqlite_telemetry(otel.as_ref(), OTEL_SERVICE_NAME);
    let state_db = whisply_core::init_state_db(&config).await;
    let environment_manager = Arc::new(
        EnvironmentManager::from_codex_home(
            config.codex_home.clone(),
            Some(ExecServerRuntimePaths::from_optional_paths(
                arg0_paths.codex_self_exe.clone(),
                arg0_paths.codex_linux_sandbox_exe.clone(),
            )?),
            config.http_client_factory(),
        )
        .await
        .map_err(std::io::Error::other)?,
    );

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::from_default_env());
    let otel_logger_layer = otel.as_ref().and_then(|provider| provider.logger_layer());
    let otel_tracing_layer = otel.as_ref().and_then(|provider| provider.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    // Set up channels.
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingMessage>(CHANNEL_CAPACITY);
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
    let installation_id = resolve_installation_id(&config.codex_home).await?;

    // Task: read from stdin, push to `incoming_tx`.
    let stdin_reader_handle = tokio::spawn({
        async move {
            let stdin = io::stdin();
            let reader = BufReader::new(stdin);
            let mut lines = reader.lines();

            while let Some(line) = lines.next_line().await.unwrap_or_default() {
                match serde_json::from_str::<IncomingMessage>(&line) {
                    Ok(msg) => {
                        if incoming_tx.send(msg).await.is_err() {
                            // Receiver gone – nothing left to do.
                            break;
                        }
                    }
                    Err(_) => error!("failed to deserialize JSON-RPC message"),
                }
            }

            debug!("stdin reader finished (EOF)");
        }
    });

    // Task: process incoming messages.
    let processor_handle = tokio::spawn({
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);
        let mut processor = MessageProcessor::new(
            outgoing_message_sender,
            arg0_paths,
            Arc::new(config),
            environment_manager,
            state_db,
            installation_id,
        )
        .await;
        async move {
            while let Some(msg) = incoming_rx.recv().await {
                match msg {
                    JsonRpcMessage::Request(r) => processor.process_request(r).await,
                    JsonRpcMessage::Response(r) => processor.process_response(r).await,
                    JsonRpcMessage::Notification(n) => processor.process_notification(n).await,
                    JsonRpcMessage::Error(e) => processor.process_error(e),
                }
            }

            info!("processor task exited (channel closed)");
        }
    });

    // Task: write outgoing messages to stdout.
    let stdout_writer_handle = tokio::spawn(async move {
        let mut stdout = io::stdout();
        while let Some(outgoing_message) = outgoing_rx.recv().await {
            let msg: OutgoingJsonRpcMessage = outgoing_message.into();
            match serde_json::to_string(&msg) {
                Ok(json) => {
                    if stdout.write_all(json.as_bytes()).await.is_err() {
                        error!("failed to write JSON-RPC message to stdout");
                        break;
                    }
                    if stdout.write_all(b"\n").await.is_err() {
                        error!("failed to write JSON-RPC newline to stdout");
                        break;
                    }
                }
                Err(_) => error!("failed to serialize JSON-RPC message"),
            }
        }

        info!("stdout writer exited (channel closed)");
    });

    // Wait for all tasks to finish.  The typical exit path is the stdin reader
    // hitting EOF which, once it drops `incoming_tx`, propagates shutdown to
    // the processor and then to the stdout task.
    let _ = tokio::join!(stdin_reader_handle, processor_handle, stdout_writer_handle);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use whisply_config::types::OtelExporterKind;
    use whisply_core::config::ConfigBuilder;

    #[tokio::test]
    async fn mcp_server_disables_otel_exporters_even_when_configured() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let mut config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await?;
        let exporter = OtelExporterKind::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            headers: HashMap::new(),
            tls: None,
        };
        config.otel.exporter = exporter.clone();
        config.otel.trace_exporter = exporter.clone();
        config.otel.metrics_exporter = exporter;
        config.analytics_enabled = None;

        let provider = whisply_core::otel_init::build_provider(
            &config,
            "0.0.0-test",
            Some(OTEL_SERVICE_NAME),
            DEFAULT_ANALYTICS_ENABLED,
        )
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

        assert!(
            provider.is_none(),
            "BrokerOnly must suppress direct OTEL exporters even when a caller mutates Config"
        );

        Ok(())
    }
}
