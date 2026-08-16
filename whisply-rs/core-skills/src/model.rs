use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::sync::Arc;

use whisply_exec_server::ExecutorFileSystem;
use whisply_exec_server::LOCAL_FS;
use whisply_protocol::protocol::Product;
pub use whisply_skills::SkillDependencies;
pub use whisply_skills::SkillInterface;
pub use whisply_skills::SkillMetadata;
pub use whisply_skills::SkillPolicy;
pub use whisply_skills::SkillToolDependency;
use whisply_utils_absolute_path::AbsolutePathBuf;
use whisply_utils_path_uri::PathUri;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillError {
    pub path: AbsolutePathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillLoadOutcome {
    pub skills: Vec<SkillMetadata>,
    pub errors: Vec<SkillError>,
    pub disabled_paths: HashSet<AbsolutePathBuf>,
    pub(crate) skill_roots: Vec<AbsolutePathBuf>,
    pub(crate) skill_root_by_path: Arc<HashMap<AbsolutePathBuf, AbsolutePathBuf>>,
    pub(crate) skill_discovery_path_by_path: Arc<HashMap<AbsolutePathBuf, AbsolutePathBuf>>,
    pub(crate) agent_plugin_skill_paths: HashSet<AbsolutePathBuf>,
    pub(crate) file_systems_by_skill_path: SkillFileSystemsByPath,
    pub(crate) implicit_skills_by_scripts_dir: Arc<HashMap<AbsolutePathBuf, SkillMetadata>>,
    pub(crate) implicit_skills_by_doc_path: Arc<HashMap<AbsolutePathBuf, SkillMetadata>>,
}

impl SkillLoadOutcome {
    pub fn is_skill_enabled(&self, skill: &SkillMetadata) -> bool {
        !self.disabled_paths.contains(&skill.path_to_skills_md)
    }

    pub fn is_skill_allowed_for_implicit_invocation(&self, skill: &SkillMetadata) -> bool {
        self.is_skill_enabled(skill) && skill.allows_implicit_invocation()
    }

    pub fn allowed_skills_for_implicit_invocation(&self) -> Vec<SkillMetadata> {
        self.skills
            .iter()
            .filter(|skill| self.is_skill_allowed_for_implicit_invocation(skill))
            .cloned()
            .collect()
    }

    pub fn skills_with_enabled(&self) -> impl Iterator<Item = (&SkillMetadata, bool)> {
        self.skills
            .iter()
            .map(|skill| (skill, self.is_skill_enabled(skill)))
    }

    pub fn with_disabled_paths(mut self, disabled_paths: HashSet<AbsolutePathBuf>) -> Self {
        self.disabled_paths = disabled_paths;
        let (by_scripts_dir, by_doc_path) = crate::build_implicit_skill_path_indexes(
            self.skills
                .iter()
                .filter(|skill| self.is_skill_enabled(skill))
                .cloned()
                .collect(),
        );
        self.implicit_skills_by_scripts_dir = Arc::new(by_scripts_dir);
        self.implicit_skills_by_doc_path = Arc::new(by_doc_path);
        self
    }

    pub fn is_agent_plugin_skill(&self, skill: &SkillMetadata) -> bool {
        self.agent_plugin_skill_paths
            .contains(&skill.path_to_skills_md)
    }

    /// Returns the discovery root that supplied a loaded skill path.
    pub fn skill_root_for_path(&self, path: &AbsolutePathBuf) -> Option<&AbsolutePathBuf> {
        self.skill_root_by_path.get(path)
    }

    /// Returns the logical path used to discover a canonical skill path.
    pub fn skill_discovery_path_for_path(
        &self,
        path: &AbsolutePathBuf,
    ) -> Option<&AbsolutePathBuf> {
        self.skill_discovery_path_by_path.get(path)
    }

    /// Returns loaded skill roots in discovery order.
    pub fn skill_roots_in_discovery_order(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.skill_roots.iter()
    }

    pub(crate) fn file_system_for_skill(
        &self,
        skill: &SkillMetadata,
    ) -> Option<Arc<dyn ExecutorFileSystem>> {
        self.file_systems_by_skill_path
            .get(&skill.path_to_skills_md)
    }

    /// Builds the legacy aggregate from independently loaded roots.
    ///
    /// This is a temporary migration boundary while host root loading moves to the skills
    /// extension. It deliberately reuses the existing merge, precedence, and deduplication logic.
    pub fn from_root_snapshots(snapshots: Vec<crate::loader::SkillRootSnapshot>) -> Self {
        crate::root_loader::merge_skill_root_snapshots(snapshots)
    }

    /// Reads one loaded skill through the filesystem that discovered it.
    pub async fn read_skill_text(&self, skill: &SkillMetadata) -> io::Result<String> {
        let fs = self
            .file_system_for_skill(skill)
            .unwrap_or_else(|| Arc::clone(&LOCAL_FS));
        let path = PathUri::from_abs_path(&skill.path_to_skills_md);
        fs.read_file_text(&path, /*sandbox*/ None).await
    }

    /// Reads a file that belongs to a loaded skill's package.
    ///
    /// Progressive disclosure means the model reads `SKILL.md` first and pulls
    /// in `references/...` only when it actually needs them, so a package has
    /// to be readable as a unit rather than one file.
    ///
    /// The path is confined to the package directory twice over. Lexically, a
    /// relative path may not be absolute, empty, or contain `.`/`..` segments.
    /// Then both the package root and the resolved file are canonicalized
    /// through the same filesystem and compared, so a symlink planted inside a
    /// package cannot be used to hand out a file from anywhere else.
    pub async fn read_skill_resource_text(
        &self,
        skill: &SkillMetadata,
        relative: &str,
    ) -> io::Result<String> {
        let denied =
            |reason: &str| io::Error::new(io::ErrorKind::PermissionDenied, reason.to_string());

        if relative.is_empty()
            || relative.starts_with('/')
            || relative
                .split('/')
                .any(|segment| matches!(segment, "" | "." | ".."))
        {
            return Err(denied("skill resource path escapes its package"));
        }

        let fs = self
            .file_system_for_skill(skill)
            .unwrap_or_else(|| Arc::clone(&LOCAL_FS));
        let skill_path = PathUri::from_abs_path(&skill.path_to_skills_md);
        let package_root = skill_path
            .parent()
            .ok_or_else(|| denied("skill package has no directory"))?;
        let resource = package_root
            .join(relative)
            .map_err(|_| denied("skill resource path is not resolvable"))?;

        let canonical_root = fs.canonicalize(&package_root, /*sandbox*/ None).await?;
        let canonical_resource = fs.canonicalize(&resource, /*sandbox*/ None).await?;
        if !canonical_resource.starts_with(&canonical_root) {
            return Err(denied("skill resource resolves outside its package"));
        }

        fs.read_file_text(&canonical_resource, /*sandbox*/ None)
            .await
    }
}

impl whisply_skills::ImplicitSkillLookup for SkillLoadOutcome {
    fn implicit_skill_for_scripts_dir(&self, path: &AbsolutePathBuf) -> Option<&SkillMetadata> {
        self.implicit_skills_by_scripts_dir.get(path)
    }

    fn implicit_skill_for_doc_path(&self, path: &AbsolutePathBuf) -> Option<&SkillMetadata> {
        self.implicit_skills_by_doc_path.get(path)
    }
}

impl whisply_skills::ExplicitSkillLookup for SkillLoadOutcome {
    fn skills(&self) -> &[SkillMetadata] {
        &self.skills
    }

    fn disabled_paths(&self) -> &HashSet<AbsolutePathBuf> {
        &self.disabled_paths
    }

    fn skill_discovery_path_for_path(&self, path: &AbsolutePathBuf) -> Option<&AbsolutePathBuf> {
        SkillLoadOutcome::skill_discovery_path_for_path(self, path)
    }

    fn is_skill_enabled(&self, skill: &SkillMetadata) -> bool {
        SkillLoadOutcome::is_skill_enabled(self, skill)
    }
}

#[derive(Clone, Default)]
pub(crate) struct SkillFileSystemsByPath {
    values: Arc<HashMap<AbsolutePathBuf, Arc<dyn ExecutorFileSystem>>>,
}

impl SkillFileSystemsByPath {
    pub(crate) fn new(values: HashMap<AbsolutePathBuf, Arc<dyn ExecutorFileSystem>>) -> Self {
        Self {
            values: Arc::new(values),
        }
    }

    fn get(&self, path: &AbsolutePathBuf) -> Option<Arc<dyn ExecutorFileSystem>> {
        self.values.get(path).map(Arc::clone)
    }

    fn retain_paths(&mut self, paths: &HashSet<AbsolutePathBuf>) {
        self.values = Arc::new(
            self.values
                .iter()
                .filter(|(path, _)| paths.contains(*path))
                .map(|(path, fs)| (path.clone(), Arc::clone(fs)))
                .collect(),
        );
    }
}

impl fmt::Debug for SkillFileSystemsByPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillFileSystemsByPath")
            .field("len", &self.values.len())
            .finish()
    }
}

