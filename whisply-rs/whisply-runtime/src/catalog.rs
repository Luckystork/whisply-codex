//! Signed, server-authoritative Whisply model-catalog contracts.
//!
//! The gateway owns model availability, provider routing, entitlements, and
//! rate cards. This module deliberately validates a signed projection instead
//! of freezing a list of provider routes into the client.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Signature;
use ed25519_dalek::Verifier as _;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

/// The only catalog schema accepted by this runtime line.
pub const MODEL_CATALOG_SCHEMA_VERSION: u16 = 2;
/// Detached-signature format for a catalog envelope.
pub const MODEL_CATALOG_SIGNATURE_VERSION: &str = "jcs-ed25519-v1";
/// Stable schema for the in-bundle catalog verification-key resource.
pub const CATALOG_KEY_SET_SCHEMA_VERSION: u16 = 1;
/// Immutable public-key resource installed beside the managed Whisply binary.
pub const CATALOG_KEY_SET_RESOURCE_FILENAME: &str = "whisply-model-catalog-keys.json";

/// The only local Whisply profiles a signed catalog may advertise.
///
/// Profile selection remains local thread policy; this list prevents a signed
/// catalog from introducing a profile name that the runtime cannot interpret.
pub const SUPPORTED_CATALOG_PROFILES: &[&str] = &["general", "workspace"];

/// Reasoning values the direct runtime can preserve exactly on a Responses
/// request. `ultra` is intentionally absent: upstream compatibility code
/// currently normalizes it to `max` before serialization, so advertising it
/// would make the signed catalog promise a distinct value the wire cannot keep.
pub const SUPPORTED_CATALOG_REASONING_EFFORTS: &[&str] =
    &["minimal", "low", "medium", "high", "xhigh", "max"];

/// Stable Whisply model identifier stored in threads and receipts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    /// Creates a stable catalog identifier without accepting display copy.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = value.into();
        let is_valid = !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            });
        if is_valid {
            Ok(Self(value))
        } else {
            Err(CatalogError::InvalidModelId)
        }
    }

    /// Returns the stable ID, not user-visible display copy.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Server-authoritative route availability state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    Available,
    TemporarilyUnavailable,
    Deprecated,
}

/// Server-authoritative subscription state for one model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionAvailability {
    Available,
    UpgradeRequired,
    Unavailable,
}

/// Capabilities supplied by a current Whisply route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCapabilities {
    pub input_modalities: Vec<String>,
    /// Output modalities are part of the signed public catalog contract.
    ///
    /// Historical routes were text-output-only before this field was projected
    /// to every consumer. The default preserves that meaning only while a
    /// historical fixture is deserialized and re-signed by test support; a
    /// shipping schema-v1 envelope remains fail-closed at the schema boundary.
    #[serde(default = "default_text_output_modalities")]
    pub output_modalities: Vec<String>,
    pub supports_tool_calls: bool,
    pub supports_images: bool,
    pub supports_structured_output: bool,
    pub supports_safe_reasoning_summary: bool,
    #[serde(default)]
    pub supports_computer_use: bool,
    #[serde(default)]
    pub supports_native_visible_progress: bool,
    pub context_limit: u64,
    pub output_limit: u64,
}

fn default_text_output_modalities() -> Vec<String> {
    vec!["text".to_string()]
}

/// A single public Whisply model route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogModel {
    pub id: ModelId,
    pub display_name: String,
    #[serde(default)]
    pub short_name: String,
    #[serde(default)]
    pub description: String,
    pub provider_family: String,
    pub route_revision: String,
    pub capabilities: ModelCapabilities,
    pub allowed_profiles: Vec<String>,
    pub allowed_reasoning_efforts: Vec<String>,
    pub availability: ModelAvailability,
    pub subscription_availability: SubscriptionAvailability,
    pub rate_card_revision: String,
    pub usage_multiplier_millis: u32,
    pub recommended_default: bool,
    #[serde(default)]
    pub response_start_timeout_seconds: u64,
    #[serde(default)]
    pub response_idle_timeout_seconds: u64,
}

/// The payload signed and served by the Whisply control plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCatalog {
    pub schema_version: u16,
    pub catalog_revision: String,
    pub generated_at_unix_seconds: i64,
    pub expires_at_unix_seconds: i64,
    pub models: Vec<CatalogModel>,
}

