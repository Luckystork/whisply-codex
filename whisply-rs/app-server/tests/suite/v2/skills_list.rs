use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigEdit;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::ExperimentalFeatureEnablementSetParams;
use codex_app_server_protocol::ExperimentalFeatureEnablementSetResponse;
use codex_app_server_protocol::MergeStrategy;
use codex_app_server_protocol::PluginInstallParams;
use codex_app_server_protocol::PluginInstallResponse;
use codex_app_server_protocol::SkillScope;
use codex_app_server_protocol::SkillsChangedNotification;
use codex_app_server_protocol::SkillsExtraRootsSetParams;
use codex_app_server_protocol::SkillsExtraRootsSetResponse;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use whisply_core::config::set_project_trust_level;
use whisply_exec_server::CODEX_EXEC_SERVER_URL_ENV_VAR;
use whisply_protocol::config_types::TrustLevel;
use whisply_utils_absolute_path::AbsolutePathBuf;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const WATCHER_TIMEOUT: Duration = Duration::from_secs(20);

fn write_skill(root: &TempDir, name: &str) -> Result<()> {
    let skill_dir = root.path().join("skills").join(name);
    std::fs::create_dir_all(&skill_dir)?;
    let content = format!("---\nname: {name}\ndescription: {name} description\n---\n\n# Body\n");
    std::fs::write(skill_dir.join("SKILL.md"), content)?;
    Ok(())
}

fn isolated_home_env(codex_home: &TempDir) -> [(&'static str, Option<&str>); 1] {
    [("HOME", codex_home.path().to_str())]
}

async fn expect_skills_changed_notification(
    mcp: &mut TestAppServer,
    timeout_duration: Duration,
) -> Result<()> {
    let notification: SkillsChangedNotification =
        timeout(timeout_duration, mcp.read_notification("skills/changed")).await??;
    assert_eq!(notification, SkillsChangedNotification {});
    Ok(())
}

fn write_plugin_with_skill(
    repo_root: &std::path::Path,
    plugin_name: &str,
    skill_name: &str,
) -> Result<()> {
    std::fs::create_dir_all(repo_root.join(".git"))?;
    std::fs::create_dir_all(repo_root.join(".agents/plugins"))?;
    std::fs::write(
        repo_root.join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "local-marketplace",
  "plugins": [
    {{
      "name": "{plugin_name}",
      "source": {{
        "source": "local",
        "path": "./{plugin_name}"
      }}
    }}
  ]
}}"#
        ),
    )?;

    let plugin_root = repo_root.join(plugin_name);
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;

    let skill_dir = plugin_root.join("skills").join(skill_name);
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {skill_name}\ndescription: {skill_name} description\n---\n\n# Body\n"),
    )?;
    Ok(())
}

fn write_cached_local_curated_plugin_with_skill(codex_home: &std::path::Path) -> Result<()> {
    let plugin_root = codex_home.join("plugins/cache/openai-curated/google-calendar/local");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"google-calendar"}"#,
    )?;

    let skill_dir = plugin_root.join("skills/meeting-prep");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: meeting-prep\ndescription: Prepare for meetings\n---\n\n# Body\n",
    )?;
    Ok(())
}

#[tokio::test]
async fn skills_list_disabled_bundled_skills_preserves_shared_system_skill_cache() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    let mut enabled_mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let enabled_skills_request_id = enabled_mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        enabled_mcp.read_response(enabled_skills_request_id),
    )
    .await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    let system_skill_paths = data[0]
        .skills
        .iter()
        .filter(|skill| skill.scope == SkillScope::System)
        .map(|skill| skill.path.clone())
        .collect::<Vec<_>>();
    assert!(
        !system_skill_paths.is_empty(),
        "expected enabled app-server to materialize bundled system skills"
    );

    let mut disabled_mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .with_args(&["-c", "skills.bundled.enabled=false"])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let disabled_skills_request_id = disabled_mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        disabled_mcp.read_response(disabled_skills_request_id),
    )
    .await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.scope != SkillScope::System)
    );
    assert!(
        system_skill_paths
            .iter()
            .all(|path| path.as_path().is_file()),
        "disabled app-server must not remove the cache shared by other processes"
    );

    let reloaded_skills_request_id = enabled_mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        enabled_mcp.read_response(reloaded_skills_request_id),
    )
    .await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    let reloaded_system_skill_paths = data[0]
        .skills
        .iter()
        .filter(|skill| skill.scope == SkillScope::System)
        .map(|skill| skill.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(reloaded_system_skill_paths, system_skill_paths);
    Ok(())
}