pub fn filter_skill_load_outcome_for_product(
    mut outcome: SkillLoadOutcome,
    restriction_product: Option<Product>,
) -> SkillLoadOutcome {
    outcome
        .skills
        .retain(|skill| skill.matches_product_restriction_for_product(restriction_product));
    let retained_paths: HashSet<AbsolutePathBuf> = outcome
        .skills
        .iter()
        .map(|skill| skill.path_to_skills_md.clone())
        .collect();
    outcome
        .file_systems_by_skill_path
        .retain_paths(&retained_paths);
    outcome.skill_root_by_path = Arc::new(
        outcome
            .skill_root_by_path
            .iter()
            .filter(|(path, _)| retained_paths.contains(*path))
            .map(|(path, root)| (path.clone(), root.clone()))
            .collect(),
    );
    outcome.skill_discovery_path_by_path = Arc::new(
        outcome
            .skill_discovery_path_by_path
            .iter()
            .filter(|(path, _)| retained_paths.contains(*path))
            .map(|(path, discovery_path)| (path.clone(), discovery_path.clone()))
            .collect(),
    );
    outcome
        .agent_plugin_skill_paths
        .retain(|path| retained_paths.contains(path));
    let retained_roots: HashSet<AbsolutePathBuf> =
        outcome.skill_root_by_path.values().cloned().collect();
    outcome
        .skill_roots
        .retain(|root| retained_roots.contains(root));
    outcome.implicit_skills_by_scripts_dir = Arc::new(
        outcome
            .implicit_skills_by_scripts_dir
            .iter()
            .filter(|(_, skill)| skill.matches_product_restriction_for_product(restriction_product))
            .map(|(path, skill)| (path.clone(), skill.clone()))
            .collect(),
    );
    outcome.implicit_skills_by_doc_path = Arc::new(
        outcome
            .implicit_skills_by_doc_path
            .iter()
            .filter(|(_, skill)| skill.matches_product_restriction_for_product(restriction_product))
            .map(|(path, skill)| (path.clone(), skill.clone()))
            .collect(),
    );
    outcome
}
