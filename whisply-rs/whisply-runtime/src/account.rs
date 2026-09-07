//! Typed, broker-only account projections used by the CLI and TUI.
//!
//! These types deliberately model read-only Website projections. The runtime
//! never calls the Website directly and never receives a Supabase session or
//! an account bearer. Native broker operations validate the signed-in account
//! epoch and return only these bounded, safe-to-display values.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

/// Canonical account projection version returned by Whisply-owned services.
pub const CONTEXTUAL_ACTION_CONTRACT_VERSION: &str = "contextual-action.v1";
/// Canonical cost-metering projection version.
pub const USAGE_METERING_CONTRACT_VERSION: &str = "usage-metering.v1";
/// Cost-weighted Usage is the only supported public basis.
pub const USAGE_METERING_BASIS: &str = "model_cost_weighted";

/// One of the three product-facing Usage window categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageWindowCategory {
    Usage,
    Transcription,
}

/// The bounded window kinds accepted by the Usage contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageWindowKind {
    FiveHour,
    Weekly,
}

/// A cost-based Usage allowance window. `settled` and `reserved` are kept
/// separate so a future reservation is never represented as completed spend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualUsageWindow {
    pub category: UsageWindowCategory,
    pub window: UsageWindowKind,
    pub settled: f64,
    pub reserved: f64,
    pub cap: f64,
    pub used_fraction: f64,
    pub starts_at: Option<String>,
    pub resets_at: Option<String>,
    pub rate_card_version: Option<String>,
}

/// Token totals correlated with a cost window. These are explanatory detail,
/// not an independent allowance or an authorization mechanism.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualUsageTokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

/// The two token window projections carried with one Usage snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualUsageTokenWindows {
    pub five_hour: ContextualUsageTokenTotals,
    pub weekly: ContextualUsageTokenTotals,
}

/// Metadata explaining how cost-based Usage was calculated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualUsageMetering {
    pub contract_version: String,
    pub basis: String,
    pub currency: String,
    pub rate_card_version: String,
    pub normalized_units_per_cash_micro: i64,
    pub tokens: ContextualUsageTokenWindows,
}

/// The exact read-only Usage snapshot shared by Website, Mac, CLI, and TUI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualUsageSnapshot {
    pub contract_version: String,
    pub tier: String,
    pub windows: Vec<ContextualUsageWindow>,
    /// Advanced portions already charged to their real five-hour intervals.
    /// Absent on the original plain projection; never add these to totals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance_windows: Option<Vec<ContextualUsageWindow>>,
    pub metering: ContextualUsageMetering,
    pub generated_at: String,
    pub stale: bool,
}

impl ContextualUsageSnapshot {
    /// Rejects incomplete, stale, malformed, or non-cost-authoritative
    /// projections before they appear in CLI/TUI output.
    pub fn validate(&self) -> Result<(), AccountProjectionError> {
        if self.contract_version != CONTEXTUAL_ACTION_CONTRACT_VERSION
            || self.tier.trim().is_empty()
            || self.generated_at.trim().is_empty()
            || self.stale
            || self.windows.len() != 3
        {
            return Err(AccountProjectionError::InvalidUsage);
        }

        let required = [
            (UsageWindowCategory::Usage, UsageWindowKind::FiveHour),
            (UsageWindowCategory::Usage, UsageWindowKind::Weekly),
            (UsageWindowCategory::Transcription, UsageWindowKind::Weekly),
        ];
        for (category, window) in required {
            if self
                .windows
                .iter()
                .filter(|entry| entry.category == category && entry.window == window)
                .count()
                != 1
            {
                return Err(AccountProjectionError::InvalidUsage);
            }
        }
        for window in &self.windows {
            validate_usage_window(window)?;
        }
        self.validate_advance_windows()?;
        validate_metering(&self.metering)?;
        for window in self
            .windows
            .iter()
            .filter(|window| window.category == UsageWindowCategory::Usage)
        {
            // A window can retain its original card through a pricing refresh,
            // including a borrowed window that has just become current. Its
            // validated amounts remain authoritative for this read projection.
            if window
                .rate_card_version
                .as_deref()
                .is_none_or(|version| version.trim().is_empty())
            {
                return Err(AccountProjectionError::InvalidUsage);
            }
        }
        Ok(())
    }

    fn validate_advance_windows(&self) -> Result<(), AccountProjectionError> {
        let Some(windows) = self
            .advance_windows
            .as_ref()
            .filter(|windows| !windows.is_empty())
        else {
            return Ok(());
        };
        if windows.len() > 2 {
            return Err(AccountProjectionError::InvalidUsage);
        }
        let parse = |value: Option<&str>| {
            value
                .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
                .ok_or(AccountProjectionError::InvalidUsage)
        };
        let generated = parse(Some(&self.generated_at))?;
        let current = self
            .window(UsageWindowCategory::Usage, UsageWindowKind::FiveHour)
            .ok_or(AccountProjectionError::InvalidUsage)?;
        let current_start = parse(current.starts_at.as_deref())?;
        let current_end = parse(current.resets_at.as_deref())?;
        if !(current_start <= generated && generated < current_end) {
            return Err(AccountProjectionError::InvalidUsage);
        }
        let mut previous_start = None;
        for window in windows {
            validate_usage_window(window)?;
            let start = parse(window.starts_at.as_deref())?;
            let end = parse(window.resets_at.as_deref())?;
            let accounted = window.settled + window.reserved;
            if window.category != UsageWindowCategory::Usage
                || window.window != UsageWindowKind::FiveHour
                || accounted <= 0.0
                || (window.used_fraction - accounted / window.cap).abs() > 0.000_000_001
                || window
                    .rate_card_version
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                || end - start != time::Duration::hours(5)
                || end <= generated
                || (start != current_start && start != current_end)
                || previous_start.is_some_and(|previous| start <= previous)
            {
                return Err(AccountProjectionError::InvalidUsage);
            }
            previous_start = Some(start);
        }
        Ok(())
    }