#[tokio::test]
async fn skills_list_runtime_enable_refreshes_shared_system_skill_cache() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    let stale_skill_path = codex_home
        .path()
        .join("skills/.system/stale-system-skill/SKILL.md");
    std::fs::create_dir_all(
        stale_skill_path
            .parent()
            .expect("stale system skill should have a parent"),
    )?;
    std::fs::write(
        &stale_skill_path,
        "---\nname: stale-system-skill\ndescription: stale system skill\n---\n\n# Body\n",
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[skills.bundled]\nenabled = false\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let disabled_skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_response(disabled_skills_request_id),
    )
    .await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.scope != SkillScope::System)
    );
    assert!(stale_skill_path.is_file());

    let enable_request_id = mcp
        .send_config_batch_write_request(ConfigBatchWriteParams {
            edits: vec![ConfigEdit {
                key_path: "skills.bundled.enabled".to_string(),
                value: serde_json::json!(true),
                merge_strategy: MergeStrategy::Replace,
            }],
            file_path: None,
            expected_version: None,
            reload_user_config: true,
        })
        .await?;
    let _: ConfigWriteResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(enable_request_id)).await??;

    let enabled_skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_response(enabled_skills_request_id),
    )
    .await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.scope == SkillScope::System)
    );
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "stale-system-skill")
    );
    assert!(!stale_skill_path.exists());
    Ok(())
}

#[tokio::test]
async fn remote_plugin_toggle_does_not_hide_local_curated_plugin_skills() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    write_cached_local_curated_plugin_with_skill(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."google-calendar@openai-curated"]
enabled = true
"#,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let disablement_request_id = mcp
        .send_experimental_feature_enablement_set_request(ExperimentalFeatureEnablementSetParams {
            enablement: BTreeMap::from([("remote_plugin".to_string(), false)]),
        })
        .await?;
    let _: ExperimentalFeatureEnablementSetResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(disablement_request_id)).await??;

    let initial_skills_list_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let thread_start_request_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            cwd: Some(cwd.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_response(initial_skills_list_request_id),
    )
    .await??;
    assert!(data.iter().any(|entry| {
        entry
            .skills
            .iter()
            .any(|skill| skill.name == "google-calendar:meeting-prep")
    }));
    let _: ThreadStartResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(thread_start_request_id)).await??;

    std::fs::write(
        codex_home.path().join(
            "plugins/cache/openai-curated/google-calendar/local/skills/meeting-prep/SKILL.md",
        ),
        "---\nname: meeting-prep\ndescription: Updated meeting preparation\n---\n\n# Body\n",
    )?;
    for force_reload in [true, false] {
        let request_id = mcp
            .send_skills_list_request(SkillsListParams {
                cwds: vec![cwd.path().to_path_buf()],
                force_reload,
            })
            .await?;
        let SkillsListResponse { data } =
            timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
        assert!(data.iter().any(|entry| {
            entry.skills.iter().any(|skill| {
                skill.name == "google-calendar:meeting-prep"
                    && skill.description == "Updated meeting preparation"
            })
        }));
    }

    let enablement_request_id = mcp
        .send_experimental_feature_enablement_set_request(ExperimentalFeatureEnablementSetParams {
            enablement: BTreeMap::from([("remote_plugin".to_string(), true)]),
        })
        .await?;
    let _: ExperimentalFeatureEnablementSetResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(enablement_request_id)).await??;

    let skills_list_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(skills_list_request_id)).await??;

    assert!(data.iter().any(|entry| {
        entry
            .skills
            .iter()
            .any(|skill| skill.name == "google-calendar:meeting-prep")
    }));
    Ok(())
}