impl ModelCatalog {
    /// Validates static catalog invariants before it is accepted by runtime state.
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.schema_version != MODEL_CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::UnsupportedSchemaVersion(self.schema_version));
        }
        if self.catalog_revision.trim().is_empty()
            || self.models.is_empty()
            || self.generated_at_unix_seconds < 0
            || self.expires_at_unix_seconds <= self.generated_at_unix_seconds
        {
            return Err(CatalogError::InvalidCatalog);
        }

        let mut ids = std::collections::HashSet::with_capacity(self.models.len());
        let mut default_count = 0;
        for model in &self.models {
            // `ModelId` is transparent on the wire, so deserialization alone
            // cannot enforce the constructor's stable public-ID rule.
            if ModelId::parse(model.id.as_str()).is_err()
                || !ids.insert(model.id.clone())
                || model.display_name.trim().is_empty()
                || model.short_name.trim().is_empty()
                || model.description.trim().is_empty()
                || model.provider_family.trim().is_empty()
                || model.route_revision.trim().is_empty()
                || model.rate_card_revision.trim().is_empty()
                || model.allowed_profiles.is_empty()
                || model.capabilities.input_modalities.is_empty()
                || model.capabilities.output_modalities.is_empty()
                || model.capabilities.context_limit == 0
                || model.capabilities.output_limit == 0
                || model.usage_multiplier_millis == 0
                || model.response_start_timeout_seconds == 0
                || model.response_start_timeout_seconds > 60 * 60
                || model.response_idle_timeout_seconds == 0
                || model.response_idle_timeout_seconds > 60 * 60
                || (model.capabilities.supports_native_visible_progress
                    && !model.capabilities.supports_safe_reasoning_summary)
            {
                return Err(CatalogError::InvalidCatalog);
            }
            if !valid_unique_values(&model.allowed_profiles, SUPPORTED_CATALOG_PROFILES)
                || !valid_unique_values(
                    &model.allowed_reasoning_efforts,
                    SUPPORTED_CATALOG_REASONING_EFFORTS,
                )
                // A route that cannot produce a safe reasoning summary (for
                // example the non-reasoning Haiku route) may truthfully
                // advertise no reasoning-effort controls. A reasoning-capable
                // route must advertise at least one direct-wire effort.
                || (model.capabilities.supports_safe_reasoning_summary
                    && model.allowed_reasoning_efforts.is_empty())
            {
                return Err(CatalogError::InvalidCatalog);
            }
            if model.recommended_default {
                default_count += 1;
                if model.availability != ModelAvailability::Available
                    || model.subscription_availability != SubscriptionAvailability::Available
                {
                    return Err(CatalogError::UnavailableDefault);
                }
            }
        }
        if default_count == 1 {
            Ok(())
        } else {
            Err(CatalogError::DefaultModelCount(default_count))
        }
    }

    /// Validates freshness at the caller-supplied wall-clock instant.
    pub fn validate_at(&self, now_unix_seconds: i64) -> Result<(), CatalogError> {
        self.validate()?;
        if now_unix_seconds >= self.expires_at_unix_seconds {
            return Err(CatalogError::ExpiredCatalog {
                expired_at_unix_seconds: self.expires_at_unix_seconds,
                now_unix_seconds,
            });
        }
        Ok(())
    }

    /// Produces the JCS canonical payload used for signature verification.
    pub fn canonical_payload(&self) -> Result<Vec<u8>, CatalogError> {
        canonical_json_bytes(&serde_json::to_value(self).map_err(CatalogError::Serialize)?)
    }

    /// Produces a stable digest for manifests and compatibility checks.
    pub fn digest(&self) -> Result<String, CatalogError> {
        self.validate()?;
        Ok(hex_digest(Sha256::digest(self.canonical_payload()?)))
    }
}

fn valid_unique_values(values: &[String], allowed: &[&str]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values
        .iter()
        .all(|value| allowed.contains(&value.as_str()) && seen.insert(value.as_str()))
}

/// Signed catalog envelope returned from `GET /v1/model-catalog`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogEnvelope {
    pub schema_version: u16,
    pub signature_version: String,
    pub key_id: String,
    /// Base64url (without padding) Ed25519 detached signature over `catalog`.
    pub signature: String,
    pub catalog: ModelCatalog,
}

/// Verifies one server-signed catalog without granting the verifier network or
/// account authority.
pub trait CatalogVerifier {
    /// Verifies JCS canonical payload bytes and a base64url detached signature.
    fn verify(&self, key_id: &str, payload: &[u8], signature: &str) -> Result<(), CatalogError>;
}