    /// Returns one exact named product window after the snapshot is validated.
    pub fn window(
        &self,
        category: UsageWindowCategory,
        kind: UsageWindowKind,
    ) -> Option<&ContextualUsageWindow> {
        self.windows
            .iter()
            .find(|window| window.category == category && window.window == kind)
    }
}

fn validate_usage_window(window: &ContextualUsageWindow) -> Result<(), AccountProjectionError> {
    let tolerance = (window.cap * 0.000_000_001_f64).max(0.000_001_f64);
    let dates_valid = match (
        window
            .starts_at
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        window
            .resets_at
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
    ) {
        (true, true) => true,
        (false, false) => window.settled == 0.0 && window.reserved == 0.0,
        _ => false,
    };
    let values_valid = window.settled.is_finite()
        && window.reserved.is_finite()
        && window.cap.is_finite()
        && window.used_fraction.is_finite()
        && window.settled >= 0.0
        && window.reserved >= 0.0
        && window.cap > 0.0
        && window.settled + window.reserved <= window.cap + tolerance
        && (0.0..=1.0 + tolerance).contains(&window.used_fraction);
    if dates_valid && values_valid {
        Ok(())
    } else {
        Err(AccountProjectionError::InvalidUsage)
    }
}

fn validate_metering(metering: &ContextualUsageMetering) -> Result<(), AccountProjectionError> {
    let values = [&metering.tokens.five_hour, &metering.tokens.weekly];
    let token_values_valid = values.iter().all(|totals| {
        totals.input >= 0 && totals.output >= 0 && totals.cache_read >= 0 && totals.cache_write >= 0
    });
    if metering.contract_version == USAGE_METERING_CONTRACT_VERSION
        && metering.basis == USAGE_METERING_BASIS
        && metering.currency == "USD"
        && !metering.rate_card_version.trim().is_empty()
        && metering.normalized_units_per_cash_micro > 0
        && token_values_valid
    {
        Ok(())
    } else {
        Err(AccountProjectionError::InvalidUsage)
    }
}

/// Public connector provider ids accepted by the Website account projection.
/// Unknown providers are rejected rather than becoming implicit local routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorProvider {
    Google,
    Github,
    Microsoft,
    Notion,
    Slack,
    Zoom,
    Calendly,
    Salesforce,
    Hubspot,
}

/// Connection state emitted by the Website contract. A disconnected provider
/// is not silently treated as readable or writable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextualConnectionStatus {
    Connected,
    Degraded,
    ReconnectRequired,
}

/// Read/write grant and capability access. Unknown access values fail closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAccess {
    Read,
    Write,
}

/// Whether a connector is used only for an explicitly selected action or
/// continues to contribute context after it has been connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorDisclosureAccessBehavior {
    OnDemand,
    Continuous,
}

/// Bounded per-connection account Usage. These counts describe connector
/// activity; they are never a billing authority or a provider credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectionUsage {
    pub operations: u64,
    pub provider_calls: u64,
    pub last_operation_at: Option<String>,
}

/// One current account connection returned through `account.connections.read`.
/// Generic `capabilities` values stay bounded JSON because the Website
/// deliberately supports provider-specific metadata; credential-like keys are
/// rejected before the projection can reach CLI/TUI output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectionSummary {
    pub id: String,
    pub provider: ConnectorProvider,
    pub label: String,
    pub provider_account_type: Option<String>,
    pub account_hint: Option<String>,
    pub status: ContextualConnectionStatus,
    pub products: Vec<String>,
    pub granted_scopes: Vec<String>,
    pub grant_ids: Vec<String>,
    pub capabilities: Vec<BTreeMap<String, Value>>,
    pub last_success_at: Option<String>,
    pub last_used_at: Option<String>,
    pub last_failure_at: Option<String>,
    pub last_failure_code: Option<String>,
    pub reconnect_reason: Option<String>,
    pub provider_revocation_verified: Option<bool>,
    pub disconnected_at: Option<String>,
    pub retention_policy: Option<String>,
    pub memory_behavior: Option<String>,
    pub usage_behavior: Option<String>,
    pub usage: ContextualConnectionUsage,
    pub version: u64,
    pub updated_at: String,
}

/// One provider product in the Website connector catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorProduct {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// Named authorization profiles are display-safe labels only. They do not
/// contain an OAuth client, token, or selected account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorAuthorizationProfiles {
    pub read: Option<String>,
    pub write: Option<String>,
}

/// A catalog authorization grant. Grant ids are the only values a local
/// connector selection may retain; authorization itself stays native-owned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorGrant {
    pub id: String,
    pub product_id: String,
    pub access: ConnectorAccess,
    pub label: String,
    pub description: String,
    pub scopes: Vec<String>,
    pub authorization_profiles: ContextualConnectorAuthorizationProfiles,
    pub default_selected: bool,
    pub available: bool,
    pub reconnect_available: bool,
    pub unavailable_reason: Option<String>,
}

