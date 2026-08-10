use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::ConfigRequirementsToml;
use codex_config::LoaderOverrides;
use codex_config::NoopThreadConfigLoader;
use codex_config::loader::load_config_layers_state;
use codex_core_skills::SkillMetadata;
use codex_core_skills::loader::MAX_CONCURRENT_ROOT_SCANS;
use codex_core_skills::loader::load_skills_from_roots;
use codex_exec_server::CopyOptions;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::ExecutorFileSystemFuture;
use codex_exec_server::FileMetadata;
use codex_exec_server::FileSystemReadStream;
use codex_exec_server::FileSystemSandboxContext;
use codex_exec_server::LOCAL_FS;
use codex_exec_server::ReadDirectoryEntry;
use codex_exec_server::RemoveOptions;
use codex_exec_server::WalkOptions;
use codex_exec_server::WalkOutcome;
use codex_protocol::protocol::SkillScope;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use codex_utils_plugins::PluginIdentity;
use codex_utils_plugins::PluginSkillRoot;
use codex_utils_plugins::SkillDiscoveryMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio::sync::Semaphore;

use super::repo_agents_skill_roots;
use super::resolve_skill_roots_with_home_dir;
use super::roots_from_layer_stack;

struct BlockingMetadataFileSystem {
    inner: Arc<dyn ExecutorFileSystem>,
    calls: Arc<BlockingMetadataCalls>,
}

struct BlockingMetadataCalls {
    paths: Mutex<Vec<PathUri>>,
    started: Notify,
    release: Semaphore,
}

impl Default for BlockingMetadataCalls {
    fn default() -> Self {
        Self {
            paths: Mutex::new(Vec::new()),
            started: Notify::new(),
            release: Semaphore::new(0),
        }
    }
}

impl ExecutorFileSystem for BlockingMetadataFileSystem {
    fn canonicalize<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        self.inner.canonicalize(path, sandbox)
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        self.inner.read_file(path, sandbox)
    }

    fn read_file_stream<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        self.inner.read_file_stream(path, sandbox)
    }

    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner.write_file(path, contents, sandbox)
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a PathUri,
        options: CreateDirectoryOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner.create_directory(path, options, sandbox)
    }

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        let Ok(path_abs) = path.to_abs_path() else {
            return self.inner.get_metadata(path, sandbox);
        };
        let repo_skill_root_suffix = Path::new(".agents").join("skills");
        if !path_abs.ends_with(repo_skill_root_suffix) {
            return self.inner.get_metadata(path, sandbox);
        }

        self.calls
            .paths
            .lock()
            .expect("metadata paths lock")
            .push(path.clone());
        self.calls.started.notify_one();
        Box::pin(async move {
            self.calls
                .release
                .acquire()
                .await
                .expect("metadata release semaphore")
                .forget();
            self.inner.get_metadata(path, sandbox).await
        })
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        self.inner.read_directory(path, sandbox)
    }

    fn walk<'a>(
        &'a self,
        path: &'a PathUri,
        options: WalkOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        self.inner.walk(path, options, sandbox)
    }

    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        options: RemoveOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner.remove(path, options, sandbox)
    }

    fn copy<'a>(
        &'a self,
        source_path: &'a PathUri,
        destination_path: &'a PathUri,
        options: CopyOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner
            .copy(source_path, destination_path, options, sandbox)
    }
}

fn absolute(path: impl Into<std::path::PathBuf>) -> AbsolutePathBuf {
    AbsolutePathBuf::try_from(path.into()).expect("absolute path")
}

fn empty_config() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

fn stack(layers: Vec<ConfigLayerEntry>) -> ConfigLayerStack {
    ConfigLayerStack::new(
        layers,
        Default::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("valid config stack")
}

fn user_layer(codex_home: &AbsolutePathBuf) -> ConfigLayerEntry {
    ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: codex_home.join("config.toml"),
            profile: None,
        },
        empty_config(),
    )
}

fn project_layer(dot_codex_folder: &AbsolutePathBuf) -> ConfigLayerEntry {
    ConfigLayerEntry::new(
        ConfigLayerSource::Project {
            dot_codex_folder: dot_codex_folder.clone(),
        },
        empty_config(),
    )
}

