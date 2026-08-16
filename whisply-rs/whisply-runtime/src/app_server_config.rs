//! Namespaced Whisply values accepted inside stable app-server `config` maps.
//!
//! The upstream app-server protocol deliberately preserves an extensible
//! `config` object. Whisply consumes its values before that map reaches the
//! generic configuration loader, so these product controls cannot be mistaken
//! for user-editable TOML configuration or a new JSON-RPC envelope field.

use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Stable namespaced `thread/start.config` key for directory state.
pub const DIRECTORY_CONTRACT_KEY: &str = "whisply.directory.contract";
/// Compatibility-manifest feature required for directory state.
pub const DIRECTORY_CONTRACT_FEATURE: &str = "whisply.directory-contract.v1";
/// Stable namespaced `thread/start.config` key for permission state.
pub const APPROVAL_CONTRACT_KEY: &str = "whisply.approval.contract";
/// Compatibility-manifest feature required for permission state.
pub const APPROVAL_CONTRACT_FEATURE: &str = "whisply.stateless-auto-review.v1";

/// Explicit user-facing workspace state carried by the native client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DirectoryContractMode {
    NoDirectory,
    SelectedDirectory,
}

/// The directory-state projection passed by the native app at thread start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoryContract {
    pub feature: String,
    pub mode: DirectoryContractMode,
    pub project_discovery_enabled: bool,
}

/// Explicit user-facing permission mode carried by the native client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalContractMode {
    #[serde(rename = "ask")]
    Ask,
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "full_access")]
    FullAccess,
}

/// The permission-state projection passed by the native app at thread start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalContract {
    pub feature: String,
    pub mode: ApprovalContractMode,
    pub requires_stateless_auto_review: bool,
}

/// The paired native values the runtime must validate before starting a
/// Whisply-managed thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhisplyThreadContracts {
    pub directory: DirectoryContract,
    pub approval: ApprovalContract,
}

impl WhisplyThreadContracts {
    /// Removes and validates Whisply-owned entries from a stable app-server
    /// config map. The remaining entries are safe to pass to the generic
    /// configuration loader.
    pub fn take_from_config(
        config: &mut HashMap<String, Value>,
    ) -> Result<Option<Self>, AppServerContractError> {
        let (directory, approval) = match (
            config.remove(DIRECTORY_CONTRACT_KEY),
            config.remove(APPROVAL_CONTRACT_KEY),
        ) {
            (Some(directory), Some(approval)) => (directory, approval),
            (None, None) => return Ok(None),
            _ => return Err(AppServerContractError::PairedContractsRequired),
        };

        let directory = serde_json::from_value(directory)
            .map_err(|_| AppServerContractError::MalformedDirectoryContract)?;
        let approval = serde_json::from_value(approval)
            .map_err(|_| AppServerContractError::MalformedApprovalContract)?;
        let contracts = Self {
            directory,
            approval,
        };
        contracts.validate()?;
        Ok(Some(contracts))
    }

    /// Validates both exact feature identities and fail-closed cross-field
    /// invariants. A client cannot request a weaker underlying behavior by
    /// changing a flag independently of its public mode.
    pub fn validate(&self) -> Result<(), AppServerContractError> {
        if self.directory.feature != DIRECTORY_CONTRACT_FEATURE {
            return Err(AppServerContractError::UnsupportedDirectoryFeature);
        }
        match self.directory.mode {
            DirectoryContractMode::NoDirectory if !self.directory.project_discovery_enabled => {}
            DirectoryContractMode::SelectedDirectory
                if self.directory.project_discovery_enabled => {}
            _ => return Err(AppServerContractError::InvalidDirectoryProjection),
        }

        if self.approval.feature != APPROVAL_CONTRACT_FEATURE {
            return Err(AppServerContractError::UnsupportedApprovalFeature);
        }
        match self.approval.mode {
            ApprovalContractMode::Ask | ApprovalContractMode::FullAccess
                if !self.approval.requires_stateless_auto_review => {}
            ApprovalContractMode::Auto if self.approval.requires_stateless_auto_review => {}
            _ => return Err(AppServerContractError::InvalidApprovalProjection),
        }
        Ok(())
    }
}

/// Fail-closed errors for Whisply-owned app-server config values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AppServerContractError {
    #[error("Whisply directory and approval contracts must be supplied together")]
    PairedContractsRequired,
    #[error("the Whisply directory contract is malformed")]
    MalformedDirectoryContract,
    #[error("the Whisply approval contract is malformed")]
    MalformedApprovalContract,
    #[error("the Whisply directory contract feature is unsupported")]
    UnsupportedDirectoryFeature,
    #[error("the Whisply directory contract has an invalid mode/discovery projection")]
    InvalidDirectoryProjection,
    #[error("the Whisply approval contract feature is unsupported")]
    UnsupportedApprovalFeature,
    #[error("the Whisply approval contract has an invalid mode/reviewer projection")]
    InvalidApprovalProjection,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(directory: Value, approval: Value) -> HashMap<String, Value> {
        HashMap::from([
            (DIRECTORY_CONTRACT_KEY.to_string(), directory),
            (APPROVAL_CONTRACT_KEY.to_string(), approval),
            (
                "model".to_string(),
                Value::String("gpt-5.6-luna".to_string()),
            ),
        ])
    }

    #[test]
    fn consumes_native_no_directory_auto_contract_without_leaking_to_toml() {
        let mut values = config(
            serde_json::json!({
                "feature": DIRECTORY_CONTRACT_FEATURE,
                "mode": "noDirectory",
                "projectDiscoveryEnabled": false
            }),
            serde_json::json!({
                "feature": APPROVAL_CONTRACT_FEATURE,
                "mode": "auto",
                "requiresStatelessAutoReview": true
            }),
        );

        let parsed = WhisplyThreadContracts::take_from_config(&mut values)
            .expect("valid contracts")
            .expect("contracts present");
        assert_eq!(parsed.directory.mode, DirectoryContractMode::NoDirectory);
        assert_eq!(parsed.approval.mode, ApprovalContractMode::Auto);
        assert_eq!(values.len(), 1);
        assert!(values.contains_key("model"));
    }

    #[test]
    fn rejects_auto_without_a_fresh_stateless_review_requirement() {
        let mut values = config(
            serde_json::json!({
                "feature": DIRECTORY_CONTRACT_FEATURE,
                "mode": "selectedDirectory",
                "projectDiscoveryEnabled": true
            }),
            serde_json::json!({
                "feature": APPROVAL_CONTRACT_FEATURE,
                "mode": "auto",
                "requiresStatelessAutoReview": false
            }),
        );

        assert_eq!(
            WhisplyThreadContracts::take_from_config(&mut values),
            Err(AppServerContractError::InvalidApprovalProjection)
        );
    }

    #[test]
    fn rejects_no_directory_when_project_discovery_is_enabled() {
        let mut values = config(
            serde_json::json!({
                "feature": DIRECTORY_CONTRACT_FEATURE,
                "mode": "noDirectory",
                "projectDiscoveryEnabled": true
            }),
            serde_json::json!({
                "feature": APPROVAL_CONTRACT_FEATURE,
                "mode": "ask",
                "requiresStatelessAutoReview": false
            }),
        );

        assert_eq!(
            WhisplyThreadContracts::take_from_config(&mut values),
            Err(AppServerContractError::InvalidDirectoryProjection)
        );
    }
}
