use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use ts_rs::TS;
use uuid::Uuid;

/// Stable Whisply account identifier used across CLI, runtime gateway, and
/// broker projections.
///
/// Wire and storage forms are always the canonical lowercase UUID string.
/// Mixed-case spellings are rejected rather than normalized silently.
#[derive(Debug, Clone, PartialEq, Eq, Hash, TS)]
#[ts(type = "string")]
pub struct CanonicalAccountId {
    uuid: Uuid,
    raw: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CanonicalAccountIdError {
    #[error("Whisply account identifiers must be canonical lowercase UUIDs")]
    Invalid,
}

impl CanonicalAccountId {
    pub fn new(uuid: Uuid) -> Self {
        let raw = uuid.to_string().to_ascii_lowercase();
        Self { uuid, raw }
    }

    pub fn parse(value: &str) -> Result<Self, CanonicalAccountIdError> {
        let trimmed = value.trim();
        let uuid = Uuid::parse_str(trimmed).map_err(|_| CanonicalAccountIdError::Invalid)?;
        let raw = uuid.to_string().to_ascii_lowercase();
        if trimmed != raw {
            return Err(CanonicalAccountIdError::Invalid);
        }
        Ok(Self { uuid, raw })
    }

    /// Accepts any UUID spelling and returns the canonical lowercase form.
    pub fn from_loose_uuid_string(value: &str) -> Option<Self> {
        Uuid::parse_str(value.trim()).ok().map(Self::new)
    }

    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn receipt_scope(&self, receipt_id: &str) -> String {
        format!("{}:receipt:{receipt_id}", self.raw)
    }
}

impl From<Uuid> for CanonicalAccountId {
    fn from(value: Uuid) -> Self {
        Self::new(value)
    }
}

impl From<CanonicalAccountId> for String {
    fn from(value: CanonicalAccountId) -> Self {
        value.raw
    }
}

impl FromStr for CanonicalAccountId {
    type Err = CanonicalAccountIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Display for CanonicalAccountId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Serialize for CanonicalAccountId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for CanonicalAccountId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for CanonicalAccountId {
    fn schema_name() -> String {
        "CanonicalAccountId".to_string()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <String>::json_schema(generator)
    }
}

/// Normalizes a managed-auth account identifier when it is a UUID.
pub fn normalize_optional_account_id(value: Option<String>) -> Option<String> {
    value.and_then(|account_id| CanonicalAccountId::parse(&account_id).ok().map(|id| id.raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_account_id_rejects_mixed_case() {
        assert_eq!(
            CanonicalAccountId::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            Ok(CanonicalAccountId::new(
                Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("uuid")
            ))
        );
        assert_eq!(
            CanonicalAccountId::parse("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE"),
            Err(CanonicalAccountIdError::Invalid)
        );
    }

    #[test]
    fn receipt_scope_stays_account_bound() {
        let account = CanonicalAccountId::new(
            Uuid::parse_str("20000000-0000-0000-0000-000000000002").expect("uuid"),
        );
        assert_eq!(
            account.receipt_scope("task-1"),
            "20000000-0000-0000-0000-000000000002:receipt:task-1"
        );
    }

    #[test]
    fn normalize_optional_account_id_only_accepts_canonical_uuids() {
        assert_eq!(
            normalize_optional_account_id(Some("30000000-0000-0000-0000-000000000003".to_string())),
            Some("30000000-0000-0000-0000-000000000003".to_string())
        );
        assert_eq!(
            normalize_optional_account_id(Some("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE".to_string())),
            None
        );
        assert_eq!(
            normalize_optional_account_id(Some("workspace-123".to_string())),
            None
        );
    }
}
