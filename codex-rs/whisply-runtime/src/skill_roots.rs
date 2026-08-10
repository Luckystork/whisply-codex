//! Explicit Whisply skill-root discovery precedence.

use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::AccountRuntimeHome;

/// The source category for a discovered skill root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRootKind {
    ExplicitAttachment,
    ProjectWhisply,
    ProjectAgentsCompatibility,
    UserWhisply,
    InstalledPlugin,
    BundledWhisply,
    BundledCompatible,
}

/// One path plus its user-visible origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRoot {
    pub kind: SkillRootKind,
    pub path: PathBuf,
}

/// Deterministic root precedence supplied to the upstream extra-roots bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillRoots {
    roots: Vec<SkillRoot>,
}

impl SkillRoots {
    /// Builds the ordered root list without relying on the legacy product home.
    pub fn resolve(
        explicit_attachment: Option<PathBuf>,
        project_root: Option<&Path>,
        account_home: &AccountRuntimeHome,
        installed_plugin_roots: impl IntoIterator<Item = PathBuf>,
        bundled_whisply_root: PathBuf,
        bundled_compatible_root: PathBuf,
    ) -> Self {
        let mut roots = Vec::new();
        if let Some(path) = explicit_attachment {
            roots.push(SkillRoot {
                kind: SkillRootKind::ExplicitAttachment,
                path,
            });
        }
        if let Some(project_root) = project_root {
            roots.push(SkillRoot {
                kind: SkillRootKind::ProjectWhisply,
                path: project_root.join(".whisply/skills"),
            });
            roots.push(SkillRoot {
                kind: SkillRootKind::ProjectAgentsCompatibility,
                path: project_root.join(".agents/skills"),
            });
        }
        roots.push(SkillRoot {
            kind: SkillRootKind::UserWhisply,
            // User-installed skills are mutable account state. Keep them below
            // the opaque account runtime root rather than in the shared product
            // home, which may contain only immutable bundled assets.
            path: account_home.runtime_home().join("skills"),
        });
        roots.extend(installed_plugin_roots.into_iter().map(|path| SkillRoot {
            kind: SkillRootKind::InstalledPlugin,
            path,
        }));
        roots.push(SkillRoot {
            kind: SkillRootKind::BundledWhisply,
            path: bundled_whisply_root,
        });
        roots.push(SkillRoot {
            kind: SkillRootKind::BundledCompatible,
            path: bundled_compatible_root,
        });
        Self { roots }
    }

    /// Ordered roots, from highest to lowest product precedence.
    pub fn ordered(&self) -> &[SkillRoot] {
        &self.roots
    }

    /// Account-owned roots that can be passed to the stable
    /// `skills/extraRoots/set` bridge.
    ///
    /// Project, attachment, plugin, and bundled roots must be resolved through
    /// their scoped discovery paths. Passing them through the process-global
    /// bridge would leak one request's authority into unrelated CWDs.
    pub fn extra_roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .filter(|root| root.kind == SkillRootKind::UserWhisply)
            .map(|root| root.path.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccountKey;
    use tempfile::tempdir;

    #[test]
    fn roots_preserve_whisply_precedence() {
        let temporary = tempdir().expect("temporary root");
        let account_home = AccountRuntimeHome::create(
            temporary.path().join(".whisply"),
            AccountKey::parse("account_80cf07d9d8fc4dc0a434").expect("key"),
        )
        .expect("account home");
        let roots = SkillRoots::resolve(
            Some(PathBuf::from("/tmp/attached-skill")),
            Some(Path::new("/tmp/project")),
            &account_home,
            [PathBuf::from("/tmp/plugin")],
            PathBuf::from("/tmp/bundled-whisply"),
            PathBuf::from("/tmp/bundled-compatible"),
        );

        assert_eq!(roots.ordered()[0].kind, SkillRootKind::ExplicitAttachment);
        assert_eq!(roots.ordered()[1].kind, SkillRootKind::ProjectWhisply);
        assert_eq!(
            roots.ordered()[2].kind,
            SkillRootKind::ProjectAgentsCompatibility
        );
        assert_eq!(roots.ordered()[3].kind, SkillRootKind::UserWhisply);
        assert!(
            roots.ordered()[3]
                .path
                .ends_with("accounts/account_80cf07d9d8fc4dc0a434/runtime/skills")
        );
        assert_eq!(
            roots.extra_roots(),
            vec![account_home.runtime_home().join("skills")]
        );
    }
}
