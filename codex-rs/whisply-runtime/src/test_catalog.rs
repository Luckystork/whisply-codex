//! Test-only signed model-catalog material for the inherited gateway fixture.
//!
//! The test gateway still transports this through the same broker-owned
//! descriptor route as production.  Nothing in this module is reachable from
//! a shipping build: it is compiled only with `test-support`.

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

fn test_catalog_signing_key() -> SigningKey {
    SigningKey::from_bytes(&TEST_CATALOG_SIGNING_SEED)
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
/// catalog. The production fixture intentionally expires; retiming and
/// re-signing here keep test freshness real without granting a generic catalog
/// cache or changing the shipping trust root.
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
    catalog.catalog_revision = "test-catalog-runtime-v1".to_string();
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
    }
}
