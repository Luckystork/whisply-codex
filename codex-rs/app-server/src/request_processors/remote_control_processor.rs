use crate::error_code::invalid_request;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RemoteControlClientsListParams;
use codex_app_server_protocol::RemoteControlClientsListResponse;
use codex_app_server_protocol::RemoteControlClientsRevokeParams;
use codex_app_server_protocol::RemoteControlClientsRevokeResponse;
use codex_app_server_protocol::RemoteControlDisableResponse;
use codex_app_server_protocol::RemoteControlEnableResponse;
use codex_app_server_protocol::RemoteControlPairingStartParams;
use codex_app_server_protocol::RemoteControlPairingStartResponse;
use codex_app_server_protocol::RemoteControlPairingStatusParams;
use codex_app_server_protocol::RemoteControlPairingStatusResponse;
use codex_app_server_protocol::RemoteControlStatusReadResponse;
const WHISPLY_MANAGED_REMOTE_CONTROL_UNAVAILABLE_ERROR: &str =
    "Whisply remote control is managed by the installed app and is unavailable in this runtime.";

#[derive(Clone, Default)]
pub(crate) struct RemoteControlRequestProcessor;

impl RemoteControlRequestProcessor {
    fn unavailable<T>() -> Result<T, JSONRPCErrorError> {
        Err(invalid_request(
            WHISPLY_MANAGED_REMOTE_CONTROL_UNAVAILABLE_ERROR,
        ))
    }

    pub(crate) async fn enable(
        &self,
        _ephemeral: bool,
        _app_server_client_name: Option<&str>,
    ) -> Result<RemoteControlEnableResponse, JSONRPCErrorError> {
        Self::unavailable()
    }

    pub(crate) async fn disable(
        &self,
        _ephemeral: bool,
        _app_server_client_name: Option<&str>,
    ) -> Result<RemoteControlDisableResponse, JSONRPCErrorError> {
        Self::unavailable()
    }

    pub(crate) fn status_read(&self) -> Result<RemoteControlStatusReadResponse, JSONRPCErrorError> {
        Self::unavailable()
    }

    pub(crate) async fn pairing_start(
        &self,
        _params: RemoteControlPairingStartParams,
        _app_server_client_name: Option<&str>,
    ) -> Result<RemoteControlPairingStartResponse, JSONRPCErrorError> {
        Self::unavailable()
    }

    pub(crate) async fn pairing_status(
        &self,
        _params: RemoteControlPairingStatusParams,
    ) -> Result<RemoteControlPairingStatusResponse, JSONRPCErrorError> {
        Self::unavailable()
    }

    pub(crate) async fn clients_list(
        &self,
        _params: RemoteControlClientsListParams,
    ) -> Result<RemoteControlClientsListResponse, JSONRPCErrorError> {
        Self::unavailable()
    }

    pub(crate) async fn clients_revoke(
        &self,
        _params: RemoteControlClientsRevokeParams,
    ) -> Result<RemoteControlClientsRevokeResponse, JSONRPCErrorError> {
        Self::unavailable()
    }
}