fn write_skill(root: &AbsolutePathBuf, directory: &str, name: &str) -> AbsolutePathBuf {
    let skill_dir = root.join(directory);
    fs::create_dir_all(&skill_dir).expect("create skill directory");
    let skill_path = skill_dir.join("SKILL.md");
    fs::write(
        &skill_path,
        format!("---\nname: {name}\ndescription: {name} description\n---\n"),
    )
    .expect("write skill");
    AbsolutePathBuf::from_absolute_path(
        dunce::canonicalize(skill_path).expect("canonical skill path"),
    )
    .expect("absolute skill path")
}

fn expected_skill(path: AbsolutePathBuf, name: &str, scope: SkillScope) -> SkillMetadata {
    SkillMetadata {
        name: name.to_string(),
        description: format!("{name} description"),
        short_description: None,
        interface: None,
        dependencies: None,
        policy: None,
        path_to_skills_md: path,
        scope,
        plugin_id: None,
        remote_plugin_id: None,
    }
}

#[test]
fn layer_roots_preserve_scope_precedence_and_disabled_projects() {
    let temp_dir = TempDir::new().expect("temp dir");
    let system_folder = absolute(temp_dir.path().join("etc/codex"));
    let home_folder = absolute(temp_dir.path().join("home"));
    let user_folder = home_folder.join("codex");
    let project_folder = absolute(temp_dir.path().join("repo/.codex"));
    let nested_project_folder = absolute(temp_dir.path().join("repo/nested/.codex"));
    let config_stack = stack(vec![
        ConfigLayerEntry::new(
            ConfigLayerSource::System {
                file: system_folder.join("config.toml"),
            },
            empty_config(),
        ),
        user_layer(&user_folder),
        ConfigLayerEntry::new_disabled(
            ConfigLayerSource::Project {
                dot_codex_folder: project_folder.clone(),
            },
            empty_config(),
            "untrusted project",
        ),
        project_layer(&nested_project_folder),
    ]);

    let roots = roots_from_layer_stack(
        &config_stack,
        Some(&home_folder),
        Some(Arc::clone(&LOCAL_FS)),
    )
    .into_iter()
    .map(|root| (root.scope, root.path))
    .collect::<Vec<_>>();

    assert_eq!(
        roots,
        vec![
            (SkillScope::Repo, nested_project_folder.join("skills")),
            (SkillScope::User, user_folder.join("skills")),
            (SkillScope::User, home_folder.join(".agents/skills")),
            (SkillScope::System, user_folder.join("skills/.system")),
            (SkillScope::Admin, system_folder.join("skills")),
        ]
    );
}

#[tokio::test]
async fn plugin_roots_preserve_plugin_resolution_metadata() {
    let temp_dir = TempDir::new().expect("temp dir");
    let cwd = absolute(temp_dir.path().join("workspace"));
    let plugin_root = absolute(temp_dir.path().join("plugins/example"));
    let skills_root = plugin_root.join("skills");
    let plugin_identity = PluginIdentity {
        plugin_id: "example@test".to_string(),
        remote_plugin_id: Some("plugins~Plugin_example".to_string()),
    };
    let plugin_namespace = "example".to_string();

    let roots = resolve_skill_roots_with_home_dir(
        /*repository_file_system*/ None,
        &stack(Vec::new()),
        &cwd,
        /*home_dir*/ None,
        vec![PluginSkillRoot {
            path: skills_root.clone(),
            plugin_identity: plugin_identity.clone(),
            plugin_namespace: plugin_namespace.clone(),
            plugin_root: plugin_root.clone(),
            discovery_mode: SkillDiscoveryMode::DirectChildren,
        }],
        Vec::new(),
    )
    .await;

    assert_eq!(roots.len(), 1);
    let root = &roots[0];
    assert_eq!(
        (
            root.path.clone(),
            root.scope,
            root.plugin_identity.clone(),
            root.plugin_namespace.clone(),
            root.plugin_root.clone(),
            root.discovery_mode,
        ),
        (
            skills_root,
            SkillScope::User,
            Some(plugin_identity),
            Some(plugin_namespace),
            Some(plugin_root),
            SkillDiscoveryMode::DirectChildren,
        )
    );
    assert!(Arc::ptr_eq(&root.file_system, &LOCAL_FS));
}