/// One public key in the signed, in-bundle catalog verification-key resource.
///
/// `publicKey` is exactly a 32-byte Ed25519 public key encoded as base64url
/// without padding. PEM/SPKI input is intentionally not accepted because the
/// release manifest hashes a compact, unambiguous resource representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogVerificationKey {
    pub key_id: String,
    pub algorithm: String,
    pub public_key: String,
}

/// Release-bundled verification key set for server model-catalog signatures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogVerificationKeySet {
    pub schema_version: u16,
    pub keys: Vec<CatalogVerificationKey>,
}

impl CatalogVerificationKeySet {
    /// Parses a signed-bundle resource. Environment variables and network key
    /// discovery are deliberately not part of this trust boundary.
    pub fn from_json(bytes: &[u8]) -> Result<Self, CatalogError> {
        let key_set: Self = serde_json::from_slice(bytes).map_err(CatalogError::Serialize)?;
        key_set.validate()?;
        Ok(key_set)
    }

    /// Validates resource shape and all public-key encodings before a request
    /// can use a catalog response.
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.schema_version != CATALOG_KEY_SET_SCHEMA_VERSION || self.keys.is_empty() {
            return Err(CatalogError::InvalidVerificationKeySet);
        }
        let mut key_ids = std::collections::HashSet::with_capacity(self.keys.len());
        for key in &self.keys {
            if key.key_id.trim().is_empty()
                || key.algorithm != "ed25519"
                || !key_ids.insert(key.key_id.as_str())
            {
                return Err(CatalogError::InvalidVerificationKeySet);
            }
            Self::decode_verifying_key(key)?;
        }
        Ok(())
    }

    fn decode_verifying_key(key: &CatalogVerificationKey) -> Result<VerifyingKey, CatalogError> {
        if key.public_key.contains('=') {
            return Err(CatalogError::InvalidVerificationKey);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(&key.public_key)
            .map_err(|_| CatalogError::InvalidVerificationKey)?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CatalogError::InvalidVerificationKey)?;
        VerifyingKey::from_bytes(&key_bytes).map_err(|_| CatalogError::InvalidVerificationKey)
    }
}

/// Returns the required public-key resource path for one managed runtime
/// binary. The launcher installs the binary and its sibling resource atomically
/// as part of the Runtime support tree.
pub fn catalog_key_set_path_for_runtime_binary(
    runtime_binary: &Path,
) -> Result<PathBuf, CatalogError> {
    let runtime_directory = runtime_binary
        .parent()
        .ok_or_else(|| CatalogError::InvalidKeySetResourcePath(runtime_binary.to_path_buf()))?;
    Ok(runtime_directory.join(CATALOG_KEY_SET_RESOURCE_FILENAME))
}

/// Loads the release-hashed catalog key set beside an explicit managed binary.
///
/// This accepts no shell environment override, no network discovery, and no
/// key material in arguments. It is deliberately separate from catalog
/// retrieval so callers can report a missing/corrupt bundle resource before
/// making any authenticated network request.
pub fn load_catalog_key_set_for_runtime_binary(
    runtime_binary: &Path,
) -> Result<CatalogVerificationKeySet, CatalogError> {
    let path = catalog_key_set_path_for_runtime_binary(runtime_binary)?;
    let bytes = std::fs::read(&path).map_err(|source| CatalogError::KeySetResourceIo {
        path: path.clone(),
        source,
    })?;
    CatalogVerificationKeySet::from_json(&bytes)
}

/// Loads the release-hashed catalog key set installed beside this process's
/// managed Whisply binary.
pub fn load_catalog_key_set_for_current_runtime() -> Result<CatalogVerificationKeySet, CatalogError>
{
    let runtime_binary =
        std::env::current_exe().map_err(|source| CatalogError::CurrentRuntimePath { source })?;
    load_catalog_key_set_for_runtime_binary(&runtime_binary)
}

impl CatalogVerifier for CatalogVerificationKeySet {
    fn verify(&self, key_id: &str, payload: &[u8], signature: &str) -> Result<(), CatalogError> {
        self.validate()?;
        let key = self
            .keys
            .iter()
            .find(|key| key.key_id == key_id)
            .ok_or(CatalogError::UnknownVerificationKey)?;
        if signature.contains('=') {
            return Err(CatalogError::InvalidSignature);
        }
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| CatalogError::InvalidSignature)?;
        let signature =
            Signature::from_slice(&signature_bytes).map_err(|_| CatalogError::InvalidSignature)?;
        Self::decode_verifying_key(key)?
            .verify(payload, &signature)
            .map_err(|_| CatalogError::InvalidSignature)
    }
}