#[tokio::test]
async fn skills_list_uses_local_plugin_policy_without_workspace_settings_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    write_skill(&codex_home, "home-skill")?;
    write_plugin_with_skill(repo_root.path(), "demo-plugin", "plugin-skill")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"[features]
plugins = true

[marketplaces.local-marketplace]
source_type = "local"
source = {:?}

[plugins."demo-plugin@local-marketplace"]
enabled = true
"#,
            repo_root.path()
        ),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .without_managed_config()
        .with_env_overrides(&isolated_home_env(&codex_home))
        .with_env_overrides(&[
            ("HTTP_PROXY", Some(proxy_uri.as_str())),
            ("http_proxy", Some(proxy_uri.as_str())),
            ("HTTPS_PROXY", Some(proxy_uri.as_str())),
            ("https_proxy", Some(proxy_uri.as_str())),
            ("ALL_PROXY", None),
            ("all_proxy", None),
            ("NO_PROXY", None),
            ("no_proxy", None),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let install_request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            remote_marketplace_name: None,
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;
    let _: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(install_request_id)).await??;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![repo_root.path().to_path_buf()],
            force_reload: true,
        })
        .await?;

    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "home-skill"),
        "non-plugin skills should remain available"
    );
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "demo-plugin:plugin-skill"),
        "local plugin skills should remain available under BrokerOnly"
    );
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "skills/list must not fetch persisted-auth workspace settings"
    );
    Ok(())
}

#[tokio::test]
async fn skills_list_skips_cwd_roots_when_environment_disabled() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    write_skill(&codex_home, "home-skill")?;
    let repo_skill_dir = cwd.path().join(".agents/skills/repo-skill");
    std::fs::create_dir_all(&repo_skill_dir)?;
    std::fs::write(
        repo_skill_dir.join("SKILL.md"),
        "---\nname: repo-skill\ndescription: from repo root\n---\n\n# Body\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .with_env_overrides(&[(CODEX_EXEC_SERVER_URL_ENV_VAR, Some("none"))])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;

    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].cwd, cwd.path().to_path_buf());
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "home-skill")
    );
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "repo-skill")
    );
    Ok(())
}

#[tokio::test]
async fn skills_list_accepts_relative_cwds() -> Result<()> {
    let codex_home = TempDir::new()?;
    let relative_cwd = std::path::PathBuf::from("relative-cwd");
    std::fs::create_dir_all(codex_home.path().join(&relative_cwd))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![relative_cwd.clone()],
            force_reload: true,
        })
        .await?;

    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].cwd, relative_cwd);
    assert_eq!(data[0].errors, Vec::new());
    Ok(())
}