#[tokio::test]
async fn unique_extra_root_loads_as_recursive_user_root() {
    let temp_dir = TempDir::new().expect("temp dir");
    let cwd = absolute(temp_dir.path().join("workspace"));
    let extra_root = absolute(temp_dir.path().join("runtime-skills"));
    let skill_path = write_skill(&extra_root, "runtime", "runtime-skill");

    let roots = resolve_skill_roots_with_home_dir(
        /*repository_file_system*/ None,
        &stack(Vec::new()),
        &cwd,
        /*home_dir*/ None,
        Vec::new(),
        vec![extra_root.clone()],
    )
    .await;

    assert_eq!(roots.len(), 1);
    let root = &roots[0];
    assert_eq!(
        (
            root.path.clone(),
            root.scope,
            root.plugin_identity.clone(),
            root.plugin_namespace.clone(),
            root.plugin_root.clone(),
            root.discovery_mode,
        ),
        (
            extra_root,
            SkillScope::User,
            None,
            None,
            None,
            SkillDiscoveryMode::Recursive,
        )
    );
    assert!(Arc::ptr_eq(&root.file_system, &LOCAL_FS));

    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert!(outcome.errors.is_empty());
    assert_eq!(
        outcome.skills,
        vec![expected_skill(
            skill_path,
            "runtime-skill",
            SkillScope::User,
        )]
    );
}

#[tokio::test]
async fn repo_ancestry_does_not_walk_beyond_trusted_project_layer() {
    let temp_dir = TempDir::new().expect("temp dir");
    let outer = absolute(temp_dir.path().join("outer"));
    let cwd = outer.join("nested/inner");
    fs::create_dir_all(outer.join(".agents/skills")).expect("create outer skills");
    fs::create_dir_all(cwd.join(".agents/skills")).expect("create cwd skills");

    let config_stack = stack(vec![project_layer(&cwd.join(".whisply"))]);
    let roots = repo_agents_skill_roots(Some(Arc::clone(&LOCAL_FS)), &config_stack, &cwd)
        .await
        .into_iter()
        .map(|root| root.path)
        .collect::<Vec<_>>();

    assert_eq!(roots, vec![cwd.join(".agents/skills")]);
}

#[tokio::test]
async fn repo_ancestry_stops_at_project_root_and_preserves_root_to_cwd_order() {
    let temp_dir = TempDir::new().expect("temp dir");
    let outer = absolute(temp_dir.path().join("outer"));
    let repository = outer.join("repo");
    let nested = repository.join("nested/inner");
    fs::create_dir_all(&nested).expect("create nested cwd");
    fs::write(repository.join(".git"), "gitdir: fake\n").expect("write git marker");
    fs::create_dir_all(outer.join(".agents/skills")).expect("create outer skills");
    fs::create_dir_all(repository.join(".agents/skills")).expect("create repo skills");
    fs::create_dir_all(repository.join("nested/.agents/skills")).expect("create nested skills");
    let config_stack = stack(vec![project_layer(&repository.join(".whisply"))]);

    let roots = repo_agents_skill_roots(Some(Arc::clone(&LOCAL_FS)), &config_stack, &nested)
        .await
        .into_iter()
        .map(|root| root.path)
        .collect::<Vec<_>>();

    assert_eq!(
        roots,
        vec![
            repository.join(".agents/skills"),
            repository.join("nested/.agents/skills"),
        ]
    );
}

#[tokio::test]
async fn nested_trusted_project_excludes_untrusted_ancestor_agents_roots() {
    let temp_dir = TempDir::new().expect("temp dir");
    let workspace = absolute(temp_dir.path().join("workspace"));
    let nested = workspace.join("nested");
    fs::create_dir_all(&nested).expect("create nested cwd");
    fs::create_dir_all(workspace.join(".agents/skills")).expect("create outer skills");
    fs::create_dir_all(nested.join(".agents/skills")).expect("create nested skills");
    let config_stack = stack(vec![
        ConfigLayerEntry::new_disabled(
            ConfigLayerSource::Project {
                dot_codex_folder: workspace.join(".whisply"),
            },
            empty_config(),
            "untrusted project",
        ),
        project_layer(&nested.join(".whisply")),
    ]);

    let roots = repo_agents_skill_roots(Some(Arc::clone(&LOCAL_FS)), &config_stack, &nested)
        .await
        .into_iter()
        .map(|root| root.path)
        .collect::<Vec<_>>();

    assert_eq!(roots, vec![nested.join(".agents/skills")]);
}