/// A first-party connector capability in the public catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorCapability {
    pub id: String,
    pub product_id: String,
    pub access: ConnectorAccess,
    pub label: String,
    pub action_type: String,
    pub policy_class: String,
    pub action_class: String,
    pub required_grant_ids: Vec<String>,
    pub unavailable_reason: Option<String>,
}

/// User-facing provider disclosures from the Website catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorDisclosure {
    pub access_behavior: ConnectorDisclosureAccessBehavior,
    pub redirect_notice: String,
    pub admin_notice: Option<String>,
    pub wrong_account_recovery: String,
    pub credential_notice: String,
    pub model_data_notice: String,
    pub cache_notice: String,
    pub control_notice: String,
    pub provider_data_notice: String,
}

/// Revisioned disclosure acknowledgement metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorAuthorizationDisclosure {
    pub version: String,
    pub revision: String,
}

/// Explicit account-family compatibility. This does not infer credential
/// sharing beyond the exact Website contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorAccountFamily {
    pub kind: String,
    pub products: Vec<String>,
    pub standalone_products: Vec<String>,
    pub authorization_unit: String,
    pub credential_reuse: String,
}

/// Public provider availability. It describes UI affordances only; it cannot
/// bypass account, scope, confirmation, or native permission policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorAvailability {
    pub connect_enabled: bool,
    pub reconnect_enabled: bool,
    pub read_enabled: bool,
    pub write_enabled: bool,
    pub reason_code: Option<String>,
}

/// Full public connector catalog entry used by `connectors list` and status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectorCatalogEntry {
    pub id: ConnectorProvider,
    pub name: String,
    pub description: String,
    pub products: Vec<ContextualConnectorProduct>,
    pub grants: Vec<ContextualConnectorGrant>,
    pub capabilities: Vec<ContextualConnectorCapability>,
    pub disclosure: ContextualConnectorDisclosure,
    pub authorization_disclosure: ContextualConnectorAuthorizationDisclosure,
    pub browser_management_required: bool,
    pub multiple_accounts_supported: bool,
    pub repository_selection_required: bool,
    pub provider_picker_required: bool,
    pub management_url: String,
    pub account_family: Option<ContextualConnectorAccountFamily>,
    pub availability: ContextualConnectorAvailability,
}

/// Retention information attached to the full Website connections projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectionsRetention {
    pub connector_source_material: String,
    pub connector_operations_days: u16,
    pub provider_cost_events_months: u16,
}

/// Exact broker-only projection for the Website `connections` response.
/// There is deliberately no endpoint, token, or raw account id here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextualConnectionsSnapshot {
    pub contract_version: String,
    pub connections: Vec<ContextualConnectionSummary>,
    pub catalog: Vec<ContextualConnectorCatalogEntry>,
    pub retention: ContextualConnectionsRetention,
    pub generated_at: String,
}

impl ContextualConnectionsSnapshot {
    /// Validates the complete Website projection so a partial or malformed
    /// broker response never masquerades as current connection state.
    pub fn validate(&self) -> Result<(), AccountProjectionError> {
        if self.contract_version != CONTEXTUAL_ACTION_CONTRACT_VERSION
            || self.connections.len() > MAX_CONNECTIONS
            || self.catalog.len() > MAX_CATALOG_ENTRIES
            || !is_rfc3339(&self.generated_at)
            || !valid_bounded_text(&self.retention.connector_source_material, MAX_STRING_BYTES)
            || self.retention.connector_operations_days > 365
            || self.retention.provider_cost_events_months > 120
        {
            return Err(AccountProjectionError::InvalidConnections);
        }
        let mut connection_ids = BTreeSet::new();
        for connection in &self.connections {
            if !connection_ids.insert(&connection.id) || !validate_connection(connection) {
                return Err(AccountProjectionError::InvalidConnections);
            }
        }
        let mut catalog_ids = BTreeSet::new();
        for entry in &self.catalog {
            if !catalog_ids.insert(entry.id) || !validate_catalog_entry(entry) {
                return Err(AccountProjectionError::InvalidConnections);
            }
        }
        Ok(())
    }
}

const MAX_CONNECTIONS: usize = 100;
const MAX_CATALOG_ENTRIES: usize = 16;
const MAX_STRING_BYTES: usize = 12 * 1024;
const MAX_SMALL_STRING_BYTES: usize = 1024;
const MAX_SCOPES_OR_GRANT_IDS: usize = 256;
const MAX_CONNECTION_CAPABILITIES: usize = 256;
const MAX_CATALOG_PRODUCTS: usize = 64;
const MAX_CATALOG_GRANTS: usize = 512;
const MAX_CATALOG_CAPABILITIES: usize = 512;
const CONNECTOR_DISCLOSURE_CONTRACT_VERSION: &str = "whisply-connector-disclosure.v1";

