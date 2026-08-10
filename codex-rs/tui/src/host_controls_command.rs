//! Typed, non-interactive `/controls` TUI command.
//!
//! This module intentionally has no configuration, socket, or app-launch
//! fallback. It calls only the capability-authenticated native broker APIs,
//! which in turn require the already-running authenticated Mac app owner.

use codex_whisply::BrokerError;
use codex_whisply::BrokerErrorCode;
use codex_whisply::HostControlsAudioAssistMode;
use codex_whisply::HostControlsBuiltinCapability;
use codex_whisply::HostControlsCommand;
use codex_whisply::HostControlsComputerUseRoute;
use codex_whisply::HostControlsInteractiveAction;
use codex_whisply::HostControlsPatch;
use codex_whisply::HostControlsPermissionMode;
use codex_whisply::HostControlsThreadDirectory;
use codex_whisply::NativeBrokerClient;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) const HOST_CONTROLS_USAGE: &str = "Usage: /controls [status [product-session <uuid>] | master <on|off> | route <native-apps|existing-browser|spawned-browser|local-files> <on|off> | builtin <browser|chrome|computer-use|documents|pdf|spreadsheets|presentations> <on|off> | thread product-session <uuid> <directory <absolute-path|none>|permission <ask|auto|full-access>|prefer-screen <on|off>> | audio <mode <manual|automatic>|suggestions <on|off>> | stop product-session <uuid> | request <accessibility|screen-recording|choose-directory|choose-app|choose-window|system-settings>]\nRun /controls status to view the active Mac product-session UUID, then pass it explicitly for thread and Stop controls.";

/// A fixed projection of ordinary, overlay-equivalent host controls. There is
/// deliberately no Arm, Exam, invisibility, or Undetected case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostControlsTuiCommand {
    Snapshot {
        session_id: Option<String>,
    },
    Apply {
        snapshot_session_id: Option<String>,
        patch: HostControlsPatch,
    },
    Execute {
        command: HostControlsCommand,
    },
}

impl HostControlsTuiCommand {
    pub(crate) fn parse(args: &str) -> Result<Self, String> {
        let words = shlex::split(args).ok_or_else(|| HOST_CONTROLS_USAGE.to_string())?;
        if words.is_empty() || matches!(words.as_slice(), [command] if command == "status") {
            return Ok(Self::Snapshot { session_id: None });
        }

        match words.as_slice() {
            [command, qualifier, session_id]
                if command == "status" && qualifier == "product-session" =>
            {
                Ok(Self::Snapshot {
                    session_id: Some(parse_product_session_id(session_id)?),
                })
            }
            [command, enabled] if command == "master" => Ok(Self::Apply {
                snapshot_session_id: None,
                patch: HostControlsPatch::ComputerUseMaster {
                    enabled: parse_enabled(enabled)?,
                },
            }),
            [command, route, enabled] if command == "route" => Ok(Self::Apply {
                snapshot_session_id: None,
                patch: HostControlsPatch::ComputerUseRoute {
                    route: parse_route(route)?,
                    enabled: parse_enabled(enabled)?,
                },
            }),
            [command, capability, enabled] if command == "builtin" => Ok(Self::Apply {
                snapshot_session_id: None,
                patch: HostControlsPatch::BuiltinCapability {
                    capability: parse_builtin(capability)?,
                    enabled: parse_enabled(enabled)?,
                },
            }),
            [command, qualifier, session, subcommand, value]
                if command == "thread"
                    && qualifier == "product-session"
                    && subcommand == "directory" =>
            {
                let session_id = parse_product_session_id(session)?;
                let directory = if value == "none" {
                    HostControlsThreadDirectory::NoDirectory
                } else {
                    validate_absolute_directory(value)?;
                    HostControlsThreadDirectory::Directory {
                        canonical_path: value.to_string(),
                    }
                };
                Ok(Self::Apply {
                    snapshot_session_id: Some(session_id.clone()),
                    patch: HostControlsPatch::Thread {
                        session_id,
                        directory: Some(directory),
                        permission_mode: None,
                        prefer_screen: None,
                    },
                })
            }
            [command, qualifier, session, subcommand, value]
                if command == "thread"
                    && qualifier == "product-session"
                    && subcommand == "permission" =>
            {
                let session_id = parse_product_session_id(session)?;
                Ok(Self::Apply {
                    snapshot_session_id: Some(session_id.clone()),
                    patch: HostControlsPatch::Thread {
                        session_id,
                        directory: None,
                        permission_mode: Some(parse_permission(value)?),
                        prefer_screen: None,
                    },
                })
            }
            [command, qualifier, session, subcommand, value]
                if command == "thread"
                    && qualifier == "product-session"
                    && subcommand == "prefer-screen" =>
            {
                let session_id = parse_product_session_id(session)?;
                Ok(Self::Apply {
                    snapshot_session_id: Some(session_id.clone()),
                    patch: HostControlsPatch::Thread {
                        session_id,
                        directory: None,
                        permission_mode: None,
                        prefer_screen: Some(parse_enabled(value)?),
                    },
                })
            }
            [command, subcommand, value] if command == "audio" && subcommand == "mode" => {
                Ok(Self::Apply {
                    snapshot_session_id: None,
                    patch: HostControlsPatch::AudioAssist {
                        mode: Some(parse_audio_mode(value)?),
                        transcript_task_suggestions_enabled: None,
                    },
                })
            }
            [command, subcommand, value] if command == "audio" && subcommand == "suggestions" => {
                Ok(Self::Apply {
                    snapshot_session_id: None,
                    patch: HostControlsPatch::AudioAssist {
                        mode: None,
                        transcript_task_suggestions_enabled: Some(parse_enabled(value)?),
                    },
                })
            }
            [command, qualifier, session]
                if command == "stop" && qualifier == "product-session" =>
            {
                Ok(Self::Execute {
                    command: HostControlsCommand::StopComputerUse {
                        session_id: parse_product_session_id(session)?,
                    },
                })
            }
            [command, action] if command == "request" => Ok(Self::Execute {
                command: HostControlsCommand::RequestInteractive {
                    action: parse_interactive_action(action)?,
                },
            }),
            _ => Err(HOST_CONTROLS_USAGE.to_string()),
        }
    }
}

