//! Test-only signed model-catalog material for the inherited gateway fixture.
//!
//! The test gateway still transports this through the same broker-owned
//! descriptor route as production.  Nothing in this module is reachable from
//! a shipping build: it is compiled only with `test-support`.

use std::collections::BTreeMap;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Signer as _;
use ed25519_dalek::SigningKey;
use serde::Deserialize;

use crate::CATALOG_KEY_SET_SCHEMA_VERSION;
use crate::CatalogEnvelope;
use crate::CatalogError;
use crate::CatalogVerificationKey;
use crate::CatalogVerificationKeySet;
use crate::MODEL_CATALOG_SCHEMA_VERSION;
use crate::MODEL_CATALOG_SIGNATURE_VERSION;

const TEST_CATALOG_KEY_ID: &str = "test-catalog-2026-08";
const TEST_CATALOG_SIGNING_SEED: [u8; 32] = [0x7B; 32];
const TEST_CATALOG_TTL_SECONDS: i64 = 60 * 60;

#[derive(Deserialize)]
struct StaticCatalogFixture {
    envelope: CatalogEnvelope,
}

/// Current selector data used only by the typed managed test gateway.
///
/// The checked-in signed catalog fixture is deliberately historical: it proves
/// release-key rotation and signature verification at a fixed point in time.
/// This separate unsigned payload lets the test-only signing key keep the
/// fresh fixture aligned with the current Worker and Mac selector without
/// manufacturing or carrying a release signing key.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestSelectorFixture {
    schema_version: u16,
    catalog_revision: String,
    rate_card_revision: String,
    default_model_id: String,
    models: Vec<TestSelectorModel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestSelectorModel {
    id: String,
    display_name: String,
    short_name: String,
    description: String,
    provider_family: String,
    route_revision: String,
    input_modalities: Vec<String>,
    output_modalities: Vec<String>,
    supports_tool_calls: bool,
    supports_images: bool,
    supports_structured_output: bool,
    supports_safe_reasoning_summary: bool,
    usage_multiplier_millis: u32,
    supports_computer_use: bool,
    supports_native_visible_progress: bool,
    context_limit: u64,
    output_limit: u64,
    allowed_profiles: Vec<String>,
    allowed_reasoning_efforts: Vec<String>,
    availability: crate::ModelAvailability,
    subscription_availability: crate::SubscriptionAvailability,
    response_start_timeout_seconds: u64,
    response_idle_timeout_seconds: u64,
}

fn test_catalog_signing_key() -> SigningKey {
    SigningKey::from_bytes(&TEST_CATALOG_SIGNING_SEED)
}

fn apply_current_test_selector(catalog: &mut crate::ModelCatalog) -> Result<(), CatalogError> {
    let fixture: TestSelectorFixture = serde_json::from_str(include_str!(
        "../tests/fixtures/whisply-model-selector-test-v1.json"
    ))
    .map_err(CatalogError::Serialize)?;
    if fixture.schema_version != MODEL_CATALOG_SCHEMA_VERSION
        || fixture.catalog_revision.trim().is_empty()
        || fixture.rate_card_revision.trim().is_empty()
        || fixture.default_model_id.trim().is_empty()
        || fixture.models.len() != catalog.models.len()
    {
        return Err(CatalogError::InvalidCatalog);
    }

    let mut configured = BTreeMap::new();
    for selector in fixture.models {
        if selector.id.trim().is_empty()
            || selector.display_name.trim().is_empty()
            || selector.short_name.trim().is_empty()
            || selector.description.trim().is_empty()
            || selector.provider_family.trim().is_empty()
            || selector.route_revision.trim().is_empty()
            || selector.input_modalities.is_empty()
            || selector.output_modalities.is_empty()
            || selector.usage_multiplier_millis == 0
            || selector.context_limit == 0
            || selector.output_limit == 0
            || selector.allowed_profiles.is_empty()
            || selector.response_start_timeout_seconds == 0
            || selector.response_start_timeout_seconds > 60 * 60
            || selector.response_idle_timeout_seconds == 0
            || selector.response_idle_timeout_seconds > 60 * 60
            || configured.insert(selector.id.clone(), selector).is_some()
        {
            return Err(CatalogError::InvalidCatalog);
        }
    }

    for model in &mut catalog.models {
        let Some(selector) = configured.remove(model.id.as_str()) else {
            return Err(CatalogError::InvalidCatalog);
        };
        model.display_name = selector.display_name;
        model.short_name = selector.short_name;
        model.description = selector.description;
        model.provider_family = selector.provider_family;
        model.route_revision = selector.route_revision;
        model.capabilities.input_modalities = selector.input_modalities;
        model.capabilities.output_modalities = selector.output_modalities;
        model.capabilities.supports_tool_calls = selector.supports_tool_calls;
        model.capabilities.supports_images = selector.supports_images;
        model.capabilities.supports_structured_output = selector.supports_structured_output;
        model.capabilities.supports_safe_reasoning_summary =
            selector.supports_safe_reasoning_summary;
        model.usage_multiplier_millis = selector.usage_multiplier_millis;
        model.capabilities.supports_computer_use = selector.supports_computer_use;
        model.capabilities.supports_native_visible_progress =
            selector.supports_native_visible_progress;
        model.capabilities.context_limit = selector.context_limit;
        model.capabilities.output_limit = selector.output_limit;
        model.allowed_profiles = selector.allowed_profiles;
        model.allowed_reasoning_efforts = selector.allowed_reasoning_efforts;
        model.availability = selector.availability;
        model.subscription_availability = selector.subscription_availability;
        model.response_start_timeout_seconds = selector.response_start_timeout_seconds;
        model.response_idle_timeout_seconds = selector.response_idle_timeout_seconds;
        model.rate_card_revision = fixture.rate_card_revision.clone();
        model.recommended_default = model.id.as_str() == fixture.default_model_id;
    }
    if !configured.is_empty() {
        return Err(CatalogError::InvalidCatalog);
    }

    catalog.schema_version = MODEL_CATALOG_SCHEMA_VERSION;
    catalog.catalog_revision = format!("test-{}", fixture.catalog_revision);
    Ok(())
}

