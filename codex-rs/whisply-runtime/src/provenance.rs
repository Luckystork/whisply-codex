//! Version and source-lineage data emitted by diagnostics and release manifests.

use serde::Deserialize;
use serde::Serialize;

/// The reviewed released upstream source used for this Whisply fork.
pub const UPSTREAM_TAG: &str = "rust-v0.147.0";
/// The peeled upstream release commit, not the annotated tag object.
pub const UPSTREAM_COMMIT: &str = "be6e8eac029b183056b7e4402879f15d2c85f61b";
/// Public downstream runtime line. The build manifest records the exact fork SHA.
pub const WHISPLY_RUNTIME_VERSION: &str = "0.147.0-wsply.1";

/// Immutable lineage values that clients compare with their bundled manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLineage {
    pub runtime_version: String,
    pub upstream_tag: String,
    pub upstream_commit: String,
    pub fork_commit: String,
    pub protocol_schema_sha256: String,
    pub tool_registry_sha256: String,
}

impl RuntimeLineage {
    /// Creates runtime lineage from release-manifest values already verified by the launcher.
    pub fn new(
        fork_commit: String,
        protocol_schema_sha256: String,
        tool_registry_sha256: String,
    ) -> Self {
        Self {
            runtime_version: WHISPLY_RUNTIME_VERSION.to_string(),
            upstream_tag: UPSTREAM_TAG.to_string(),
            upstream_commit: UPSTREAM_COMMIT.to_string(),
            fork_commit,
            protocol_schema_sha256,
            tool_registry_sha256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineage_retains_the_exact_reviewed_upstream_commit() {
        let lineage = RuntimeLineage::new(
            "fork-sha".to_string(),
            "schema-sha".to_string(),
            "registry-sha".to_string(),
        );

        assert_eq!(lineage.upstream_tag, "rust-v0.147.0");
        assert_eq!(
            lineage.upstream_commit,
            "be6e8eac029b183056b7e4402879f15d2c85f61b"
        );
    }
}