#[tokio::test]
async fn skills_list_preserves_requested_cwd_order() -> Result<()> {
    let codex_home = TempDir::new()?;
    let first_cwd = TempDir::new()?;
    let second_cwd = TempDir::new()?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![
                first_cwd.path().to_path_buf(),
                second_cwd.path().to_path_buf(),
            ],
            force_reload: true,
        })
        .await?;

    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(
        data.iter()
            .map(|entry| entry.cwd.clone())
            .collect::<Vec<_>>(),
        vec![
            first_cwd.path().to_path_buf(),
            second_cwd.path().to_path_buf(),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn skills_list_uses_cached_result_after_session_default_writes_until_force_reload()
-> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    set_project_trust_level(codex_home.path(), cwd.path(), TrustLevel::Trusted)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    // Seed the cwd cache before the cwd-local skill exists.
    let first_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let SkillsListResponse { data: first_data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(first_request_id)).await??;
    assert_eq!(first_data.len(), 1);
    assert!(
        first_data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "late-extra-skill")
    );

    let skill_dir = cwd.path().join(".agents/skills/late-extra-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: late-extra-skill\ndescription: late skill\n---\n\n# Body\n",
    )?;

    for edits in [
        vec![ConfigEdit {
            key_path: "plan_mode_reasoning_effort".to_string(),
            value: serde_json::json!("high"),
            merge_strategy: MergeStrategy::Replace,
        }],
        vec![ConfigEdit {
            key_path: "service_tier".to_string(),
            value: serde_json::json!("fast"),
            merge_strategy: MergeStrategy::Replace,
        }],
        vec![ConfigEdit {
            key_path: "personality".to_string(),
            value: serde_json::json!("friendly"),
            merge_strategy: MergeStrategy::Replace,
        }],
        vec![
            ConfigEdit {
                key_path: "model".to_string(),
                value: serde_json::json!("gpt-5.4"),
                merge_strategy: MergeStrategy::Replace,
            },
            ConfigEdit {
                key_path: "model_reasoning_effort".to_string(),
                value: serde_json::json!("high"),
                merge_strategy: MergeStrategy::Replace,
            },
        ],
    ] {
        let write_id = mcp
            .send_config_batch_write_request(ConfigBatchWriteParams {
                edits,
                file_path: None,
                expected_version: None,
                reload_user_config: true,
            })
            .await?;
        let _: ConfigWriteResponse =
            timeout(DEFAULT_TIMEOUT, mcp.read_response(write_id)).await??;
    }

    let second_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let SkillsListResponse { data: second_data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(second_request_id)).await??;
    assert_eq!(second_data.len(), 1);
    assert!(
        second_data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "late-extra-skill")
    );

    let third_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data: third_data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(third_request_id)).await??;
    assert_eq!(third_data.len(), 1);
    assert!(
        third_data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "late-extra-skill")
    );
    Ok(())
}

#[tokio::test]
async fn skills_extra_roots_set_preserves_account_runtime_roots() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    let extra_skills_root = codex_home.path().join("skills");
    let skill_dir = extra_skills_root.join("runtime-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: runtime-skill\ndescription: runtime skill\n---\n\n# Body\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let set_request_id = mcp
        .send_skills_extra_roots_set_request(SkillsExtraRootsSetParams {
            extra_roots: vec![AbsolutePathBuf::from_absolute_path(&extra_skills_root)?],
        })
        .await?;
    let _: SkillsExtraRootsSetResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(set_request_id)).await??;
    expect_skills_changed_notification(&mut mcp, DEFAULT_TIMEOUT).await?;

    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(skills_request_id)).await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "runtime-skill")
    );

    let reset_request_id = mcp
        .send_skills_extra_roots_set_request(SkillsExtraRootsSetParams {
            extra_roots: Vec::new(),
        })
        .await?;
    let _: SkillsExtraRootsSetResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(reset_request_id)).await??;
    expect_skills_changed_notification(&mut mcp, DEFAULT_TIMEOUT).await?;

    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(skills_request_id)).await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "runtime-skill")
    );

    let clear_request_id = mcp
        .send_skills_extra_roots_set_request(SkillsExtraRootsSetParams {
            extra_roots: Vec::new(),
        })
        .await?;
    let _: SkillsExtraRootsSetResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(clear_request_id)).await??;
    expect_skills_changed_notification(&mut mcp, DEFAULT_TIMEOUT).await?;
    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(skills_request_id)).await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "runtime-skill")
    );

    drop(mcp);
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(skills_request_id)).await??;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "runtime-skill")
    );
    Ok(())
}