#[tokio::test]
async fn untrusted_project_suppresses_project_skill_roots_but_keeps_user_roots() {
    let temp_dir = TempDir::new().expect("temp dir");
    let home_folder = absolute(temp_dir.path().join("home"));
    let codex_home = home_folder.join("account");
    let workspace = absolute(temp_dir.path().join("workspace"));
    let project_config = workspace.join(".whisply");
    let user_skill = write_skill(&codex_home.join("skills"), "user", "user-skill");
    let home_skill = write_skill(&home_folder.join(".agents/skills"), "home", "home-skill");
    let project_skill = write_skill(&project_config.join("skills"), "project", "project-skill");
    let agent_skill = write_skill(
        &workspace.join(".agents/skills"),
        "compatibility",
        "compatibility-skill",
    );
    let config_stack = stack(vec![
        user_layer(&codex_home),
        ConfigLayerEntry::new_disabled(
            ConfigLayerSource::Project {
                dot_codex_folder: project_config,
            },
            empty_config(),
            "untrusted project",
        ),
    ]);

    let roots = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &config_stack,
        &workspace,
        Some(&home_folder),
        Vec::new(),
        Vec::new(),
    )
    .await;
    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert_eq!(
        outcome.skills,
        vec![
            expected_skill(home_skill, "home-skill", SkillScope::User),
            expected_skill(user_skill, "user-skill", SkillScope::User),
        ]
    );
    assert!(
        outcome
            .skills
            .iter()
            .all(|skill| skill.path_to_skills_md != project_skill
                && skill.path_to_skills_md != agent_skill)
    );
}

#[tokio::test]
async fn trusted_project_keeps_agents_compatibility_as_repo_scope() {
    let temp_dir = TempDir::new().expect("temp dir");
    let workspace = absolute(temp_dir.path().join("workspace"));
    let project_config = workspace.join(".whisply");
    let agent_skill = write_skill(
        &workspace.join(".agents/skills"),
        "compatibility",
        "compatibility-skill",
    );
    let config_stack = stack(vec![project_layer(&project_config)]);

    let roots = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &config_stack,
        &workspace,
        /*home_dir*/ None,
        Vec::new(),
        Vec::new(),
    )
    .await;
    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert_eq!(
        outcome.skills,
        vec![expected_skill(
            agent_skill,
            "compatibility-skill",
            SkillScope::Repo,
        )]
    );
}

#[tokio::test]
async fn trusted_git_project_without_whisply_keeps_agents_compatibility() {
    let temp_dir = TempDir::new().expect("temp dir");
    let codex_home = absolute(temp_dir.path().join("account"));
    let workspace = absolute(temp_dir.path().join("workspace"));
    fs::create_dir_all(&codex_home).expect("create account root");
    fs::create_dir_all(workspace.join(".git")).expect("create git marker");
    fs::write(
        codex_home.join("config.toml"),
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            workspace.display()
        ),
    )
    .expect("write account config");
    let agent_skill = write_skill(
        &workspace.join(".agents/skills"),
        "compatibility",
        "compatibility-skill",
    );
    let overrides = LoaderOverrides {
        managed_config_path: Some(temp_dir.path().join("missing-managed-config.toml")),
        system_config_path: Some(temp_dir.path().join("missing-system-config.toml")),
        system_requirements_path: Some(temp_dir.path().join("missing-system-requirements.toml")),
        ignore_managed_requirements: true,
        #[cfg(target_os = "macos")]
        managed_preferences_base64: Some(String::new()),
        macos_managed_config_requirements_base64: Some(String::new()),
        ..Default::default()
    };
    let config_stack = load_config_layers_state(
        LOCAL_FS.as_ref(),
        codex_home.as_path(),
        Some(workspace.clone()),
        &[],
        overrides,
        &NoopThreadConfigLoader,
    )
    .await
    .expect("load trusted project config");

    assert_eq!(config_stack.trusted_project_root(), Some(&workspace));
    assert!(
        config_stack
            .all_layers_low_to_high()
            .all(|layer| !matches!(layer.name, ConfigLayerSource::Project { .. }))
    );

    let roots = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &config_stack,
        &workspace,
        /*home_dir*/ None,
        Vec::new(),
        Vec::new(),
    )
    .await;
    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert_eq!(
        outcome.skills,
        vec![expected_skill(
            agent_skill,
            "compatibility-skill",
            SkillScope::Repo,
        )]
    );
}

