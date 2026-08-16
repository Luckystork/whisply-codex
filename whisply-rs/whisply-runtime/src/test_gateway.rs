//! Test-only inherited gateway descriptor support.
//!
//! This module is intentionally absent unless the non-default `test-support`
//! feature is selected. It accepts a loopback endpoint only when a descriptor
//! arrives through the existing inherited-FD path and carries this exact typed
//! transport marker; config, request, and environment URL values remain unable
//! to select a test route.

use serde::Deserialize;
use url::Url;

use crate::CatalogVerificationKeySet;
use crate::ManagedGatewayError;

const TEST_TRANSPORT_MARKER: &str = "inherited-fd-localhost-v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestGatewayEndpointDocument {
    schema_version: u16,
    gateway_base_url: String,
    test_transport: Option<String>,
    test_catalog_key_set: Option<CatalogVerificationKeySet>,
}

pub(crate) struct TestGatewayDescriptor {
    pub(crate) api_base_url: String,
    pub(crate) catalog_key_set: CatalogVerificationKeySet,
}

pub(crate) fn parse_inherited_test_gateway_descriptor(
    bytes: &[u8],
) -> Result<Option<TestGatewayDescriptor>, ManagedGatewayError> {
    let document: TestGatewayEndpointDocument =
        serde_json::from_slice(bytes).map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
    let Some(marker) = document.test_transport else {
        return Ok(None);
    };
    if marker != TEST_TRANSPORT_MARKER || document.schema_version != 1 {
        return Err(ManagedGatewayError::InvalidDescriptor);
    }
    let catalog_key_set = document
        .test_catalog_key_set
        .ok_or(ManagedGatewayError::InvalidDescriptor)?;
    catalog_key_set
        .validate()
        .map_err(|_| ManagedGatewayError::InvalidDescriptor)?;

    let endpoint = Url::parse(&document.gateway_base_url)
        .map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
    if endpoint.scheme() != "http"
        || endpoint.host_str() != Some("127.0.0.1")
        || endpoint.port().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
    {
        return Err(ManagedGatewayError::InvalidDescriptor);
    }

    Ok(Some(TestGatewayDescriptor {
        api_base_url: format!("{}/v1", document.gateway_base_url.trim_end_matches('/')),
        catalog_key_set,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_the_exact_typed_inherited_transport_marker() {
        let catalog_key_set =
            serde_json::to_value(crate::test_catalog_key_set()).expect("serialize test key set");
        let valid = serde_json::json!({
            "schemaVersion": 1,
            "gatewayBaseUrl": "http://127.0.0.1:4444/",
            "testTransport": "inherited-fd-localhost-v1",
            "testCatalogKeySet": catalog_key_set,
        });
        assert_eq!(
            parse_inherited_test_gateway_descriptor(
                serde_json::to_vec(&valid)
                    .expect("serialize test descriptor")
                    .as_slice()
            )
            .expect("valid test descriptor")
            .map(|descriptor| descriptor.api_base_url),
            Some("http://127.0.0.1:4444/v1".to_string())
        );

        let missing_marker = br#"{"schemaVersion":1,"gatewayBaseUrl":"http://127.0.0.1:4444/"}"#;
        assert!(
            parse_inherited_test_gateway_descriptor(missing_marker)
                .expect("release descriptor")
                .is_none()
        );

        let wrong_marker = br#"{"schemaVersion":1,"gatewayBaseUrl":"http://127.0.0.1:4444/","testTransport":"wrong","testCatalogKeySet":{"schemaVersion":1,"keys":[]}}"#;
        assert!(parse_inherited_test_gateway_descriptor(wrong_marker).is_err());

        let missing_key_set = br#"{"schemaVersion":1,"gatewayBaseUrl":"http://127.0.0.1:4444/","testTransport":"inherited-fd-localhost-v1"}"#;
        assert!(parse_inherited_test_gateway_descriptor(missing_key_set).is_err());
    }
}