fn validate_connection(connection: &ContextualConnectionSummary) -> bool {
    valid_identifier(&connection.id)
        && valid_bounded_text(&connection.label, MAX_SMALL_STRING_BYTES)
        && valid_optional_bounded_text(
            connection.provider_account_type.as_deref(),
            MAX_SMALL_STRING_BYTES,
        )
        && valid_optional_bounded_text(connection.account_hint.as_deref(), MAX_SMALL_STRING_BYTES)
        && valid_bounded_text_array(&connection.products, MAX_SCOPES_OR_GRANT_IDS)
        && valid_bounded_text_array(&connection.granted_scopes, MAX_SCOPES_OR_GRANT_IDS)
        && valid_bounded_text_array(&connection.grant_ids, MAX_SCOPES_OR_GRANT_IDS)
        && connection.capabilities.len() <= MAX_CONNECTION_CAPABILITIES
        && connection
            .capabilities
            .iter()
            .all(|value| valid_safe_capability_record(value, 0))
        && connection.usage.operations <= 10_000_000
        && connection.usage.provider_calls <= 100_000_000
        && is_optional_rfc3339(connection.usage.last_operation_at.as_deref())
        && is_optional_rfc3339(connection.last_success_at.as_deref())
        && is_optional_rfc3339(connection.last_used_at.as_deref())
        && is_optional_rfc3339(connection.last_failure_at.as_deref())
        && is_optional_rfc3339(connection.disconnected_at.as_deref())
        && valid_optional_bounded_text(
            connection.last_failure_code.as_deref(),
            MAX_SMALL_STRING_BYTES,
        )
        && valid_optional_bounded_text(connection.reconnect_reason.as_deref(), MAX_STRING_BYTES)
        && valid_optional_bounded_text(connection.retention_policy.as_deref(), MAX_STRING_BYTES)
        && valid_optional_bounded_text(connection.memory_behavior.as_deref(), MAX_STRING_BYTES)
        && valid_optional_bounded_text(connection.usage_behavior.as_deref(), MAX_STRING_BYTES)
        && (1..=1_000_000).contains(&connection.version)
        && is_rfc3339(&connection.updated_at)
}

fn validate_catalog_entry(entry: &ContextualConnectorCatalogEntry) -> bool {
    valid_bounded_text(&entry.name, MAX_SMALL_STRING_BYTES)
        && valid_bounded_text(&entry.description, MAX_STRING_BYTES)
        && entry.products.len() <= MAX_CATALOG_PRODUCTS
        && entry.grants.len() <= MAX_CATALOG_GRANTS
        && entry.capabilities.len() <= MAX_CATALOG_CAPABILITIES
        && entry.browser_management_required
        && entry.multiple_accounts_supported
        && valid_https_url(&entry.management_url)
        && validate_disclosure(&entry.disclosure)
        && entry.authorization_disclosure.version == CONNECTOR_DISCLOSURE_CONTRACT_VERSION
        && valid_lowercase_sha256(&entry.authorization_disclosure.revision)
        && valid_optional_bounded_text(
            entry.availability.reason_code.as_deref(),
            MAX_SMALL_STRING_BYTES,
        )
        && entry.products.iter().all(validate_catalog_product)
        && entry.grants.iter().all(validate_catalog_grant)
        && entry.capabilities.iter().all(validate_catalog_capability)
        && entry
            .account_family
            .as_ref()
            .is_none_or(validate_account_family)
}

fn validate_catalog_product(product: &ContextualConnectorProduct) -> bool {
    valid_identifier(&product.id)
        && valid_bounded_text(&product.name, MAX_SMALL_STRING_BYTES)
        && valid_bounded_text(&product.description, MAX_STRING_BYTES)
}

fn validate_catalog_grant(grant: &ContextualConnectorGrant) -> bool {
    valid_identifier(&grant.id)
        && valid_identifier(&grant.product_id)
        && valid_bounded_text(&grant.label, MAX_SMALL_STRING_BYTES)
        && valid_bounded_text(&grant.description, MAX_STRING_BYTES)
        && valid_bounded_text_array(&grant.scopes, MAX_SCOPES_OR_GRANT_IDS)
        && valid_optional_bounded_text(
            grant.authorization_profiles.read.as_deref(),
            MAX_SMALL_STRING_BYTES,
        )
        && valid_optional_bounded_text(
            grant.authorization_profiles.write.as_deref(),
            MAX_SMALL_STRING_BYTES,
        )
        && valid_optional_bounded_text(grant.unavailable_reason.as_deref(), MAX_STRING_BYTES)
}

fn validate_catalog_capability(capability: &ContextualConnectorCapability) -> bool {
    valid_identifier(&capability.id)
        && valid_identifier(&capability.product_id)
        && valid_bounded_text(&capability.label, MAX_SMALL_STRING_BYTES)
        && valid_identifier(&capability.action_type)
        && valid_identifier(&capability.policy_class)
        && valid_identifier(&capability.action_class)
        && valid_bounded_text_array(&capability.required_grant_ids, MAX_SCOPES_OR_GRANT_IDS)
        && valid_optional_bounded_text(capability.unavailable_reason.as_deref(), MAX_STRING_BYTES)
}

fn validate_disclosure(disclosure: &ContextualConnectorDisclosure) -> bool {
    valid_bounded_text(&disclosure.redirect_notice, MAX_STRING_BYTES)
        && valid_optional_bounded_text(disclosure.admin_notice.as_deref(), MAX_STRING_BYTES)
        && valid_bounded_text(&disclosure.wrong_account_recovery, MAX_STRING_BYTES)
        && valid_bounded_text(&disclosure.credential_notice, MAX_STRING_BYTES)
        && valid_bounded_text(&disclosure.model_data_notice, MAX_STRING_BYTES)
        && valid_bounded_text(&disclosure.cache_notice, MAX_STRING_BYTES)
        && valid_bounded_text(&disclosure.control_notice, MAX_STRING_BYTES)
        && valid_bounded_text(&disclosure.provider_data_notice, MAX_STRING_BYTES)
}

fn validate_account_family(family: &ContextualConnectorAccountFamily) -> bool {
    family.kind == "microsoft_graph"
        && valid_bounded_text_array(&family.products, MAX_CATALOG_PRODUCTS)
        && valid_bounded_text_array(&family.standalone_products, MAX_CATALOG_PRODUCTS)
        && family.authorization_unit == "exact_toolkit_profile"
        && family.credential_reuse == "exact_authorization_only"
}