#[tokio::test]
async fn root_trust_keeps_root_agents_when_nested_whisply_inherits_it() {
    let temp_dir = TempDir::new().expect("temp dir");
    let codex_home = absolute(temp_dir.path().join("account"));
    let workspace = absolute(temp_dir.path().join("workspace"));
    let cwd = workspace.join("nested");
    fs::create_dir_all(&codex_home).expect("create account root");
    fs::create_dir_all(workspace.join(".git")).expect("create git marker");
    fs::create_dir_all(cwd.join(".whisply")).expect("create nested project layer");
    fs::write(
        codex_home.join("config.toml"),
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            workspace.display()
        ),
    )
    .expect("write account config");
    let agent_skill = write_skill(
        &workspace.join(".agents/skills"),
        "compatibility",
        "compatibility-skill",
    );
    let overrides = LoaderOverrides {
        managed_config_path: Some(temp_dir.path().join("missing-managed-config.toml")),
        system_config_path: Some(temp_dir.path().join("missing-system-config.toml")),
        system_requirements_path: Some(temp_dir.path().join("missing-system-requirements.toml")),
        ignore_managed_requirements: true,
        #[cfg(target_os = "macos")]
        managed_preferences_base64: Some(String::new()),
        macos_managed_config_requirements_base64: Some(String::new()),
        ..Default::default()
    };
    let config_stack = load_config_layers_state(
        LOCAL_FS.as_ref(),
        codex_home.as_path(),
        Some(cwd.clone()),
        &[],
        overrides,
        &NoopThreadConfigLoader,
    )
    .await
    .expect("load trusted project config");

    assert_eq!(config_stack.trusted_project_root(), Some(&workspace));
    assert!(
        config_stack
            .all_layers_low_to_high()
            .any(|layer| matches!(layer.name, ConfigLayerSource::Project { .. }))
    );

    let roots = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &config_stack,
        &cwd,
        /*home_dir*/ None,
        Vec::new(),
        Vec::new(),
    )
    .await;
    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert_eq!(
        outcome.skills,
        vec![expected_skill(
            agent_skill,
            "compatibility-skill",
            SkillScope::Repo,
        )]
    );
}

#[tokio::test]
async fn project_skill_roots_are_isolated_per_cwd_while_extra_user_root_stays_global() {
    let temp_dir = TempDir::new().expect("temp dir");
    let account_root = absolute(temp_dir.path().join("account/skills"));
    let workspace_a = absolute(temp_dir.path().join("workspace-a"));
    let workspace_b = absolute(temp_dir.path().join("workspace-b"));
    let user_skill = write_skill(&account_root, "user", "account-skill");
    let agent_skill_a = write_skill(
        &workspace_a.join(".agents/skills"),
        "workspace-a",
        "workspace-a-skill",
    );
    let agent_skill_b = write_skill(
        &workspace_b.join(".agents/skills"),
        "workspace-b",
        "workspace-b-skill",
    );
    let stack_a = stack(vec![project_layer(&workspace_a.join(".whisply"))]);
    let stack_b = stack(vec![project_layer(&workspace_b.join(".whisply"))]);

    let roots_a = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &stack_a,
        &workspace_a,
        /*home_dir*/ None,
        Vec::new(),
        vec![account_root.clone()],
    )
    .await;
    let roots_b = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &stack_b,
        &workspace_b,
        /*home_dir*/ None,
        Vec::new(),
        vec![account_root],
    )
    .await;
    let outcome_a = load_skills_from_roots(
        roots_a,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;
    let outcome_b = load_skills_from_roots(
        roots_b,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert_eq!(
        outcome_a.skills,
        vec![
            expected_skill(agent_skill_a, "workspace-a-skill", SkillScope::Repo),
            expected_skill(user_skill.clone(), "account-skill", SkillScope::User),
        ]
    );
    assert_eq!(
        outcome_b.skills,
        vec![
            expected_skill(agent_skill_b, "workspace-b-skill", SkillScope::Repo),
            expected_skill(user_skill, "account-skill", SkillScope::User),
        ]
    );
}

#[tokio::test]
async fn resolved_project_layer_loads_skill_without_git_marker() {
    let temp_dir = TempDir::new().expect("temp dir");
    let workspace = absolute(temp_dir.path().join("workspace"));
    let dot_codex = workspace.join(".codex");
    let skill_root = dot_codex.join("skills");
    fs::create_dir_all(&workspace).expect("create workspace");
    let skill_path = write_skill(&skill_root, "local", "local-skill");
    let config_stack = stack(vec![project_layer(&dot_codex)]);

    let roots = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &config_stack,
        &workspace,
        /*home_dir*/ None,
        Vec::new(),
        Vec::new(),
    )
    .await;
    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert!(outcome.errors.is_empty());
    assert_eq!(
        outcome.skills,
        vec![expected_skill(skill_path, "local-skill", SkillScope::Repo)]
    );
}