impl CatalogEnvelope {
    /// Validates schema and signature before exposing a catalog to the runtime.
    ///
    /// Call [`Self::verify_at`] when the catalog will be used for a model
    /// selection or a request, so an expired server projection cannot remain
    /// eligible after process startup.
    pub fn verify<V: CatalogVerifier>(&self, verifier: &V) -> Result<(), CatalogError> {
        self.validate_envelope()?;
        verifier.verify(
            &self.key_id,
            &self.catalog.canonical_payload()?,
            &self.signature,
        )
    }

    /// Validates signature and freshness at the caller-supplied wall-clock instant.
    pub fn verify_at<V: CatalogVerifier>(
        &self,
        verifier: &V,
        now_unix_seconds: i64,
    ) -> Result<(), CatalogError> {
        self.validate_envelope()?;
        self.catalog.validate_at(now_unix_seconds)?;
        verifier.verify(
            &self.key_id,
            &self.catalog.canonical_payload()?,
            &self.signature,
        )
    }

    fn validate_envelope(&self) -> Result<(), CatalogError> {
        if self.schema_version != MODEL_CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::UnsupportedEnvelopeSchemaVersion(
                self.schema_version,
            ));
        }
        if self.signature_version != MODEL_CATALOG_SIGNATURE_VERSION {
            return Err(CatalogError::UnsupportedSignatureVersion(
                self.signature_version.clone(),
            ));
        }
        self.catalog.validate()?;
        if self.key_id.trim().is_empty() || self.signature.trim().is_empty() {
            return Err(CatalogError::MissingSignature);
        }
        Ok(())
    }
}

/// Catalog validation failures are typed so callers never render an empty model picker.
#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("model IDs must be stable lowercase Whisply catalog identifiers")]
    InvalidModelId,
    #[error("unsupported Whisply model-catalog schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("unsupported Whisply model-catalog envelope schema version {0}")]
    UnsupportedEnvelopeSchemaVersion(u16),
    #[error("unsupported Whisply model-catalog signature version `{0}`")]
    UnsupportedSignatureVersion(String),
    #[error("the Whisply model catalog is incomplete or internally inconsistent")]
    InvalidCatalog,
    #[error(
        "the Whisply model catalog must contain exactly one available recommended default, found {0}"
    )]
    DefaultModelCount(usize),
    #[error("the recommended default model is not currently available to this subscription")]
    UnavailableDefault,
    #[error("the signed Whisply model catalog is missing signature material")]
    MissingSignature,
    #[error("the bundled Whisply catalog verification key set is invalid")]
    InvalidVerificationKeySet,
    #[error("the bundled Whisply catalog verification key is invalid")]
    InvalidVerificationKey,
    #[error(
        "the Whisply runtime binary has no sibling directory for its catalog key resource: {0}"
    )]
    InvalidKeySetResourcePath(PathBuf),
    #[error(
        "failed to read the bundled Whisply catalog verification key resource {path}: {source}"
    )]
    KeySetResourceIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to resolve the managed Whisply runtime path: {source}")]
    CurrentRuntimePath {
        #[source]
        source: std::io::Error,
    },
    #[error("the signed Whisply model catalog references an unknown verification key")]
    UnknownVerificationKey,
    #[error(
        "the signed Whisply model catalog expired at {expired_at_unix_seconds}; current time is {now_unix_seconds}"
    )]
    ExpiredCatalog {
        expired_at_unix_seconds: i64,
        now_unix_seconds: i64,
    },
    #[error("failed to serialize the Whisply model catalog: {0}")]
    Serialize(serde_json::Error),
    #[error("the Whisply model-catalog signature did not validate")]
    InvalidSignature,
}

/// Exact JCS encoder shared by signed catalog verification and authenticated
/// local diagnostic frames. Keeping one encoder prevents a diagnostic path
/// from accepting a subtly different JSON/MAC representation.
pub(crate) fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, CatalogError> {
    let mut output = String::new();
    write_canonical_json(value, &mut output)?;
    Ok(output.into_bytes())
}

