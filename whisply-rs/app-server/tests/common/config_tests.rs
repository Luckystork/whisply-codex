use super::*;
use tempfile::TempDir;

#[test]
fn managed_whisply_config_has_no_direct_provider_configuration() {
    let home = TempDir::new().expect("temporary WHISPLY_HOME");
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(home.path())
        .expect("write managed Whisply config");

    let config =
        std::fs::read_to_string(home.path().join("config.toml")).expect("read config.toml");
    for expected in [
        "model = \"mock-model\"",
        "model_provider = \"whisply\"",
        "[features]\npersonality = true",
    ] {
        assert!(config.contains(expected), "config is missing {expected}");
    }
    for forbidden in [
        "[model_providers",
        "base_url",
        "openai_base_url",
        "chatgpt_base_url",
    ] {
        assert!(
            !config.contains(forbidden),
            "managed config must not contain direct provider configuration: {forbidden}"
        );
    }
}