/// Public verification material carried only by the exact typed test
/// descriptor. It is deliberately independent of the release key set.
pub fn test_catalog_key_set() -> CatalogVerificationKeySet {
    let signing_key = test_catalog_signing_key();
    CatalogVerificationKeySet {
        schema_version: CATALOG_KEY_SET_SCHEMA_VERSION,
        keys: vec![CatalogVerificationKey {
            key_id: TEST_CATALOG_KEY_ID.to_string(),
            algorithm: "ed25519".to_string(),
            public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes()),
        }],
    }
}

/// Returns a current, signed projection of the checked-in twelve-model test
/// catalog. The production-signature fixture intentionally expires; retiming,
/// selector projection, and re-signing here keep test freshness real without
/// granting a generic catalog cache or changing the shipping trust root.
pub fn test_signed_catalog_envelope() -> Result<CatalogEnvelope, CatalogError> {
    let fixture: StaticCatalogFixture = serde_json::from_str(include_str!(
        "../tests/fixtures/whisply-model-catalog-v1.json"
    ))
    .map_err(CatalogError::Serialize)?;
    let now_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CatalogError::InvalidCatalog)
        .and_then(|duration| {
            i64::try_from(duration.as_secs()).map_err(|_| CatalogError::InvalidCatalog)
        })?;

    let mut catalog = fixture.envelope.catalog;
    apply_current_test_selector(&mut catalog)?;
    catalog.generated_at_unix_seconds = now_unix_seconds.saturating_sub(60);
    catalog.expires_at_unix_seconds = now_unix_seconds
        .checked_add(TEST_CATALOG_TTL_SECONDS)
        .ok_or(CatalogError::InvalidCatalog)?;
    catalog.validate_at(now_unix_seconds)?;

    let signing_key = test_catalog_signing_key();
    let signature =
        URL_SAFE_NO_PAD.encode(signing_key.sign(&catalog.canonical_payload()?).to_bytes());
    Ok(CatalogEnvelope {
        schema_version: MODEL_CATALOG_SCHEMA_VERSION,
        signature_version: MODEL_CATALOG_SIGNATURE_VERSION.to_string(),
        key_id: TEST_CATALOG_KEY_ID.to_string(),
        signature,
        catalog,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_is_current_and_verifies_with_its_descriptor_key() {
        let envelope = test_signed_catalog_envelope().expect("test catalog envelope");
        let now_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_secs()
            .try_into()
            .expect("current time fits i64");

        envelope
            .verify_at(&test_catalog_key_set(), now_unix_seconds)
            .expect("test catalog must verify");
        assert_eq!(envelope.catalog.models.len(), 12);
        assert!(
            envelope
                .catalog
                .models
                .iter()
                .any(|model| model.id.as_str() == "gpt-5.6-terra")
        );
        assert_eq!(
            envelope
                .catalog
                .models
                .iter()
                .map(|model| (model.id.as_str(), model.usage_multiplier_millis))
                .collect::<Vec<_>>(),
            vec![
                ("gpt-5.6-sol", 2_500),
                ("gpt-5.6-sol-pro", 3_500),
                ("gpt-5.6-terra", 1_300),
                ("gpt-5.6-terra-pro", 1_800),
                ("gpt-5.6-luna", 500),
                ("gpt-5.6-luna-pro", 800),
                ("haiku-4.5", 500),
                ("sonnet-5", 1_000),
                ("opus-5", 2_500),
                ("gemini-3.1-pro", 1_100),
                ("gemini-3.5-flash", 800),
                ("grok-4.5", 700),
            ]
        );
        assert_eq!(
            envelope
                .catalog
                .models
                .iter()
                .filter(|model| model.recommended_default)
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-5.6-terra"]
        );
        assert!(
            envelope
                .catalog
                .models
                .iter()
                .all(|model| model.capabilities.output_modalities == vec!["text".to_string()])
        );
        let terra = envelope
            .catalog
            .models
            .iter()
            .find(|model| model.id.as_str() == "gpt-5.6-terra")
            .expect("Terra fixture model");
        assert_eq!(terra.provider_family, "openai");
        assert_eq!(
            terra.route_revision,
            "openrouter-openai-gpt-5.6-terra-2026-08-07.1"
        );
        assert_eq!(terra.capabilities.context_limit, 1_050_000);
        assert_eq!(terra.capabilities.output_limit, 2_048);
        assert!(terra.capabilities.supports_tool_calls);
        assert_eq!(
            terra.allowed_reasoning_efforts,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        let grok = envelope
            .catalog
            .models
            .iter()
            .find(|model| model.id.as_str() == "grok-4.5")
            .expect("Grok fixture model");
        assert_eq!(grok.short_name, "Grok 4.5");
        assert!(grok.capabilities.supports_computer_use);
        assert!(!grok.capabilities.supports_native_visible_progress);
        assert_eq!(grok.response_start_timeout_seconds, 120);
    }
}
