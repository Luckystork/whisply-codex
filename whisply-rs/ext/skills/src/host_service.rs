use std::collections::HashMap;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::Weak;

use futures::StreamExt;
use tokio::sync::OnceCell;
use tokio::sync::Semaphore;
use tracing::info;
use tracing::instrument;
use tracing::warn;
use whisply_config::ConfigLayerStack;
use whisply_exec_server::ExecutorFileSystem;
use whisply_protocol::protocol::Product;
use whisply_protocol::protocol::SkillScope;
use whisply_utils_absolute_path::AbsolutePathBuf;
use whisply_utils_plugins::PluginIdentity;
use whisply_utils_plugins::PluginSkillRoot;
use whisply_utils_plugins::SkillDiscoveryMode;

use whisply_config::SkillsConfig;
use whisply_core_skills::PluginSkillSnapshots;
use whisply_core_skills::SkillError;
use whisply_core_skills::SkillLoadOutcome;
use whisply_core_skills::config_rules::SkillConfigRules;
use whisply_core_skills::config_rules::resolve_disabled_skill_paths;
use whisply_core_skills::config_rules::skill_config_rules_from_stack;
use whisply_core_skills::loader::MAX_CONCURRENT_ROOT_SCANS;
use whisply_core_skills::loader::SkillRoot;
use whisply_core_skills::loader::SkillRootSnapshot;
use whisply_core_skills::loader::load_skill_root_snapshot;
use whisply_skills::install_system_skills;

use crate::HostSkillsSnapshot;
#[cfg(not(test))]
use crate::host_roots::resolve_skill_roots;
#[cfg(test)]
use crate::host_roots::resolve_skill_roots_with_home_dir;
use crate::loader::HostSkillRoot;
use crate::loader::load_host_skill_root;

#[derive(Debug, Clone)]
pub struct HostSkillsLoadInput {
    pub cwd: AbsolutePathBuf,
    pub effective_skill_roots: Vec<PluginSkillRoot>,
    pub config_layer_stack: ConfigLayerStack,
    pub bundled_skills_enabled: bool,
    plugin_skill_snapshots: Option<PluginSkillSnapshots>,
}

impl HostSkillsLoadInput {
    pub fn new(
        cwd: AbsolutePathBuf,
        effective_skill_roots: Vec<PluginSkillRoot>,
        config_layer_stack: ConfigLayerStack,
        bundled_skills_enabled: bool,
    ) -> Self {
        Self {
            cwd,
            effective_skill_roots,
            config_layer_stack,
            bundled_skills_enabled,
            plugin_skill_snapshots: None,
        }
    }

    /// Attaches plugin skill snapshots parsed during plugin loading, when available.
    pub fn with_plugin_skill_snapshots(
        mut self,
        plugin_skill_snapshots: Option<PluginSkillSnapshots>,
    ) -> Self {
        self.plugin_skill_snapshots = plugin_skill_snapshots;
        self
    }
}

/// Owns host skill discovery, immutable snapshots, cache invalidation, and extra roots.
///
/// Source-specific model exposure remains the responsibility of the skills extension.
pub struct HostSkillsService {
    codex_home: AbsolutePathBuf,
    #[cfg(test)]
    test_home_dir: AbsolutePathBuf,
    restriction_product: Option<Product>,
    extra_roots: RwLock<Vec<AbsolutePathBuf>>,
    cache_by_cwd: RwLock<HashMap<AbsolutePathBuf, HostSkillsSnapshot>>,
    cache_by_config: RwLock<HashMap<ConfigSkillsCacheKey, Arc<OnceCell<HostSkillsSnapshot>>>>,
    // Shared across cwds so root scheduling cannot multiply per-root I/O fanout.
    root_scan_slots: Arc<Semaphore>,
}

impl HostSkillsService {
    pub fn new(codex_home: AbsolutePathBuf, bundled_skills_enabled: bool) -> Self {
        Self::new_with_restriction_product(codex_home, bundled_skills_enabled, Some(Product::Codex))
    }

    pub fn new_with_restriction_product(
        codex_home: AbsolutePathBuf,
        bundled_skills_enabled: bool,
        restriction_product: Option<Product>,
    ) -> Self {
        #[cfg(test)]
        let test_home_dir = codex_home.clone();
        let service = Self {
            codex_home,
            #[cfg(test)]
            test_home_dir,
            restriction_product,
            extra_roots: RwLock::new(Vec::new()),
            cache_by_cwd: RwLock::new(HashMap::new()),
            cache_by_config: RwLock::new(HashMap::new()),
            root_scan_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
        };
        // The cache is shared by every process using this CODEX_HOME. Disabled services filter
        // system roots when loading rather than mutating shared state.
        if bundled_skills_enabled {
            service.ensure_system_skills_installed();
        }
        service
    }