/// A deliberately narrow RFC 8785 / JCS writer for the catalog's typed JSON
/// domain. Catalog values use strings, booleans, signed/unsigned integers,
/// arrays, and objects only; floats are rejected to avoid a non-JCS numeric
/// representation crossing the signature boundary.
fn write_canonical_json(value: &Value, output: &mut String) -> Result<(), CatalogError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => {
            if value.is_f64() {
                return Err(CatalogError::InvalidCatalog);
            }
            output.push_str(&value.to_string());
        }
        Value::String(value) => {
            output.push_str(&serde_json::to_string(value).map_err(CatalogError::Serialize)?);
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            let mut members: Vec<_> = values.iter().collect();
            members.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            output.push('{');
            for (index, (key, value)) in members.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(CatalogError::Serialize)?);
                output.push(':');
                write_canonical_json(value, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

pub(crate) fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use ed25519_dalek::SigningKey;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct StaticCatalogFixture {
        key_set: CatalogVerificationKeySet,
        envelope: CatalogEnvelope,
    }

    struct AcceptingVerifier;

    impl CatalogVerifier for AcceptingVerifier {
        fn verify(&self, _: &str, _: &[u8], _: &str) -> Result<(), CatalogError> {
            Ok(())
        }
    }

    fn catalog() -> ModelCatalog {
        ModelCatalog {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            catalog_revision: "catalog-2026-08-07".to_string(),
            generated_at_unix_seconds: 1_786_126_400,
            expires_at_unix_seconds: 1_786_212_800,
            models: vec![CatalogModel {
                id: ModelId::parse("gpt-5.6-terra").expect("model id"),
                display_name: "GPT 5.6 Terra".to_string(),
                short_name: "GPT Terra".to_string(),
                description: "OpenAI fast model for everyday work".to_string(),
                provider_family: "openai".to_string(),
                route_revision: "route-1".to_string(),
                capabilities: ModelCapabilities {
                    input_modalities: vec!["text".to_string(), "image".to_string()],
                    output_modalities: vec!["text".to_string()],
                    supports_tool_calls: true,
                    supports_images: true,
                    supports_structured_output: true,
                    supports_safe_reasoning_summary: true,
                    supports_computer_use: true,
                    supports_native_visible_progress: false,
                    context_limit: 1_050_000,
                    output_limit: 2_048,
                },
                allowed_profiles: vec!["general".to_string(), "workspace".to_string()],
                allowed_reasoning_efforts: vec!["medium".to_string()],
                availability: ModelAvailability::Available,
                subscription_availability: SubscriptionAvailability::Available,
                rate_card_revision: "rate-card-1".to_string(),
                usage_multiplier_millis: 1_300,
                recommended_default: true,
                response_start_timeout_seconds: 120,
                response_idle_timeout_seconds: 120,
            }],
        }
    }

    #[test]
    fn catalog_requires_one_available_default() {
        let catalog = catalog();
        assert!(catalog.validate_at(1_786_126_401).is_ok());
        assert_eq!(catalog.digest().expect("digest").len(), 64);
    }

    #[test]
    fn catalog_rejects_expired_projection() {
        let catalog = catalog();
        assert!(matches!(
            catalog.validate_at(1_786_212_800),
            Err(CatalogError::ExpiredCatalog { .. })
        ));
    }

    #[test]
    fn catalog_rejects_an_invalid_deserialized_public_model_id() {
        let mut invalid_id_catalog = catalog();
        invalid_id_catalog.models[0].id =
            serde_json::from_value(serde_json::json!("GPT-5.6-Terra"))
                .expect("transparent model id deserializes before validation");

        assert!(matches!(
            invalid_id_catalog.validate(),
            Err(CatalogError::InvalidCatalog)
        ));
    }

    #[test]
    fn legacy_catalog_without_output_modalities_defaults_to_text() {
        let mut value = serde_json::to_value(catalog()).expect("serialize catalog");
        value["models"][0]["capabilities"]
            .as_object_mut()
            .expect("capabilities object")
            .remove("outputModalities");

        let decoded: ModelCatalog =
            serde_json::from_value(value).expect("legacy catalog still deserializes");
        assert_eq!(
            decoded.models[0].capabilities.output_modalities,
            vec!["text".to_string()]
        );
        assert!(decoded.validate_at(1_786_126_401).is_ok());
    }

    #[test]
    fn catalog_wire_objects_reject_unknown_fields() {
        let mut value = serde_json::to_value(catalog()).expect("serialize catalog");
        value["unreviewedCapability"] = serde_json::json!(true);

        assert!(serde_json::from_value::<ModelCatalog>(value).is_err());
    }

    #[test]
    fn catalog_rejects_profiles_or_efforts_the_direct_runtime_cannot_preserve() {
        let mut duplicate_profile_catalog = catalog();
        duplicate_profile_catalog.models[0].allowed_profiles =
            vec!["general".to_string(), "general".to_string()];
        assert!(matches!(
            duplicate_profile_catalog.validate(),
            Err(CatalogError::InvalidCatalog)
        ));

        let mut unsupported_profile_catalog = catalog();
        unsupported_profile_catalog.models[0].allowed_profiles = vec!["future_profile".to_string()];
        assert!(matches!(
            unsupported_profile_catalog.validate(),
            Err(CatalogError::InvalidCatalog)
        ));

        let mut unsupported_effort_catalog = catalog();
        unsupported_effort_catalog.models[0].allowed_reasoning_efforts = vec!["ultra".to_string()];
        assert!(matches!(
            unsupported_effort_catalog.validate(),
            Err(CatalogError::InvalidCatalog)
        ));

        let mut duplicate_effort_catalog = catalog();
        duplicate_effort_catalog.models[0].allowed_reasoning_efforts =
            vec!["medium".to_string(), "medium".to_string()];
        assert!(matches!(
            duplicate_effort_catalog.validate(),
            Err(CatalogError::InvalidCatalog)
        ));
    }

    #[test]
    fn non_reasoning_routes_may_truthfully_publish_no_reasoning_efforts() {
        let mut haiku_catalog = catalog();
        haiku_catalog.models[0].id = ModelId::parse("haiku-4.5").expect("model id");
        haiku_catalog.models[0].display_name = "Claude Haiku 4.5".to_string();
        haiku_catalog.models[0].provider_family = "anthropic".to_string();
        haiku_catalog.models[0].allowed_reasoning_efforts.clear();
        haiku_catalog.models[0]
            .capabilities
            .supports_safe_reasoning_summary = false;

        assert!(haiku_catalog.validate().is_ok());
    }

    #[test]
    fn reasoning_capable_routes_may_not_omit_effort_controls() {
        let mut reasoning_catalog = catalog();
        reasoning_catalog.models[0]
            .allowed_reasoning_efforts
            .clear();

        assert!(matches!(
            reasoning_catalog.validate(),
            Err(CatalogError::InvalidCatalog)
        ));
    }

    #[test]
    fn envelope_requires_the_pinned_signature_contract() {
        let envelope = CatalogEnvelope {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            signature_version: MODEL_CATALOG_SIGNATURE_VERSION.to_string(),
            key_id: "catalog-key-1".to_string(),
            signature: "detached-signature".to_string(),
            catalog: catalog(),
        };

        assert!(
            envelope
                .verify_at(&AcceptingVerifier, 1_786_126_401)
                .is_ok()
        );
    }

    #[test]
    fn canonical_payload_sorts_object_keys_for_cross_language_signing() {
        let value = serde_json::json!({"z": 1, "a": [true, "x"]});
        assert_eq!(
            String::from_utf8(canonical_json_bytes(&value).expect("canonical payload"))
                .expect("utf8"),
            r#"{"a":[true,"x"],"z":1}"#
        );
    }

    #[test]
    fn bundled_key_set_verifies_a_base64url_jcs_signature() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let key_set = CatalogVerificationKeySet {
            schema_version: CATALOG_KEY_SET_SCHEMA_VERSION,
            keys: vec![CatalogVerificationKey {
                key_id: "catalog-2026-08".to_string(),
                algorithm: "ed25519".to_string(),
                public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes()),
            }],
        };
        let catalog = catalog();
        let signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(&catalog.canonical_payload().expect("payload"))
                .to_bytes(),
        );
        let envelope = CatalogEnvelope {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            signature_version: MODEL_CATALOG_SIGNATURE_VERSION.to_string(),
            key_id: "catalog-2026-08".to_string(),
            signature,
            catalog,
        };

        assert!(envelope.verify_at(&key_set, 1_786_126_401).is_ok());
    }

    #[test]
    fn historical_worker_catalog_fixture_retains_rotated_key_signature_and_is_rejected_by_v2() {
        let fixture: StaticCatalogFixture = serde_json::from_str(include_str!(
            "../tests/fixtures/whisply-model-catalog-v1.json"
        ))
        .expect("shared static catalog fixture");
        let raw_fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/whisply-model-catalog-v1.json"
        ))
        .expect("raw historical catalog fixture");
        let raw_envelope = raw_fixture
            .get("envelope")
            .and_then(Value::as_object)
            .expect("historical envelope");
        let raw_key_id = raw_envelope
            .get("keyId")
            .and_then(Value::as_str)
            .expect("historical key id");
        let raw_signature = raw_envelope
            .get("signature")
            .and_then(Value::as_str)
            .expect("historical signature");
        let raw_catalog = raw_envelope.get("catalog").expect("historical catalog");

        assert_eq!(fixture.key_set.keys.len(), 2);
        assert_eq!(fixture.envelope.catalog.models.len(), 12);
        assert_eq!(
            fixture
                .envelope
                .catalog
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "gpt-5.6-sol",
                "gpt-5.6-sol-pro",
                "gpt-5.6-terra",
                "gpt-5.6-terra-pro",
                "gpt-5.6-luna",
                "gpt-5.6-luna-pro",
                "haiku-4.5",
                "sonnet-5",
                "opus-5",
                "gemini-3.1-pro",
                "gemini-3.5-flash",
                "grok-4.5",
            ]
        );
        assert_eq!(fixture.envelope.key_id, "catalog-rotation-2026-08");
        fixture
            .key_set
            .verify(
                raw_key_id,
                &canonical_json_bytes(raw_catalog).expect("historical JCS"),
                raw_signature,
            )
            .expect("rotated historical signature");
        assert!(matches!(
            fixture.envelope.verify_at(&fixture.key_set, 1_786_126_401),
            Err(CatalogError::UnsupportedEnvelopeSchemaVersion(1))
        ));

        let retired_only = CatalogVerificationKeySet {
            schema_version: fixture.key_set.schema_version,
            keys: vec![fixture.key_set.keys[0].clone()],
        };
        assert!(matches!(
            retired_only.verify(
                raw_key_id,
                &canonical_json_bytes(raw_catalog).expect("historical JCS"),
                raw_signature,
            ),
            Err(CatalogError::UnknownVerificationKey)
        ));

        let haiku = fixture
            .envelope
            .catalog
            .models
            .iter()
            .find(|model| model.id.as_str() == "haiku-4.5")
            .expect("non-reasoning Haiku fixture route");
        assert!(!haiku.capabilities.supports_safe_reasoning_summary);
        assert!(haiku.allowed_reasoning_efforts.is_empty());

        let direct_efforts = ["minimal", "low", "medium", "high", "xhigh", "max"];
        for model in fixture
            .envelope
            .catalog
            .models
            .iter()
            .filter(|model| model.id.as_str() != "haiku-4.5")
        {
            assert_eq!(
                model.allowed_reasoning_efforts,
                direct_efforts
                    .iter()
                    .map(|effort| (*effort).to_string())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn runtime_fixture_stays_semantically_identical_to_the_worker_contract_fixture() {
        let runtime: StaticCatalogFixture = serde_json::from_str(include_str!(
            "../tests/fixtures/whisply-model-catalog-v1.json"
        ))
        .expect("runtime static catalog fixture");
        let worker: StaticCatalogFixture = serde_json::from_str(include_str!(
            "../../../../contracts/fixtures/whisply-model-catalog-v1.json"
        ))
        .expect("worker static catalog fixture");

        assert_eq!(runtime.key_set, worker.key_set);
        assert_eq!(runtime.envelope.key_id, worker.envelope.key_id);
        assert_eq!(runtime.envelope.signature, worker.envelope.signature);
        assert_eq!(
            runtime
                .envelope
                .catalog
                .canonical_payload()
                .expect("runtime JCS"),
            worker
                .envelope
                .catalog
                .canonical_payload()
                .expect("worker JCS")
        );
    }

    #[test]
    fn key_set_resource_is_resolved_only_beside_the_managed_binary() {
        let temporary = tempfile::tempdir().expect("temporary runtime");
        let runtime_binary = temporary.path().join("whisply");
        let resource = catalog_key_set_path_for_runtime_binary(&runtime_binary)
            .expect("sibling resource path");

        assert_eq!(
            resource,
            temporary.path().join(CATALOG_KEY_SET_RESOURCE_FILENAME)
        );
        assert!(!resource.to_string_lossy().contains("WHISPLY_"));
    }
}