#[tokio::test]
async fn resolved_project_layer_loads_skill_when_cwd_is_file() {
    let temp_dir = TempDir::new().expect("temp dir");
    let repository = absolute(temp_dir.path().join("repo"));
    let dot_codex = repository.join(".codex");
    let skill_root = dot_codex.join("skills");
    fs::create_dir_all(&repository).expect("create repository");
    fs::write(repository.join(".git"), "gitdir: fake\n").expect("write git marker");
    let cwd = repository.join("some-file.txt");
    fs::write(&cwd, "contents").expect("write cwd file");
    let skill_path = write_skill(&skill_root, "repo", "repo-skill");
    let config_stack = stack(vec![project_layer(&dot_codex)]);

    let roots = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &config_stack,
        &cwd,
        /*home_dir*/ None,
        Vec::new(),
        Vec::new(),
    )
    .await;
    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;

    assert!(outcome.errors.is_empty());
    assert_eq!(
        outcome.skills,
        vec![expected_skill(skill_path, "repo-skill", SkillScope::Repo)]
    );
}

#[tokio::test]
async fn repo_ancestry_limits_concurrent_probes_and_preserves_order() {
    const CONCURRENCY_LIMIT: usize = 256;

    let temp_dir = TempDir::new().expect("temp dir");
    let repository = absolute(temp_dir.path().join("repo"));
    fs::create_dir_all(&repository).expect("create repository");
    fs::write(repository.join(".git"), "gitdir: fake\n").expect("write git marker");

    let mut directories = vec![repository.clone()];
    let mut cwd = repository;
    for _ in 0..CONCURRENCY_LIMIT {
        cwd = cwd.join("d");
        directories.push(cwd.clone());
    }
    fs::create_dir_all(&cwd).expect("create nested cwd");

    let expected_roots = [0, CONCURRENCY_LIMIT / 2, CONCURRENCY_LIMIT].map(|index| {
        let path = directories[index].join(".agents/skills");
        fs::create_dir_all(&path).expect("create repo skill root");
        path
    });
    let expected_probes = directories
        .iter()
        .map(|directory| PathUri::from_abs_path(&directory.join(".agents/skills")))
        .collect::<Vec<_>>();
    let calls = Arc::new(BlockingMetadataCalls::default());
    let file_system: Arc<dyn ExecutorFileSystem> = Arc::new(BlockingMetadataFileSystem {
        inner: Arc::clone(&LOCAL_FS),
        calls: Arc::clone(&calls),
    });

    let assertions = async {
        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 5), async {
            loop {
                let started = calls.started.notified();
                if calls.paths.lock().expect("metadata paths lock").len() >= CONCURRENCY_LIMIT {
                    break;
                }
                started.await;
            }
        })
        .await
        .expect("initial repo skill root window should start");
        assert_eq!(
            calls.paths.lock().expect("metadata paths lock").as_slice(),
            &expected_probes[..CONCURRENCY_LIMIT]
        );

        calls.release.add_permits(/*n*/ 1);
        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 5), async {
            loop {
                let started = calls.started.notified();
                if calls.paths.lock().expect("metadata paths lock").len() > CONCURRENCY_LIMIT {
                    break;
                }
                started.await;
            }
        })
        .await
        .expect("next repo skill root probe should start");
        assert_eq!(
            calls.paths.lock().expect("metadata paths lock").as_slice(),
            expected_probes.as_slice()
        );

        calls.release.add_permits(expected_probes.len());
    };
    let config_stack = stack(vec![project_layer(&directories[0].join(".whisply"))]);
    let (roots, ()) = tokio::join!(
        repo_agents_skill_roots(Some(file_system), &config_stack, &cwd),
        assertions,
    );

    assert_eq!(
        roots.into_iter().map(|root| root.path).collect::<Vec<_>>(),
        expected_roots
    );
}