    pub fn set_extra_roots(&self, extra_roots: Vec<AbsolutePathBuf>) {
        {
            let mut roots = self
                .extra_roots
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *roots = extra_roots;
        }
        self.clear_cache();
    }

    /// Replaces runtime roots supplied by an external client with account-owned
    /// user skill roots only.
    ///
    /// Project roots are resolved for each request CWD after the project-trust
    /// check. They must never be installed as process-global extra roots.
    pub fn set_account_extra_roots(
        &self,
        extra_roots: Vec<AbsolutePathBuf>,
    ) -> Vec<AbsolutePathBuf> {
        let extra_roots = extra_roots
            .into_iter()
            .filter_map(|root| self.canonical_account_skill_descendant(&root))
            .collect::<Vec<_>>();
        self.set_extra_roots(extra_roots.clone());
        extra_roots
    }

    fn canonical_account_skill_descendant(
        &self,
        root: &AbsolutePathBuf,
    ) -> Option<AbsolutePathBuf> {
        let canonical_home = self.codex_home.canonicalize().ok()?;
        let account_skills_root = self.codex_home.join("skills");
        let canonical_account_skills_root = match account_skills_root.canonicalize() {
            Ok(root) => root,
            Err(_) if root == &account_skills_root => return Some(root.clone()),
            Err(_) => return None,
        };
        if !canonical_account_skills_root
            .as_path()
            .starts_with(canonical_home.as_path())
        {
            return None;
        }

        root.canonicalize().ok().filter(|canonical_root| {
            canonical_root
                .as_path()
                .starts_with(canonical_account_skills_root.as_path())
        })
    }

