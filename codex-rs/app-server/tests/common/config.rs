use codex_features::FEATURES;
use codex_features::Feature;
use std::collections::BTreeMap;
use std::path::Path;

/// Writes a BrokerOnly Whisply configuration without hosted-provider routing.
pub struct ManagedWhisplyConfig {
    model: String,
    approval_policy: String,
    sandbox_mode: String,
    features: BTreeMap<Feature, bool>,
    additional_config: Vec<String>,
}

impl ManagedWhisplyConfig {
    pub fn new() -> Self {
        Self {
            model: "mock-model".to_string(),
            approval_policy: "never".to_string(),
            sandbox_mode: "read-only".to_string(),
            features: BTreeMap::new(),
            additional_config: Vec::new(),
        }
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    pub fn with_approval_policy(mut self, approval_policy: &str) -> Self {
        self.approval_policy = approval_policy.to_string();
        self
    }

    pub fn with_sandbox_mode(mut self, sandbox_mode: &str) -> Self {
        self.sandbox_mode = sandbox_mode.to_string();
        self
    }

    pub fn enable_feature(mut self, feature: Feature) -> Self {
        self.features.insert(feature, true);
        self
    }

    pub fn disable_feature(mut self, feature: Feature) -> Self {
        self.features.insert(feature, false);
        self
    }

    pub fn with_features(mut self, features: &BTreeMap<Feature, bool>) -> Self {
        self.features.extend(
            features
                .iter()
                .map(|(&feature, &enabled)| (feature, enabled)),
        );
        self
    }

    /// Adds test configuration that is unrelated to provider selection or
    /// endpoint/auth authority.
    pub fn with_additional_config(mut self, config: &str) -> Self {
        self.additional_config.push(config.to_string());
        self
    }

    pub fn write(self, codex_home: &Path) -> std::io::Result<()> {
        let feature_entries = self
            .features
            .into_iter()
            .map(|(feature, enabled)| {
                let key = FEATURES
                    .iter()
                    .find(|spec| spec.id == feature)
                    .map(|spec| spec.key)
                    .expect("feature should have a config key");
                format!("{key} = {enabled}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let feature_config = if feature_entries.is_empty() {
            String::new()
        } else {
            format!("[features]\n{feature_entries}\n\n")
        };
        let additional_config = self.additional_config.join("\n");

        std::fs::write(
            codex_home.join("config.toml"),
            format!(
                r#"
model = "{model}"
approval_policy = "{approval_policy}"
sandbox_mode = "{sandbox_mode}"
model_provider = "whisply"

{additional_config}

{feature_config}
"#,
                model = self.model,
                approval_policy = self.approval_policy,
                sandbox_mode = self.sandbox_mode,
            ),
        )?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