/// Performs the one fixed broker operation. All failure paths intentionally
/// collapse a missing/expired app owner into a generic unavailable response.
pub(crate) fn execute_host_controls_command(
    broker: &NativeBrokerClient,
    command: HostControlsTuiCommand,
) -> Result<String, String> {
    broker.hello().map_err(broker_error_message)?;
    let status = broker.status().map_err(broker_error_message)?;
    if !status.authenticated {
        return Err("Sign in is required before using Whisply controls.".to_string());
    }
    let account_epoch = status.account_epoch;

    let value = match command {
        HostControlsTuiCommand::Snapshot { session_id } => broker
            .host_controls_snapshot(account_epoch.as_deref(), session_id.as_deref())
            .map_err(broker_error_message)?,
        HostControlsTuiCommand::Apply {
            snapshot_session_id,
            patch,
        } => {
            let snapshot = broker
                .host_controls_snapshot(account_epoch.as_deref(), snapshot_session_id.as_deref())
                .map_err(broker_error_message)?;
            let revision = snapshot
                .get("revision")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| unavailable_message().to_string())?;
            broker
                .host_controls_apply(account_epoch.as_deref(), revision, patch)
                .map_err(broker_error_message)?
        }
        HostControlsTuiCommand::Execute { command } => broker
            .host_controls_execute(account_epoch.as_deref(), command)
            .map_err(broker_error_message)?,
    };

    serde_json::to_string_pretty(&value).map_err(|_| unavailable_message().to_string())
}

/// Reads the inherited runtime capability exactly once. A TUI session must
/// retain this client for subsequent `/controls` commands rather than trying
/// to re-read the one-shot inherited capability descriptor.
pub(crate) fn host_controls_client_from_environment() -> Result<Arc<NativeBrokerClient>, String> {
    NativeBrokerClient::from_environment()
        .map_err(broker_error_message)?
        .ok_or_else(|| unavailable_message().to_string())
}

/// Product chat IDs are UUIDs minted by the Mac app. This parser accepts only
/// an explicit candidate; it never infers a product identity from the TUI's
/// app-server `ThreadId`. The Mac owner then verifies that the supplied value
/// is either its active chat or a current-account runtime mapping.
fn parse_product_session_id(value: &str) -> Result<String, String> {
    Uuid::parse_str(value)
        // Preserve the exact app-displayed spelling: legacy persisted
        // product-thread keys are case-sensitive even though UUID comparison
        // itself is not. `/controls status` is the authorized source.
        .map(|_| value.to_string())
        .map_err(|_| "Use the explicit product-session UUID shown by /controls status.".to_string())
}

fn validate_absolute_directory(value: &str) -> Result<(), String> {
    if value.len() > 4_096 || !value.starts_with('/') || value.contains('\0') {
        return Err("Thread directory must be a bounded absolute path.".to_string());
    }
    Ok(())
}

fn parse_enabled(value: &str) -> Result<bool, String> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(HOST_CONTROLS_USAGE.to_string()),
    }
}

fn parse_route(value: &str) -> Result<HostControlsComputerUseRoute, String> {
    match value {
        "native-apps" => Ok(HostControlsComputerUseRoute::NativeApps),
        "existing-browser" => Ok(HostControlsComputerUseRoute::ExistingBrowser),
        "spawned-browser" => Ok(HostControlsComputerUseRoute::SpawnedBrowser),
        "local-files" => Ok(HostControlsComputerUseRoute::LocalFiles),
        _ => Err(HOST_CONTROLS_USAGE.to_string()),
    }
}

fn parse_builtin(value: &str) -> Result<HostControlsBuiltinCapability, String> {
    match value {
        "browser" => Ok(HostControlsBuiltinCapability::Browser),
        "chrome" => Ok(HostControlsBuiltinCapability::Chrome),
        "computer-use" => Ok(HostControlsBuiltinCapability::ComputerUse),
        "documents" => Ok(HostControlsBuiltinCapability::Documents),
        "pdf" => Ok(HostControlsBuiltinCapability::Pdf),
        "spreadsheets" => Ok(HostControlsBuiltinCapability::Spreadsheets),
        "presentations" => Ok(HostControlsBuiltinCapability::Presentations),
        _ => Err(HOST_CONTROLS_USAGE.to_string()),
    }
}

