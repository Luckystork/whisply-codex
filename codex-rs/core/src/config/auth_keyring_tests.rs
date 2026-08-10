use super::*;
use codex_config::ConfigLayerStack;
use codex_config::ConfigRequirements;
use codex_config::ConfigRequirementsToml;
use codex_config::FeatureRequirementsToml;
use codex_config::RequirementSource;
use codex_config::Sourced;
use codex_config::config_toml::ConfigToml;
use codex_config::config_toml::ForcedChatgptWorkspaceIds;
use codex_features::FeaturesToml;
use codex_login::AuthSourceMode;
use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

#[test]
fn resolve_bootstrap_auth_keyring_backend_kind_uses_secret_auth_storage_feature()
-> std::io::Result<()> {
    let config_toml = ConfigToml {
        features: Some(FeaturesToml::from(BTreeMap::from([(
            "secret_auth_storage".to_string(),
            true,
        )]))),
        ..Default::default()
    };
    assert_eq!(
        resolve_bootstrap_auth_keyring_backend_kind(&config_toml_load_result(
            config_toml,
            /*feature_requirements*/ None,
        )?)?,
        AuthKeyringBackendKind::Secrets
    );

    let config_toml = ConfigToml {
        features: Some(FeaturesToml::from(BTreeMap::from([(
            "secret_auth_storage".to_string(),
            false,
        )]))),
        ..Default::default()
    };
    assert_eq!(
        resolve_bootstrap_auth_keyring_backend_kind(&config_toml_load_result(
            config_toml.clone(),
            /*feature_requirements*/ None,
        )?)?,
        AuthKeyringBackendKind::Direct
    );

    let requirements = Sourced::new(
        FeatureRequirementsToml {
            entries: BTreeMap::from([("secret_auth_storage".to_string(), true)]),
        },
        RequirementSource::Unknown,
    );
    assert_eq!(
        resolve_bootstrap_auth_keyring_backend_kind(&config_toml_load_result(
            config_toml,
            Some(requirements),
        )?)?,
        AuthKeyringBackendKind::Secrets
    );

    Ok(())
}

#[test]
fn managed_auth_restrictions_intersect_workspaces_and_fail_closed() {
    let config = ConfigToml {
        forced_login_method: None,
        forced_chatgpt_workspace_id: Some(ForcedChatgptWorkspaceIds::Multiple(vec![
            " denied ".to_string(),
            " allowed ".to_string(),
        ])),
        ..Default::default()
    };
    let mut requirements = ConfigRequirements {
        allowed_login_methods: Some(Sourced::new(
            vec![ForcedLoginMethod::Chatgpt],
            RequirementSource::Unknown,
        )),
        allowed_chatgpt_workspaces: Some(Sourced::new(
            vec!["allowed".to_string()],
            RequirementSource::Unknown,
        )),
        ..Default::default()
    };

    let bootstrap_config = ConfigTomlLoadResult {
        config_toml: config.clone(),
        config_layer_stack: ConfigLayerStack::new(
            Vec::new(),
            requirements.clone(),
            ConfigRequirementsToml::default(),
        )
        .expect("requirements should stack"),
    };
    let auth_config = bootstrap_auth_config(Path::new("codex-home"), &bootstrap_config)
        .expect("policy should resolve");
    assert_eq!(auth_config.source_mode, AuthSourceMode::BrokerOnly);
    assert!(
        auth_config.validate().is_ok(),
        "broker-only bootstrap is valid without a direct login authority"
    );
    let direct_auth_config = codex_login::AuthConfig {
        source_mode: AuthSourceMode::Direct,
        ..auth_config
    };
    assert_eq!(direct_auth_config.forced_login_method, None);
    assert!(direct_auth_config.is_login_method_allowed(ForcedLoginMethod::Chatgpt));
    assert!(!direct_auth_config.is_login_method_allowed(ForcedLoginMethod::Api));
    assert_eq!(
        direct_auth_config.forced_chatgpt_workspace_id,
        Some(vec!["denied".to_string(), "allowed".to_string()])
    );
    assert_eq!(
        direct_auth_config.effective_chatgpt_workspaces(),
        Some(vec!["allowed".to_string()])
    );

    requirements.allowed_chatgpt_workspaces =
        Some(Sourced::new(Vec::new(), RequirementSource::Unknown));
    let bootstrap_config = ConfigTomlLoadResult {
        config_toml: config,
        config_layer_stack: ConfigLayerStack::new(
            Vec::new(),
            requirements,
            ConfigRequirementsToml::default(),
        )
        .expect("requirements should stack"),
    };
    let broker_only_auth_config = bootstrap_auth_config(Path::new("codex-home"), &bootstrap_config)
        .expect("broker-only bootstrap must not need a direct ChatGPT workspace");
    assert_eq!(
        broker_only_auth_config.source_mode,
        AuthSourceMode::BrokerOnly
    );
    let direct_auth_config = codex_login::AuthConfig {
        source_mode: AuthSourceMode::Direct,
        ..broker_only_auth_config
    };
    assert_eq!(
        direct_auth_config
            .validate()
            .expect_err("direct ChatGPT-only policy without an allowed workspace must fail")
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

fn config_toml_load_result(
    config_toml: ConfigToml,
    feature_requirements: Option<Sourced<FeatureRequirementsToml>>,
) -> std::io::Result<ConfigTomlLoadResult> {
    let requirements = ConfigRequirements {
        feature_requirements,
        ..Default::default()
    };
    Ok(ConfigTomlLoadResult {
        config_toml,
        config_layer_stack: ConfigLayerStack::new(
            Vec::new(),
            requirements,
            ConfigRequirementsToml::default(),
        )?,
    })
}