#[tokio::test]
async fn skills_extra_roots_set_ignores_non_account_roots_for_every_cwd() -> Result<()> {
    let codex_home = TempDir::new()?;
    let first_cwd = TempDir::new()?;
    let second_cwd = TempDir::new()?;
    let external_root = TempDir::new()?;
    let external_skills_root = external_root.path().join("skills");
    let skill_dir = external_skills_root.join("injected-runtime-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: injected-runtime-skill\ndescription: external skill\n---\n\n# Body\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let set_request_id = mcp
        .send_skills_extra_roots_set_request(SkillsExtraRootsSetParams {
            extra_roots: vec![AbsolutePathBuf::from_absolute_path(&external_skills_root)?],
        })
        .await?;
    let _: SkillsExtraRootsSetResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(set_request_id)).await??;
    expect_skills_changed_notification(&mut mcp, DEFAULT_TIMEOUT).await?;

    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![
                first_cwd.path().to_path_buf(),
                second_cwd.path().to_path_buf(),
            ],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(skills_request_id)).await??;
    assert_eq!(data.len(), 2);
    assert!(data.iter().all(|entry| {
        entry
            .skills
            .iter()
            .all(|skill| skill.name != "injected-runtime-skill")
    }));
    Ok(())
}

#[tokio::test]
async fn skills_changed_notification_is_emitted_after_skill_change() -> Result<()> {
    // TODO(anp): Remove after skill watching can bridge host-local storage into remote exec.
    skip_if_remote!(
        Ok(()),
        "host-local skill changes are not visible to remote executors"
    );

    let codex_home = TempDir::new()?;
    write_skill(&codex_home, "demo")?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let initial_skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![codex_home.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_response(initial_skills_request_id),
    )
    .await??;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| { skill.name == "demo" && skill.description == "demo description" })
    );

    let thread_start_request_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: None,
            model_provider: None,
            allow_provider_model_fallback: false,
            service_tier: None,
            cwd: None,
            runtime_workspace_roots: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox: None,
            permissions: None,
            config: None,
            service_name: None,
            base_instructions: None,
            developer_instructions: None,
            personality: None,
            multi_agent_mode: None,
            ephemeral: None,
            history_mode: None,
            session_start_source: None,
            thread_source: None,
            dynamic_tools: None,
            environments: None,
            selected_capability_roots: None,
            mock_experimental_field: None,
            experimental_raw_events: false,
        })
        .await?;
    let _: ThreadStartResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(thread_start_request_id)).await??;

    let skill_path = codex_home
        .path()
        .join("skills")
        .join("demo")
        .join("SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: demo\ndescription: updated\n---\n\n# Updated\n",
    )?;

    expect_skills_changed_notification(&mut mcp, WATCHER_TIMEOUT).await?;
    let updated_skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![codex_home.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let SkillsListResponse { data } = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_response(updated_skills_request_id),
    )
    .await??;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "demo" && skill.description == "updated")
    );
    Ok(())
}

/// A package has always been allowed to declare a version and the loader threw
/// it away, so `skills/list` reported none for every skill and no surface could
/// show one. This is the hop the Mac app depends on.
#[tokio::test]
async fn skills_list_reports_the_version_a_package_declares() -> Result<()> {
    let codex_home = TempDir::new()?;
    for (name, frontmatter) in [
        ("versioned-skill", "version: 2.1.0\n"),
        ("unversioned-skill", ""),
    ] {
        let skill_dir = codex_home.path().join("skills").join(name);
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {name} description\n{frontmatter}---\n\n# Body\n"
            ),
        )?;
    }

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&isolated_home_env(&codex_home))
        .without_auto_env()
        .with_env_overrides(&[(CODEX_EXEC_SERVER_URL_ENV_VAR, Some("none"))])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![codex_home.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let skills = &data[0].skills;
    let versioned = skills
        .iter()
        .find(|skill| skill.name == "versioned-skill")
        .expect("the versioned skill should be listed");
    assert_eq!(versioned.version.as_deref(), Some("2.1.0"));

    // A package that declared nothing must stay distinguishable, or a client
    // cannot tell "no version" from "some version we failed to read".
    let unversioned = skills
        .iter()
        .find(|skill| skill.name == "unversioned-skill")
        .expect("the unversioned skill should be listed");
    assert_eq!(unversioned.version, None);
    Ok(())
}