#[tokio::test]
async fn resolved_config_and_repo_roots_preserve_order_and_dedupe_paths_not_names() {
    let temp_dir = TempDir::new().expect("temp dir");
    let home_folder = absolute(temp_dir.path().join("home"));
    let codex_home = home_folder.join("codex");
    let system_folder = absolute(temp_dir.path().join("etc/codex"));
    let repository = absolute(temp_dir.path().join("repo"));
    let cwd = repository.join("nested/inner");
    fs::create_dir_all(&cwd).expect("create cwd");
    fs::write(repository.join(".git"), "gitdir: fake\n").expect("write git marker");

    let project_dot_codex = repository.join(".codex");
    let nested_project_dot_codex = repository.join("nested/.codex");
    let user_skills = codex_home.join("skills");
    let root_project_skill = write_skill(
        &project_dot_codex.join("skills"),
        "root-duplicate",
        "duplicate-skill",
    );
    let nested_project_skill = write_skill(
        &nested_project_dot_codex.join("skills"),
        "nested-duplicate",
        "duplicate-skill",
    );
    let user_skill = write_skill(&user_skills, "user-duplicate", "duplicate-skill");
    let home_skill = write_skill(&home_folder.join(".agents/skills"), "home", "home-skill");
    let system_skill = write_skill(&codex_home.join("skills/.system"), "system", "system-skill");
    let admin_skill = write_skill(&system_folder.join("skills"), "admin", "admin-skill");
    let _repo_agent_skill = write_skill(
        &repository.join(".agents/skills"),
        "repo-agent",
        "repo-agent-skill",
    );
    let nested_agent_skill = write_skill(
        &repository.join("nested/.agents/skills"),
        "nested-agent",
        "nested-agent-skill",
    );
    let config_stack = stack(vec![
        ConfigLayerEntry::new(
            ConfigLayerSource::System {
                file: system_folder.join("config.toml"),
            },
            empty_config(),
        ),
        user_layer(&codex_home),
        project_layer(&project_dot_codex),
        project_layer(&nested_project_dot_codex),
    ]);

    let roots = resolve_skill_roots_with_home_dir(
        Some(Arc::clone(&LOCAL_FS)),
        &config_stack,
        &cwd,
        Some(&home_folder),
        Vec::new(),
        vec![user_skills],
    )
    .await;
    assert_eq!(roots.len(), 7);
    let outcome = load_skills_from_roots(
        roots,
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(MAX_CONCURRENT_ROOT_SCANS)),
    )
    .await;
    assert!(outcome.errors.is_empty());
    assert_eq!(
        outcome.skills,
        vec![
            expected_skill(root_project_skill, "duplicate-skill", SkillScope::Repo),
            expected_skill(nested_project_skill, "duplicate-skill", SkillScope::Repo),
            expected_skill(nested_agent_skill, "nested-agent-skill", SkillScope::Repo),
            expected_skill(user_skill, "duplicate-skill", SkillScope::User),
            expected_skill(home_skill, "home-skill", SkillScope::User),
            expected_skill(system_skill, "system-skill", SkillScope::System),
            expected_skill(admin_skill, "admin-skill", SkillScope::Admin),
        ]
    );
}