fn parse_permission(value: &str) -> Result<HostControlsPermissionMode, String> {
    match value {
        "ask" => Ok(HostControlsPermissionMode::Ask),
        "auto" => Ok(HostControlsPermissionMode::Auto),
        "full-access" => Ok(HostControlsPermissionMode::FullAccess),
        _ => Err(HOST_CONTROLS_USAGE.to_string()),
    }
}

fn parse_audio_mode(value: &str) -> Result<HostControlsAudioAssistMode, String> {
    match value {
        "manual" => Ok(HostControlsAudioAssistMode::Manual),
        "automatic" => Ok(HostControlsAudioAssistMode::Automatic),
        _ => Err(HOST_CONTROLS_USAGE.to_string()),
    }
}

fn parse_interactive_action(value: &str) -> Result<HostControlsInteractiveAction, String> {
    match value {
        "accessibility" => Ok(HostControlsInteractiveAction::ComputerUseAccessibilityPermission),
        "screen-recording" => {
            Ok(HostControlsInteractiveAction::ComputerUseScreenRecordingPermission)
        }
        "choose-directory" => Ok(HostControlsInteractiveAction::ChooseThreadDirectory),
        "choose-app" => Ok(HostControlsInteractiveAction::ChooseTrustedApplication),
        "choose-window" => Ok(HostControlsInteractiveAction::ChooseWindow),
        "system-settings" => Ok(HostControlsInteractiveAction::SystemSettings),
        _ => Err(HOST_CONTROLS_USAGE.to_string()),
    }
}

fn unavailable_message() -> &'static str {
    "Whisply controls are unavailable. Keep the Whisply app running and try again."
}

fn broker_error_message(error: BrokerError) -> String {
    match error {
        BrokerError::Rejected(
            BrokerErrorCode::Unauthenticated | BrokerErrorCode::LoginRequired,
        ) => "Sign in is required before using Whisply controls.".to_string(),
        _ => unavailable_message().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_parser_only_represents_ordinary_controls() {
        assert!(matches!(
            HostControlsTuiCommand::parse("master on"),
            Ok(HostControlsTuiCommand::Apply {
                patch: HostControlsPatch::ComputerUseMaster { enabled: true },
                ..
            })
        ));
        assert!(matches!(
            HostControlsTuiCommand::parse("audio mode automatic"),
            Ok(HostControlsTuiCommand::Apply {
                patch: HostControlsPatch::AudioAssist {
                    mode: Some(HostControlsAudioAssistMode::Automatic),
                    ..
                },
                ..
            })
        ));
        for overlay_only in ["arm", "exam", "invisible", "undetected"] {
            assert!(HostControlsTuiCommand::parse(overlay_only).is_err());
        }
    }

    #[test]
    fn session_qualified_controls_require_an_explicit_product_session() {
        let product_session = "aac37a2d-7d37-4f70-b03c-f652516c0088";
        assert!(HostControlsTuiCommand::parse("stop").is_err());
        assert!(HostControlsTuiCommand::parse("stop runtime-thread-a").is_err());
        assert!(HostControlsTuiCommand::parse("stop product-session runtime-thread-a").is_err());
        assert!(matches!(
            HostControlsTuiCommand::parse(&format!("stop product-session {product_session}")),
            Ok(HostControlsTuiCommand::Execute {
                command: HostControlsCommand::StopComputerUse { session_id },
            }) if session_id == product_session
        ));
    }

    #[test]
    fn thread_directory_requires_an_absolute_path() {
        let product_session = "aac37a2d-7d37-4f70-b03c-f652516c0088";
        assert!(HostControlsTuiCommand::parse("thread directory relative/path").is_err());
        assert!(matches!(
            HostControlsTuiCommand::parse(&format!(
                "thread product-session {product_session} directory '/tmp/with spaces'"
            )),
            Ok(HostControlsTuiCommand::Apply {
                patch: HostControlsPatch::Thread {
                    session_id,
                    directory: Some(HostControlsThreadDirectory::Directory { canonical_path }),
                    ..
                },
                ..
            }) if session_id == product_session && canonical_path == "/tmp/with spaces"
        ));
    }

    #[test]
    fn status_can_project_only_an_explicit_product_session() {
        let product_session = "aac37a2d-7d37-4f70-b03c-f652516c0088";
        assert!(matches!(
            HostControlsTuiCommand::parse("status"),
            Ok(HostControlsTuiCommand::Snapshot { session_id: None })
        ));
        assert!(matches!(
            HostControlsTuiCommand::parse(&format!("status product-session {product_session}")),
            Ok(HostControlsTuiCommand::Snapshot { session_id: Some(session_id) })
                if session_id == product_session
        ));
        assert!(HostControlsTuiCommand::parse("status runtime-thread-a").is_err());
    }
}