fn valid_safe_capability_record(value: &BTreeMap<String, Value>, depth: usize) -> bool {
    value.len() <= 64
        && value.iter().all(|(key, value)| {
            valid_bounded_text(key, MAX_SMALL_STRING_BYTES)
                && !credential_like_key(key)
                && valid_safe_json(value, depth + 1)
        })
}

fn valid_safe_json(value: &Value, depth: usize) -> bool {
    if depth > 12 {
        return false;
    }
    match value {
        Value::Null | Value::Bool(_) => true,
        Value::Number(number) => number.as_f64().is_some_and(f64::is_finite),
        Value::String(value) => valid_bounded_text(value, MAX_STRING_BYTES),
        Value::Array(values) => {
            values.len() <= MAX_CONNECTION_CAPABILITIES
                && values.iter().all(|value| valid_safe_json(value, depth + 1))
        }
        Value::Object(values) => {
            values.len() <= 64
                && values.iter().all(|(key, value)| {
                    valid_bounded_text(key, MAX_SMALL_STRING_BYTES)
                        && !credential_like_key(key)
                        && valid_safe_json(value, depth + 1)
                })
        }
    }
}

fn credential_like_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('_', "");
    [
        "token",
        "secret",
        "password",
        "authorization",
        "cookie",
        "credential",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn valid_identifier(value: &str) -> bool {
    valid_bounded_text(value, MAX_SMALL_STRING_BYTES)
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b':' | b'/' | b'@')
        })
}

fn valid_bounded_text_array(values: &[String], maximum_count: usize) -> bool {
    values.len() <= maximum_count
        && values
            .iter()
            .all(|value| valid_bounded_text(value, MAX_SMALL_STRING_BYTES))
}

fn valid_bounded_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_bytes
}

fn valid_optional_bounded_text(value: Option<&str>, maximum_bytes: usize) -> bool {
    value.is_none_or(|value| valid_bounded_text(value, maximum_bytes))
}

fn is_rfc3339(value: &str) -> bool {
    value.len() <= 128 && OffsetDateTime::parse(value, &Rfc3339).is_ok()
}

fn is_optional_rfc3339(value: Option<&str>) -> bool {
    value.is_none_or(is_rfc3339)
}

fn valid_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_https_url(value: &str) -> bool {
    valid_bounded_text(value, MAX_STRING_BYTES)
        && Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
        })
}