    /// Load skills for an already-constructed [`Config`], avoiding any additional config-layer
    /// loading.
    ///
    /// This path uses a cache keyed by the effective skill-relevant config state rather than just
    /// cwd so role-local and session-local skill overrides cannot bleed across sessions that happen
    /// to share a directory.
    #[instrument(
        name = "skills_for_config",
        level = "info",
        skip_all,
        fields(otel.name = "skills_for_config")
    )]
    pub async fn snapshot_for_config(
        &self,
        input: &HostSkillsLoadInput,
        fs: Option<Arc<dyn ExecutorFileSystem>>,
    ) -> HostSkillsSnapshot {
        let roots = self.skill_roots_for_config(input, fs).await;
        let skill_config_rules = skill_config_rules_from_stack(&input.config_layer_stack);
        let cache_key = config_skills_cache_key(
            &roots,
            &skill_config_rules,
            input.plugin_skill_snapshots.as_ref(),
        );
        if let Some(snapshot) = self.cached_snapshot_for_config(&cache_key) {
            return snapshot;
        }

        self.snapshot_for_skill_roots(
            input,
            roots,
            &skill_config_rules,
            cache_key,
            /*force_reload*/ false,
        )
        .await
    }

    pub async fn skill_roots_for_config(
        &self,
        input: &HostSkillsLoadInput,
        fs: Option<Arc<dyn ExecutorFileSystem>>,
    ) -> Vec<SkillRoot> {
        if input.bundled_skills_enabled {
            self.ensure_system_skills_installed();
        }
        let mut roots = self.resolve_roots(input, fs).await;
        if !input.bundled_skills_enabled {
            roots.retain(|root| root.scope != SkillScope::System);
        }
        roots
    }

    pub async fn snapshot_for_cwd(
        &self,
        input: &HostSkillsLoadInput,
        force_reload: bool,
        fs: Option<Arc<dyn ExecutorFileSystem>>,
    ) -> HostSkillsSnapshot {
        let bundled_skills_enabled = bundled_skills_enabled_from_stack(&input.config_layer_stack);
        if bundled_skills_enabled {
            self.ensure_system_skills_installed();
        }
        let use_cwd_cache = fs.is_some();
        let cache_snapshot_by_cwd = use_cwd_cache && input.effective_skill_roots.is_empty();
        if cache_snapshot_by_cwd
            && !force_reload
            && let Some(snapshot) = self.cached_snapshot_for_cwd(&input.cwd)
        {
            return snapshot;
        }

        let mut roots = self.resolve_roots(input, fs.clone()).await;
        if !bundled_skills_enabled {
            roots.retain(|root| root.scope != SkillScope::System);
        }
        let skill_config_rules = skill_config_rules_from_stack(&input.config_layer_stack);
        let snapshot = if use_cwd_cache {
            let cache_key = config_skills_cache_key(
                &roots,
                &skill_config_rules,
                input.plugin_skill_snapshots.as_ref(),
            );
            self.snapshot_for_skill_roots(
                input,
                roots,
                &skill_config_rules,
                cache_key,
                force_reload,
            )
            .await
        } else {
            HostSkillsSnapshot::new(Arc::new(
                self.build_skill_outcome(input, roots, &skill_config_rules)
                    .await,
            ))
        };
        if cache_snapshot_by_cwd {
            let mut cache = self
                .cache_by_cwd
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.insert(input.cwd.clone(), snapshot.clone());
        }
        snapshot
    }

    async fn resolve_roots(
        &self,
        input: &HostSkillsLoadInput,
        fs: Option<Arc<dyn ExecutorFileSystem>>,
    ) -> Vec<SkillRoot> {
        #[cfg(test)]
        {
            return resolve_skill_roots_with_home_dir(
                fs,
                &input.config_layer_stack,
                &input.cwd,
                Some(&self.test_home_dir),
                input.effective_skill_roots.clone(),
                self.extra_roots(),
            )
            .await;
        }

        #[cfg(not(test))]
        {
            resolve_skill_roots(
                fs,
                &input.config_layer_stack,
                &input.cwd,
                input.effective_skill_roots.clone(),
                self.extra_roots(),
            )
            .await
        }
    }

    async fn snapshot_for_skill_roots(
        &self,
        input: &HostSkillsLoadInput,
        roots: Vec<SkillRoot>,
        skill_config_rules: &SkillConfigRules,
        cache_key: ConfigSkillsCacheKey,
        force_reload: bool,
    ) -> HostSkillsSnapshot {
        let snapshot_cell = {
            let mut cache = self
                .cache_by_config
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if force_reload {
                let snapshot_cell = Arc::new(OnceCell::new());
                cache.insert(cache_key, Arc::clone(&snapshot_cell));
                snapshot_cell
            } else {
                Arc::clone(
                    cache
                        .entry(cache_key)
                        .or_insert_with(|| Arc::new(OnceCell::new())),
                )
            }
        };

        snapshot_cell
            .get_or_init(|| async {
                HostSkillsSnapshot::new(Arc::new(
                    self.build_skill_outcome(input, roots, skill_config_rules)
                        .await,
                ))
            })
            .await
            .clone()
    }

    #[instrument(level = "trace", skip_all)]
    async fn build_skill_outcome(
        &self,
        input: &HostSkillsLoadInput,
        roots: Vec<SkillRoot>,
        skill_config_rules: &SkillConfigRules,
    ) -> SkillLoadOutcome {
        let plugin_skill_snapshots = input.plugin_skill_snapshots.as_ref();
        let mut indexed_snapshots = futures::stream::iter(roots.into_iter().enumerate())
            .map(|(root_index, root)| async move {
                let _root_scan_slot = self
                    .root_scan_slots
                    .acquire()
                    .await
                    .unwrap_or_else(|_| unreachable!());
                let use_legacy_loader = root.plugin_identity.is_some()
                    || root.plugin_namespace.is_some()
                    || root.plugin_root.is_some()
                    || root.discovery_mode != SkillDiscoveryMode::Recursive;
                let snapshot = if use_legacy_loader {
                    load_skill_root_snapshot(root, plugin_skill_snapshots).await
                } else {
                    let snapshot = load_host_skill_root(HostSkillRoot {
                        path: root.path,
                        scope: root.scope,
                        file_system: root.file_system,
                        plugin_root: root.plugin_root,
                    })
                    .await;
                    SkillRootSnapshot::new(
                        snapshot.root,
                        snapshot.skills,
                        snapshot.skill_discovery_path_by_path,
                        snapshot
                            .errors
                            .into_iter()
                            .map(|error| SkillError {
                                path: error.path,
                                message: error.message,
                            })
                            .collect(),
                        snapshot.file_system,
                    )
                };
                (root_index, snapshot)
            })
            .buffer_unordered(MAX_CONCURRENT_ROOT_SCANS)
            .collect::<Vec<_>>()
            .await;
        indexed_snapshots.sort_unstable_by_key(|(root_index, _)| *root_index);
        let outcome = SkillLoadOutcome::from_root_snapshots(
            indexed_snapshots
                .into_iter()
                .map(|(_, snapshot)| snapshot)
                .collect(),
        );
        let outcome = whisply_core_skills::filter_skill_load_outcome_for_product(
            outcome,
            self.restriction_product,
        );
        let disabled_paths = resolve_disabled_skill_paths(&outcome.skills, skill_config_rules);
        outcome.with_disabled_paths(disabled_paths)
    }

    pub fn clear_cache(&self) {
        let cleared_cwd = {
            let mut cache = self
                .cache_by_cwd
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cleared = cache.len();
            cache.clear();
            cleared
        };
        let cleared_config = {
            let mut cache = self
                .cache_by_config
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cleared = cache.len();
            cache.clear();
            cleared
        };
        let cleared = cleared_cwd + cleared_config;
        info!("skills cache cleared ({cleared} entries)");
    }

    fn cached_snapshot_for_cwd(&self, cwd: &AbsolutePathBuf) -> Option<HostSkillsSnapshot> {
        match self.cache_by_cwd.read() {
            Ok(cache) => cache.get(cwd).cloned(),
            Err(err) => err.into_inner().get(cwd).cloned(),
        }
    }

    fn cached_snapshot_for_config(
        &self,
        cache_key: &ConfigSkillsCacheKey,
    ) -> Option<HostSkillsSnapshot> {
        match self.cache_by_config.read() {
            Ok(cache) => cache
                .get(cache_key)
                .and_then(|snapshot| snapshot.get())
                .cloned(),
            Err(err) => err
                .into_inner()
                .get(cache_key)
                .and_then(|snapshot| snapshot.get())
                .cloned(),
        }
    }

    fn extra_roots(&self) -> Vec<AbsolutePathBuf> {
        match self.extra_roots.read() {
            Ok(roots) => roots.clone(),
            Err(err) => err.into_inner().clone(),
        }
    }

    fn ensure_system_skills_installed(&self) {
        if let Err(err) = install_system_skills(&self.codex_home) {
            tracing::error!("failed to install system skills: {err}");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConfigSkillsCacheKey {
    roots: Vec<ConfigSkillRootCacheKey>,
    skill_config_rules: SkillConfigRules,
    plugin_skill_snapshots: Option<PluginSkillSnapshots>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConfigSkillRootCacheKey {
    path: AbsolutePathBuf,
    scope_rank: u8,
    plugin_identity: Option<PluginIdentity>,
    plugin_namespace: Option<String>,
    file_system: FileSystemIdentity,
}

#[derive(Debug, Clone)]
struct FileSystemIdentity(Weak<dyn ExecutorFileSystem>);

impl PartialEq for FileSystemIdentity {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for FileSystemIdentity {}

impl Hash for FileSystemIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0.as_ptr() as *const ()).hash(state);
    }
}

pub fn bundled_skills_enabled_from_stack(
    config_layer_stack: &whisply_config::ConfigLayerStack,
) -> bool {
    let effective_config = config_layer_stack.effective_config();
    let Some(skills_value) = effective_config
        .as_table()
        .and_then(|table| table.get("skills"))
    else {
        return true;
    };

    let skills: SkillsConfig = match skills_value.clone().try_into() {
        Ok(skills) => skills,
        Err(err) => {
            warn!("invalid skills config: {err}");
            return true;
        }
    };

    skills.bundled.unwrap_or_default().enabled
}

fn config_skills_cache_key(
    roots: &[SkillRoot],
    skill_config_rules: &SkillConfigRules,
    plugin_skill_snapshots: Option<&PluginSkillSnapshots>,
) -> ConfigSkillsCacheKey {
    ConfigSkillsCacheKey {
        roots: roots
            .iter()
            .map(|root| {
                let scope_rank = match root.scope {
                    SkillScope::Repo => 0,
                    SkillScope::User => 1,
                    SkillScope::System => 2,
                    SkillScope::Admin => 3,
                };
                ConfigSkillRootCacheKey {
                    path: root.path.clone(),
                    scope_rank,
                    plugin_identity: root.plugin_identity.clone(),
                    plugin_namespace: root.plugin_namespace.clone(),
                    file_system: FileSystemIdentity(Arc::downgrade(&root.file_system)),
                }
            })
            .collect(),
        skill_config_rules: skill_config_rules.clone(),
        plugin_skill_snapshots: plugin_skill_snapshots
            .filter(|_| roots.iter().any(|root| root.plugin_identity.is_some()))
            .cloned(),
    }
}

#[cfg(test)]
#[path = "host_service_tests.rs"]
mod tests;