/// Errors emitted for bounded account-read projections. They intentionally do
/// not include an account identifier, Website token, or raw response body.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AccountProjectionError {
    #[error("Whisply account projection is unavailable")]
    Unavailable,
    #[error("Whisply account Usage projection is invalid")]
    InvalidUsage,
    #[error("Whisply account connections projection is invalid")]
    InvalidConnections,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use sha2::Digest;
    use sha2::Sha256;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct SessionAccountBehaviorFixture {
        schema_version: u8,
        fixture_revision: String,
        provenance: FixtureProvenance,
        account_boundary: FixtureAccountBoundary,
        account_change_exposure: FixtureAccountChangeExposure,
        session: FixtureSession,
        memory: FixtureMemory,
        subscription: FixtureSubscription,
        usage: ContextualUsageSnapshot,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureProvenance {
        kind: String,
        sensitive_data: String,
        live_authenticated_recovery: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureAccountBoundary {
        binding: String,
        response_epoch_must_match_binding: bool,
        on_account_change: String,
    }

    /// The cross-surface proof that a logout or account switch cannot expose
    /// anything owned by the prior account.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureAccountChangeExposure {
        prior_account_epoch: String,
        current_account_epoch: String,
        triggers: Vec<String>,
        required_surfaces: Vec<String>,
        surfaces: Vec<FixtureAccountChangeSurface>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureAccountChangeSurface {
        surface: String,
        prior_account_read: String,
        prior_account_data_exposed: bool,
        cached_projection_cleared: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureSession {
        history_scope: String,
        resume_requires_current_account: bool,
        memory_dispositions: Vec<String>,
        usage_states: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureMemory {
        initial: FixtureMemorySettings,
        request_enabled: bool,
        connected_app_available: bool,
        expected: FixtureMemorySettings,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureMemorySettings {
        enabled: bool,
        learn_automatically: bool,
        reference_chat_history: bool,
        reference_saved_sessions: bool,
        reference_connected_apps: bool,
        personalize_web_search: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureSubscription {
        catalog_availability: Vec<String>,
        default_model_requires_availability: bool,
        unknown_tier_input: String,
        unknown_tier_outcome: String,
    }

    fn totals() -> ContextualUsageTokenTotals {
        ContextualUsageTokenTotals {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
        }
    }

    fn window(
        category: UsageWindowCategory,
        kind: UsageWindowKind,
        rate_card_version: Option<&str>,
    ) -> ContextualUsageWindow {
        ContextualUsageWindow {
            category,
            window: kind,
            settled: 10.0,
            reserved: 5.0,
            cap: 100.0,
            used_fraction: 0.15,
            starts_at: Some("2026-08-08T00:00:00Z".to_string()),
            resets_at: Some("2026-08-08T05:00:00Z".to_string()),
            rate_card_version: rate_card_version.map(ToString::to_string),
        }
    }

    fn snapshot() -> ContextualUsageSnapshot {
        ContextualUsageSnapshot {
            advance_windows: None,
            contract_version: CONTEXTUAL_ACTION_CONTRACT_VERSION.to_string(),
            tier: "pro".to_string(),
            windows: vec![
                window(
                    UsageWindowCategory::Usage,
                    UsageWindowKind::FiveHour,
                    Some("rate-2026-08"),
                ),
                window(
                    UsageWindowCategory::Usage,
                    UsageWindowKind::Weekly,
                    Some("rate-2026-08"),
                ),
                window(
                    UsageWindowCategory::Transcription,
                    UsageWindowKind::Weekly,
                    None,
                ),
            ],
            metering: ContextualUsageMetering {
                contract_version: USAGE_METERING_CONTRACT_VERSION.to_string(),
                basis: USAGE_METERING_BASIS.to_string(),
                currency: "USD".to_string(),
                rate_card_version: "rate-2026-08".to_string(),
                normalized_units_per_cash_micro: 9,
                tokens: ContextualUsageTokenWindows {
                    five_hour: totals(),
                    weekly: totals(),
                },
            },
            generated_at: "2026-08-08T01:00:00Z".to_string(),
            stale: false,
        }
    }

    fn connections_snapshot() -> ContextualConnectionsSnapshot {
        let mut capability = BTreeMap::new();
        capability.insert("supportsSearch".to_string(), Value::Bool(true));
        ContextualConnectionsSnapshot {
            contract_version: CONTEXTUAL_ACTION_CONTRACT_VERSION.to_string(),
            connections: vec![ContextualConnectionSummary {
                id: "connection:google:work".to_string(),
                provider: ConnectorProvider::Google,
                label: "Work Google".to_string(),
                provider_account_type: Some("workspace".to_string()),
                account_hint: Some("w***@example.com".to_string()),
                status: ContextualConnectionStatus::Connected,
                products: vec!["gmail".to_string()],
                granted_scopes: vec!["gmail.readonly".to_string()],
                grant_ids: vec!["gmail.read".to_string()],
                capabilities: vec![capability],
                last_success_at: Some("2026-08-08T01:00:00Z".to_string()),
                last_used_at: None,
                last_failure_at: None,
                last_failure_code: None,
                reconnect_reason: None,
                provider_revocation_verified: Some(true),
                disconnected_at: None,
                retention_policy: Some("account_policy".to_string()),
                memory_behavior: Some("on_demand".to_string()),
                usage_behavior: Some("metered".to_string()),
                usage: ContextualConnectionUsage {
                    operations: 2,
                    provider_calls: 4,
                    last_operation_at: Some("2026-08-08T01:00:00Z".to_string()),
                },
                version: 1,
                updated_at: "2026-08-08T01:00:00Z".to_string(),
            }],
            catalog: vec![ContextualConnectorCatalogEntry {
                id: ConnectorProvider::Google,
                name: "Google".to_string(),
                description: "Google work tools".to_string(),
                products: vec![ContextualConnectorProduct {
                    id: "gmail".to_string(),
                    name: "Gmail".to_string(),
                    description: "Mail".to_string(),
                }],
                grants: vec![ContextualConnectorGrant {
                    id: "gmail.read".to_string(),
                    product_id: "gmail".to_string(),
                    access: ConnectorAccess::Read,
                    label: "Read mail".to_string(),
                    description: "Read selected mail".to_string(),
                    scopes: vec!["gmail.readonly".to_string()],
                    authorization_profiles: ContextualConnectorAuthorizationProfiles {
                        read: Some("readonly".to_string()),
                        write: None,
                    },
                    default_selected: true,
                    available: true,
                    reconnect_available: true,
                    unavailable_reason: None,
                }],
                capabilities: vec![ContextualConnectorCapability {
                    id: "gmail.search".to_string(),
                    product_id: "gmail".to_string(),
                    access: ConnectorAccess::Read,
                    label: "Search mail".to_string(),
                    action_type: "search".to_string(),
                    policy_class: "read-only".to_string(),
                    action_class: "automatic".to_string(),
                    required_grant_ids: vec!["gmail.read".to_string()],
                    unavailable_reason: None,
                }],
                disclosure: ContextualConnectorDisclosure {
                    access_behavior: ConnectorDisclosureAccessBehavior::OnDemand,
                    redirect_notice: "Open the provider connection page.".to_string(),
                    admin_notice: None,
                    wrong_account_recovery: "Reconnect the intended account.".to_string(),
                    credential_notice: "Credentials stay with the provider.".to_string(),
                    model_data_notice: "Only selected data is shared.".to_string(),
                    cache_notice: "Cached material follows retention policy.".to_string(),
                    control_notice: "You can disconnect at any time.".to_string(),
                    provider_data_notice: "Provider policy applies.".to_string(),
                },
                authorization_disclosure: ContextualConnectorAuthorizationDisclosure {
                    version: CONNECTOR_DISCLOSURE_CONTRACT_VERSION.to_string(),
                    revision: "a".repeat(64),
                },
                browser_management_required: true,
                multiple_accounts_supported: true,
                repository_selection_required: false,
                provider_picker_required: false,
                management_url: "https://whisply.net/account/connections/".to_string(),
                account_family: None,
                availability: ContextualConnectorAvailability {
                    connect_enabled: true,
                    reconnect_enabled: true,
                    read_enabled: true,
                    write_enabled: false,
                    reason_code: None,
                },
            }],
            retention: ContextualConnectionsRetention {
                connector_source_material: "account_policy".to_string(),
                connector_operations_days: 30,
                provider_cost_events_months: 3,
            },
            generated_at: "2026-08-08T01:00:00Z".to_string(),
        }
    }

    #[test]
    fn usage_requires_exactly_three_cost_based_windows() {
        assert!(snapshot().validate().is_ok());

        let mut malformed = snapshot();
        malformed.windows.pop();
        assert_eq!(
            malformed.validate(),
            Err(AccountProjectionError::InvalidUsage)
        );

        let mut stale = snapshot();
        stale.stale = true;
        assert_eq!(stale.validate(), Err(AccountProjectionError::InvalidUsage));
    }

    #[test]
    fn usage_never_conflates_reserved_cost_with_settled_cost() {
        let mut malformed = snapshot();
        malformed.windows[0].reserved = 95.1;
        assert_eq!(
            malformed.validate(),
            Err(AccountProjectionError::InvalidUsage)
        );
    }

    #[test]
    fn shared_session_account_behavior_fixture_matches_broker_contract() {
        const FIXTURE: &str =
            include_str!("../../../../contracts/fixtures/whisply-session-account-behavior-v1.json");
        let digest = Sha256::digest(FIXTURE.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            digest,
            include_str!(
                "../../../../contracts/fixtures/whisply-session-account-behavior-v1.sha256"
            )
            .trim()
        );

        let fixture: SessionAccountBehaviorFixture =
            serde_json::from_str(FIXTURE).expect("session/account behavior fixture");
        assert_eq!(fixture.schema_version, 1);
        assert_eq!(
            fixture.fixture_revision,
            "session-account-behavior-2026-08-12.1"
        );
        assert_eq!(
            fixture.provenance.kind,
            "synthetic_source_behavior_baseline"
        );
        assert_eq!(fixture.provenance.sensitive_data, "omitted");
        assert_eq!(
            fixture.provenance.live_authenticated_recovery,
            "candidate_evidence_required"
        );
        assert_eq!(fixture.account_boundary.binding, "account_epoch");
        assert!(fixture.account_boundary.response_epoch_must_match_binding);
        assert_eq!(fixture.account_boundary.on_account_change, "unavailable");

        // WCD-148: one proof spanning every surface a prior account could leak
        // through. The required list is asserted exactly so a surface cannot be
        // quietly dropped from the proof, and the surface entries must match it
        // one-for-one.
        let exposure = &fixture.account_change_exposure;
        assert_ne!(exposure.prior_account_epoch, exposure.current_account_epoch);
        assert_eq!(
            exposure.triggers,
            vec!["logout".to_string(), "account_switch".to_string()]
        );
        assert_eq!(
            exposure.required_surfaces,
            vec![
                "history".to_string(),
                "memory".to_string(),
                "connectors".to_string(),
                "usage".to_string(),
                "subscription".to_string(),
            ]
        );
        assert_eq!(
            exposure
                .surfaces
                .iter()
                .map(|surface| surface.surface.clone())
                .collect::<Vec<_>>(),
            exposure.required_surfaces
        );
        for surface in &exposure.surfaces {
            assert_eq!(
                surface.prior_account_read, "unavailable",
                "surface {} must refuse a prior-account read",
                surface.surface
            );
            assert!(
                !surface.prior_account_data_exposed,
                "surface {} must not expose prior-account data",
                surface.surface
            );
            assert!(
                surface.cached_projection_cleared,
                "surface {} must clear its cached projection",
                surface.surface
            );
        }
        assert_eq!(fixture.session.history_scope, "current_account_only");
        assert!(fixture.session.resume_requires_current_account);
        assert_eq!(
            fixture.session.memory_dispositions,
            vec!["used".to_string(), "proposed".to_string()]
        );
        assert_eq!(
            fixture.session.usage_states,
            vec![
                "pending".to_string(),
                "settled".to_string(),
                "not_charged".to_string(),
                "accounting_unavailable".to_string(),
            ]
        );
        assert_eq!(
            fixture.memory.initial,
            FixtureMemorySettings {
                enabled: true,
                learn_automatically: true,
                reference_chat_history: false,
                reference_saved_sessions: true,
                reference_connected_apps: true,
                personalize_web_search: true,
            }
        );
        assert!(!fixture.memory.request_enabled);
        assert!(fixture.memory.connected_app_available);
        assert_eq!(
            fixture.memory.expected,
            FixtureMemorySettings {
                enabled: false,
                learn_automatically: false,
                reference_chat_history: false,
                reference_saved_sessions: true,
                reference_connected_apps: false,
                personalize_web_search: false,
            }
        );
        assert_eq!(
            fixture.subscription.catalog_availability,
            vec![
                "available".to_string(),
                "upgrade_required".to_string(),
                "unavailable".to_string(),
            ]
        );
        assert!(fixture.subscription.default_model_requires_availability);
        assert_eq!(fixture.subscription.unknown_tier_input, "future_paid_tier");
        assert_eq!(fixture.subscription.unknown_tier_outcome, "starter");

        assert!(fixture.usage.validate().is_ok());
        assert_eq!(
            fixture
                .usage
                .window(UsageWindowCategory::Usage, UsageWindowKind::FiveHour),
            Some(&ContextualUsageWindow {
                category: UsageWindowCategory::Usage,
                window: UsageWindowKind::FiveHour,
                settled: 12.5,
                reserved: 2.5,
                cap: 100.0,
                used_fraction: 0.15,
                starts_at: Some("2026-08-11T00:00:00Z".to_string()),
                resets_at: Some("2026-08-11T05:00:00Z".to_string()),
                rate_card_version: Some("rate-card-2026-08".to_string()),
            })
        );
        let mut stale = fixture.usage;
        stale.stale = true;
        assert_eq!(stale.validate(), Err(AccountProjectionError::InvalidUsage));
    }

    #[test]
    fn connections_require_the_full_broker_only_website_projection() {
        let snapshot = connections_snapshot();
        assert!(snapshot.validate().is_ok());

        let encoded = serde_json::to_value(&snapshot).expect("connections encode");
        assert!(serde_json::from_value::<ContextualConnectionsSnapshot>(encoded).is_ok());

        let mut credential_like = snapshot;
        credential_like.connections[0].capabilities[0].insert(
            "access_token".to_string(),
            Value::String("never".to_string()),
        );
        assert_eq!(
            credential_like.validate(),
            Err(AccountProjectionError::InvalidConnections)
        );
    }

    fn advance_snapshot_value() -> Value {
        serde_json::from_str::<Value>(include_str!(
            "../../../../contracts/fixtures/whisply-usage-advance-v1.json"
        ))
        .unwrap()["snapshot"]
            .clone()
    }

    #[test]
    fn advance_detail_preserves_three_limits_and_original_plain_shape() {
        let value = advance_snapshot_value();
        let full: ContextualUsageSnapshot = serde_json::from_value(value.clone()).unwrap();
        assert!(full.validate().is_ok());
        assert_ne!(
            full.advance_windows.as_ref().unwrap()[0]
                .rate_card_version
                .as_deref(),
            Some(full.metering.rate_card_version.as_str())
        );
        for count in 0..=2 {
            let mut selected = full.clone();
            selected.advance_windows.as_mut().unwrap().truncate(count);
            assert!(selected.validate().is_ok());
            assert_eq!(selected.windows, full.windows);
            assert_eq!(selected.metering, full.metering);
        }
        let mut plain = value;
        plain.as_object_mut().unwrap().remove("advanceWindows");
        let decoded: ContextualUsageSnapshot = serde_json::from_value(plain.clone()).unwrap();
        assert!(decoded.advance_windows.is_none());
        assert!(decoded.validate().is_ok());
        let mut original_encoded = serde_json::to_value(&full).unwrap();
        original_encoded
            .as_object_mut()
            .unwrap()
            .remove("advanceWindows");
        assert_eq!(original_encoded.as_object().unwrap().len(), 6);
        assert_eq!(serde_json::to_value(decoded).unwrap(), original_encoded);
    }

    #[test]
    fn borrowed_window_becomes_current_before_another_paid_request() {
        let mut value = advance_snapshot_value();
        let mut current = value["advanceWindows"][1].clone();
        current["rateCardVersion"] = value["advanceWindows"][0]["rateCardVersion"].clone();
        value["windows"][0] = current.clone();
        value["generatedAt"] = current["startsAt"].clone();
        value["advanceWindows"] = serde_json::json!([current]);
        let snapshot: ContextualUsageSnapshot = serde_json::from_value(value).unwrap();
        assert!(snapshot.validate().is_ok());
        assert_ne!(
            snapshot.windows[0].rate_card_version.as_deref(),
            Some(snapshot.metering.rate_card_version.as_str())
        );
        assert_eq!(snapshot.windows[0].used_fraction, 0.2);
        for invalid_card in [None, Some(String::new())] {
            let mut invalid = snapshot.clone();
            invalid.windows[0].rate_card_version = invalid_card;
            assert_eq!(invalid.validate(), Err(AccountProjectionError::InvalidUsage));
        }
    }

    #[test]
    fn advance_detail_rejects_wrong_intervals_categories_and_amounts() {
        for (field, value) in [
            ("category", serde_json::json!("transcription")),
            ("window", serde_json::json!("weekly")),
            ("settled", serde_json::json!(-1)),
            ("reserved", serde_json::json!(101)),
            ("cap", serde_json::json!(0)),
            ("usedFraction", serde_json::json!(0.95)),
            ("startsAt", serde_json::json!("2026-09-07T09:00:00Z")),
            ("resetsAt", serde_json::json!("2026-09-07T14:00:00Z")),
            ("startsAt", Value::Null),
            ("rateCardVersion", serde_json::json!("")),
        ] {
            let mut invalid = advance_snapshot_value();
            invalid["advanceWindows"][0][field] = value;
            let decoded: ContextualUsageSnapshot = serde_json::from_value(invalid).unwrap();
            assert_eq!(
                decoded.validate(),
                Err(AccountProjectionError::InvalidUsage),
                "{field}"
            );
        }
        let full: ContextualUsageSnapshot =
            serde_json::from_value(advance_snapshot_value()).unwrap();
        let mut duplicate = full.clone();
        let rows = duplicate.advance_windows.as_mut().unwrap();
        rows[1] = rows[0].clone();
        assert!(duplicate.validate().is_err());
        let mut excessive = full.clone();
        let row = excessive.advance_windows.as_ref().unwrap()[0].clone();
        excessive.advance_windows.as_mut().unwrap().push(row);
        assert!(excessive.validate().is_err());
        let mut empty_portions = full.clone();
        let row = &mut empty_portions.advance_windows.as_mut().unwrap()[0];
        row.settled = 0.0;
        row.reserved = 0.0;
        row.used_fraction = 0.0;
        assert!(empty_portions.validate().is_err());
        let mut nonfinite = full;
        nonfinite.advance_windows.as_mut().unwrap()[0].cap = f64::INFINITY;
        assert!(nonfinite.validate().is_err());
    }
}
