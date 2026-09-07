use super::new_status_output;
use super::new_status_output_with_rate_limits;
use super::new_status_output_with_rate_limits_handle;
use super::rate_limit_snapshot_display;
use super::rate_limits::RateLimitSnapshotDisplay;
use super::rate_limits::RateLimitWindowDisplay;
use super::rate_limits::SpendControlLimitSnapshotDisplay;
use super::rate_limits::StatusRateLimitData;
use super::rate_limits::compose_rate_limit_data_many;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::PermissionProfileSnapshot;
use crate::pager_overlay::TranscriptOverlay;
use crate::status::StatusAccountDisplay;
use crate::status::remote_connection::RemoteConnectionStatus;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use crate::token_usage::TokenUsage;
use crate::token_usage::TokenUsageInfo;
use chrono::Duration as ChronoDuration;
use chrono::Local;
use chrono::TimeZone;
use chrono::Utc;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::CreditsSnapshot;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RateLimitWindow;
use codex_app_server_protocol::SpendControlLimitSnapshot;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::prelude::*;
use std::sync::Arc;
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;
use whisply_config::LoaderOverrides;
use whisply_models_manager::test_support::construct_model_info_offline_for_tests;
use whisply_models_manager::test_support::get_model_offline_for_tests;
use whisply_protocol::ThreadId;
use whisply_protocol::config_types::ApprovalsReviewer;
use whisply_protocol::config_types::ReasoningSummary;
use whisply_protocol::models::ActivePermissionProfile;
use whisply_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use whisply_protocol::models::ManagedFileSystemPermissions;
use whisply_protocol::models::PermissionProfile;
use whisply_protocol::openai_models::ReasoningEffort;
use whisply_protocol::permissions::FileSystemAccessMode;
use whisply_protocol::permissions::FileSystemPath;
use whisply_protocol::permissions::FileSystemSandboxEntry;
use whisply_protocol::permissions::FileSystemSpecialPath;
use whisply_protocol::permissions::NetworkSandboxPolicy;
use whisply_utils_absolute_path::AbsolutePathBuf;

#[test]
fn stale_monthly_limit_marks_fresh_rolling_snapshot_stale() {
    let now = Local::now();
    let snapshot = RateLimitSnapshotDisplay {
        limit_name: "codex".to_string(),
        captured_at: now,
        primary: Some(RateLimitWindowDisplay {
            used_percent: 20.0,
            resets_at: Some("soon".to_string()),
            window_minutes: Some(300),
        }),
        secondary: None,
        credits: None,
        individual_limit: Some(SpendControlLimitSnapshotDisplay {
            captured_at: now - ChronoDuration::minutes(20),
            percent_remaining: 68.0,
            used: "8,000".to_string(),
            limit: "25,000".to_string(),
            resets_at: Some("later".to_string()),
        }),
    };

    assert!(matches!(
        compose_rate_limit_data_many(&[snapshot], now),
        StatusRateLimitData::Stale(_)
    ));
}

fn app_server_workspace_write_profile(network_enabled: bool) -> PermissionProfile {
    PermissionProfile::Managed {
        network: if network_enabled {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        },
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Read,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::ProjectRoots { subpath: None },
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::SlashTmp,
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Tmpdir,
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
            ],
            glob_scan_max_depth: None,
        },
    }
}

async fn test_config(temp_home: &TempDir) -> Config {
    let mut config = ConfigBuilder::default()
        .codex_home(temp_home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .expect("load config");
    config.approvals_reviewer = ApprovalsReviewer::User;
    config
        .permissions
        .set_permission_profile(app_server_workspace_write_profile(
            /*network_enabled*/ true,
        ))
        .expect("set permission profile");
    config
}

fn set_workspace_cwd(config: &mut Config, cwd: AbsolutePathBuf) {
    config.cwd = cwd.clone();
    config.workspace_roots = vec![cwd];
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
}

fn test_status_account_display() -> Option<StatusAccountDisplay> {
    None
}

fn token_info_for(model_slug: &str, config: &Config, usage: &TokenUsage) -> TokenUsageInfo {
    let context_window =
        construct_model_info_offline_for_tests(model_slug, &config.to_models_manager_config())
            .context_window;
    TokenUsageInfo {
        total_token_usage: usage.clone(),
        last_token_usage: usage.clone(),
        model_context_window: context_window,
    }
}

fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn sanitize_directory(lines: Vec<String>) -> Vec<String> {
    let frame_width = lines
        .iter()
        .find(|line| line.starts_with('╭'))
        .map(|line| UnicodeWidthStr::width(line.as_str()));
    lines
        .into_iter()
        .map(|line| {
            if let (Some(frame_width), Some(dir_pos), Some(pipe_idx)) =
                (frame_width, line.find("Directory: "), line.rfind('│'))
            {
                let prefix = &line[..dir_pos + "Directory: ".len()];
                let suffix = &line[pipe_idx..];
                let replacement = "[[workspace]]";
                let content_width = frame_width.saturating_sub(
                    UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(suffix),
                );
                let mut rebuilt = prefix.to_string();
                rebuilt.push_str(replacement);
                let replacement_width = UnicodeWidthStr::width(replacement);
                if content_width > replacement_width {
                    rebuilt.push_str(&" ".repeat(content_width - replacement_width));
                }
                rebuilt.push_str(suffix);
                rebuilt
            } else {
                line
            }
        })
        .collect()
}

fn buffer_to_text(buffer: &Buffer, width: u16) -> String {
    let lines = buffer
        .content
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(|cell| {
                    let symbol = cell.symbol();
                    symbol
                        .strip_prefix("\x1b]8;;")
                        .and_then(|symbol| symbol.split_once('\x07'))
                        .and_then(|(_, symbol)| symbol.strip_suffix("\x1b]8;;\x07"))
                        .unwrap_or(symbol)
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>();
    sanitize_directory(lines).join("\n")
}

fn reset_at_from(captured_at: &chrono::DateTime<chrono::Local>, seconds: i64) -> i64 {
    (*captured_at + ChronoDuration::seconds(seconds))
        .with_timezone(&Utc)
        .timestamp()
}

fn permissions_text_for(config: &Config) -> Option<String> {
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let composite = new_status_output(
        config,
        test_status_account_display().as_ref(),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    render_lines(&composite.display_lines(/*width*/ 80))
        .iter()
        .find(|line| line.contains("Permissions:"))
        .and_then(|line| {
            line.split("Permissions:")
                .nth(1)
                .map(str::trim)
                .map(|text| text.trim_end_matches('│'))
                .map(str::trim)
                .map(ToString::to_string)
        })
}

#[tokio::test]
async fn status_snapshot_includes_reasoning_details() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .set_permission_profile(PermissionProfile::workspace_write())
        .expect("set permission profile");

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 150,
        total_tokens: 2_250,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 72,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 600)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 45,
            window_duration_mins: Some(10080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 1_200)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);

    let reasoning_effort_override = Some(Some(ReasoningEffort::High));
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        reasoning_effort_override,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_chatgpt_plan_without_email() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = StatusAccountDisplay::ChatGpt {
        email: None,
        plan: Some("Enterprise (Automation)".to_string()),
    };
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());

    let composite = new_status_output(
        &config,
        Some(&account_display),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let sanitized =
        sanitize_directory(render_lines(&composite.display_lines(/*width*/ 80))).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_permissions_non_default_workspace_write_uses_workspace_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .set_permission_profile(app_server_workspace_write_profile(
            /*network_enabled*/ true,
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Custom (workspace with network access, Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_named_read_only_profile_shows_builtin_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::read_only(),
            ActivePermissionProfile::read_only(),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Read Only (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_read_only_profile_shows_additional_writable_roots() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let extra_root = test_path_buf("/workspace/extra").abs();
    let file_system_policy = PermissionProfile::read_only()
        .file_system_sandbox_policy()
        .with_additional_writable_roots(config.cwd.as_path(), std::slice::from_ref(&extra_root));
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::from_runtime_permissions(
                &file_system_policy,
                NetworkSandboxPolicy::Restricted,
            ),
            ActivePermissionProfile::read_only(),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Read Only (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_named_workspace_profile_shows_builtin_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_workspace_auto_review_shows_reviewer_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace (Auto)")
    );
}

#[tokio::test]
async fn status_permissions_named_profile_shows_additional_writable_roots() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let extra_root = test_path_buf("/workspace/extra").abs();
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write_with(
                std::slice::from_ref(&extra_root),
                NetworkSandboxPolicy::Restricted,
                /*exclude_tmpdir_env_var*/ false,
                /*exclude_slash_tmp*/ false,
            ),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_workspace_roots_show_additional_directories() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let extra_root = test_path_buf("/workspace/extra").abs();
    config.workspace_roots = vec![config.cwd.clone(), extra_root.clone()];
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(":workspace"),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config),
        Some(format!(
            "Workspace [{}] (Ask for approval)",
            extra_root.display()
        ))
    );
}

#[tokio::test]
async fn status_permissions_workspace_roots_include_profile_defined_directories() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let profile_root = test_path_buf("/workspace/shared").abs();
    config
        .permissions
        .set_permission_profile_from_session_snapshot(
            PermissionProfileSnapshot::active_with_profile_workspace_roots(
                PermissionProfile::workspace_write_with(
                    std::slice::from_ref(&profile_root),
                    NetworkSandboxPolicy::Restricted,
                    /*exclude_tmpdir_env_var*/ false,
                    /*exclude_slash_tmp*/ false,
                ),
                ActivePermissionProfile::new(":workspace"),
                vec![profile_root.clone()],
            ),
        )
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config),
        Some(format!(
            "Workspace [{}] (Ask for approval)",
            profile_root.display()
        ))
    );
}

#[tokio::test]
async fn status_permissions_broadened_workspace_profile_shows_builtin_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write_with(
                &[],
                NetworkSandboxPolicy::Enabled,
                /*exclude_tmpdir_env_var*/ false,
                /*exclude_slash_tmp*/ false,
            ),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace with network access (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_user_defined_profile_shows_name() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::read_only(),
            ActivePermissionProfile::new("locked"),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Profile locked (read-only, Ask for approval)")
    );
}

#[tokio::test]
async fn status_snapshot_shows_active_user_defined_profile() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::read_only(),
            ActivePermissionProfile::new("locked"),
        ))
        .expect("set permission profile");

    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);

    let composite = new_status_output(
        &config,
        test_status_account_display().as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_model_provider_is_managed_and_omits_direct_usage_authority() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());

    let (composite, _handle) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        /*remote_connection*/ None,
        test_status_account_display().as_ref(),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ &[],
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ false,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120)).join("\n");

    assert!(
        rendered
            .lines()
            .any(|line| line.contains("Model provider:") && line.contains("Whisply")),
        "expected /status to identify the managed provider, got: {rendered}"
    );
    assert!(
        !rendered.contains("https://chatgpt.com/codex/settings/usage"),
        "managed /status must not advertise legacy direct usage authority, got: {rendered}"
    );

    let mut wide_destinations = composite
        .display_hyperlink_lines(/*width*/ 120)
        .into_iter()
        .flat_map(|line| line.hyperlinks.into_iter())
        .map(|link| link.destination);
    assert_eq!(wide_destinations.next(), None);
}

#[tokio::test]
async fn status_snapshot_shows_auto_review_permissions() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);

    let composite = new_status_output(
        &config,
        test_status_account_display().as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_permissions_full_disk_managed_with_network_is_danger_full_access() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile(PermissionProfile::Managed {
            network: NetworkSandboxPolicy::Enabled,
            file_system: ManagedFileSystemPermissions::Unrestricted,
        })
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Custom (danger-full-access, Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_full_disk_managed_without_network_is_external_sandbox() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile(PermissionProfile::Managed {
            network: NetworkSandboxPolicy::Restricted,
            file_system: ManagedFileSystemPermissions::Unrestricted,
        })
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Custom (external-sandbox, Ask for approval)")
    );
}

#[tokio::test]
async fn status_snapshot_includes_forked_from() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 8, 9, 10, 11, 12)
        .single()
        .expect("valid time");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let session_id =
        ThreadId::from_string("0f0f3c13-6cf9-4aa4-8b80-7d49c2f1be2e").expect("session id");
    let forked_from =
        ThreadId::from_string("e9f18a88-8081-4e51-9d4e-8af5cde2d8dd").expect("forked id");

    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &Some(session_id),
        /*thread_name*/ None,
        Some(forked_from),
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_includes_monthly_limit() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 12,
            window_duration_mins: Some(43_200),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 86_400)),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_includes_enterprise_monthly_credit_limit() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        individual_limit: Some(SpendControlLimitSnapshot {
            limit: "25000".to_string(),
            used: "8000".to_string(),
            remaining_percent: 68,
            resets_at: reset_at_from(&captured_at, /*seconds*/ 86_400),
        }),
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 92));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);

    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 46));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(
        "status_snapshot_wraps_enterprise_monthly_credit_details_in_narrow_terminal",
        sanitized
    );
}

#[tokio::test]
async fn status_snapshot_uses_generic_limit_labels_for_unsupported_windows() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 35,
            window_duration_mins: Some(2 * 60),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 86_400)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 50,
            window_duration_mins: Some(3 * 60),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 172_800)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_unlimited_credits() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: true,
            balance: None,
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Credits:") && line.contains("Unlimited")),
        "expected Credits: Unlimited line, got {rendered:?}"
    );
}

#[tokio::test]
async fn status_snapshot_shows_positive_credits() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 3, 4, 5, 6, 7)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("12.5".to_string()),
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Credits:") && line.contains("13 credits")),
        "expected Credits line with rounded credits, got {rendered:?}"
    );
}

#[tokio::test]
async fn status_snapshot_shows_available_credits_without_display_balance() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 4, 5, 6, 7, 8)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    for balance in [
        None,
        Some(String::new()),
        Some("0".to_string()),
        Some("not-a-number".to_string()),
        Some("inf".to_string()),
    ] {
        let snapshot = RateLimitSnapshot {
            limit_id: None,
            limit_name: None,
            primary: None,
            secondary: None,
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance,
            }),
            individual_limit: None,
            spend_control_reached: None,
            plan_type: None,
            rate_limit_reached_type: None,
        };
        let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
        let composite = new_status_output(
            &config,
            account_display.as_ref(),
            Some(&token_info),
            &usage,
            &None,
            /*thread_name*/ None,
            /*forked_from*/ None,
            Some(&rate_display),
            None,
            captured_at,
            &model_slug,
            /*collaboration_mode*/ None,
            /*reasoning_effort_override*/ None,
        );
        let rendered = render_lines(&composite.display_lines(/*width*/ 120));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Credits:") && line.contains("Available")),
            "expected Credits: Available line, got {rendered:?}"
        );
    }
}

#[tokio::test]
async fn status_snapshot_respects_unlimited_without_has_credits_flag() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: false,
            unlimited: true,
            balance: None,
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Credits:") && line.contains("Unlimited")),
        "expected Credits: Unlimited line, got {rendered:?}"
    );
}

#[tokio::test]
async fn status_card_token_usage_excludes_cached_tokens() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 0,
        total_tokens: 2_100,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
        .single()
        .expect("timestamp");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));

    assert!(
        rendered.iter().all(|line| !line.contains("cached")),
        "cached tokens should not be displayed, got: {rendered:?}"
    );
}

#[tokio::test]
async fn status_snapshot_truncates_in_narrow_terminal() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 150,
        total_tokens: 2_250,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 72,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 600)),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let reasoning_effort_override = Some(Some(ReasoningEffort::High));
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        reasoning_effort_override,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 70));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");

    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_truncates_halfwidth_kana_in_narrow_terminal() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account = StatusAccountDisplay::ChatGpt {
        email: Some("ｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟ@example.com".to_string()),
        plan: Some("ｶﾞﾊﾟ plan".to_string()),
    };
    let usage = TokenUsage::default();
    let now = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let composite = new_status_output(
        &config,
        Some(&account),
        /*token_info*/ None,
        &usage,
        &None,
        Some("ｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟ thread".to_string()),
        /*forked_from*/ None,
        /*rate_limits*/ None,
        /*plan_type*/ None,
        now,
        "ｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟ-model",
        Some("ｶﾞﾊﾟ collaboration mode"),
        /*reasoning_effort_override*/ None,
    );
    let rendered_lines = render_lines(&composite.display_lines(/*width*/ 42));
    let sanitized = sanitize_directory(rendered_lines).join("\n");

    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_missing_limits_message() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_uses_default_reasoning_when_config_empty() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");
    let remote_connection = RemoteConnectionStatus {
        address: "unix:///tmp/codex-home/app-server-control/app-server-control.sock".to_string(),
        version: "v0.133.0".to_string(),
    };

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let (composite, _) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        Some(&remote_connection),
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        &[],
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ Some(Some(ReasoningEffort::Medium)),
        "<none>".to_string(),
        /*refreshing_rate_limits*/ false,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_refreshing_limits_notice() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 45,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 900)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 30,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 2_700)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output_with_rate_limits(
        &config,
        /*account_display*/ None,
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        std::slice::from_ref(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        /*refreshing_rate_limits*/ true,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn transcript_overlay_remeasures_status_after_rate_limit_refresh() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    let usage = TokenUsage::default();
    let now = Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());

    let (status, handle) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        /*remote_connection*/ None,
        /*account_display*/ None,
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ &[],
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ true,
    );
    let mut overlay =
        TranscriptOverlay::new(vec![Arc::new(status)], RuntimeKeymap::defaults().pager);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 30,
    );
    let mut buffer = Buffer::empty(area);
    overlay.render(area, &mut buffer);
    let before = buffer_to_text(&buffer, area.width);

    handle.finish_rate_limit_refresh(
        &[RateLimitSnapshotDisplay {
            limit_name: "spark".to_string(),
            captured_at: now,
            primary: Some(RateLimitWindowDisplay {
                used_percent: 45.0,
                resets_at: Some("soon".to_string()),
                window_minutes: Some(300),
            }),
            secondary: Some(RateLimitWindowDisplay {
                used_percent: 30.0,
                resets_at: Some("later".to_string()),
                window_minutes: Some(10_080),
            }),
            credits: None,
            individual_limit: None,
        }],
        now,
    );
    overlay.insert_cell(Arc::new(PlainHistoryCell::new(vec!["next message".into()])));
    buffer = Buffer::empty(area);
    overlay.render(area, &mut buffer);
    let after = buffer_to_text(&buffer, area.width);

    assert!(
        after.contains("spark limit"),
        "status output was clipped: {after:?}"
    );
    assert!(
        after.contains("5h limit"),
        "status output was clipped: {after:?}"
    );
    assert!(
        after.contains("Weekly limit"),
        "status output was clipped: {after:?}"
    );
    insta::assert_snapshot!(
        "transcript_overlay_status_rate_limit_refresh",
        format!("before:\n{before}\n\nafter:\n{after}")
    );
}

#[tokio::test]
async fn status_snapshot_includes_credits_and_limits() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_500,
        cached_input_tokens: 100,
        output_tokens: 600,
        reasoning_output_tokens: 0,
        total_tokens: 2_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 7, 8, 9, 10, 11)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 45,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 900)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 30,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 2_700)),
        }),
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: None,
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_unavailable_limits_message() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_treats_refreshing_empty_limits_as_unavailable() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output_with_rate_limits(
        &config,
        /*account_display*/ None,
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        std::slice::from_ref(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        /*refreshing_rate_limits*/ true,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_stale_limits_message() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 150,
        total_tokens: 2_250,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 72,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 600)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 40,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 1_800)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let now = captured_at + ChronoDuration::minutes(20);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_cached_limits_hide_credits_without_flag() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 900,
        cached_input_tokens: 200,
        output_tokens: 350,
        reasoning_output_tokens: 0,
        total_tokens: 1_450,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 9, 10, 11, 12, 13)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 60,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 1_200)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 35,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 2_400)),
        }),
        credits: Some(CreditsSnapshot {
            has_credits: false,
            unlimited: false,
            balance: Some("80".to_string()),
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let now = captured_at + ChronoDuration::minutes(20);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_context_window_uses_last_usage() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model_context_window = Some(272_000);

    let account_display = test_status_account_display();
    let total_usage = TokenUsage {
        input_tokens: 12_800,
        cached_input_tokens: 0,
        output_tokens: 879,
        reasoning_output_tokens: 0,
        total_tokens: 102_000,
    };
    let last_usage = TokenUsage {
        input_tokens: 12_800,
        cached_input_tokens: 0,
        output_tokens: 879,
        reasoning_output_tokens: 0,
        total_tokens: 13_679,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("timestamp");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = TokenUsageInfo {
        total_token_usage: total_usage.clone(),
        last_token_usage: last_usage,
        model_context_window: config.model_context_window,
    };
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &total_usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    let context_line = rendered_lines
        .into_iter()
        .find(|line| line.contains("Context window"))
        .expect("context line");

    assert!(
        context_line.contains("13.7K used / 272K"),
        "expected context line to reflect last usage tokens, got: {context_line}"
    );
    assert!(
        !context_line.contains("102K"),
        "context line should not use total aggregated tokens, got: {context_line}"
    );
}

fn token_totals() -> codex_whisply::ContextualUsageTokenTotals {
    codex_whisply::ContextualUsageTokenTotals {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
    }
}

fn managed_usage_snapshot(stale: bool) -> codex_whisply::ContextualUsageSnapshot {
    let window = |category, kind, used_fraction, settled, reserved, cap| {
        codex_whisply::ContextualUsageWindow {
            category,
            window: kind,
            settled,
            reserved,
            cap,
            used_fraction,
            starts_at: Some("2024-06-07T00:00:00Z".to_string()),
            resets_at: Some("2024-06-07T12:00:00Z".to_string()),
            rate_card_version: Some("rate-card-9".to_string()),
        }
    };
    codex_whisply::ContextualUsageSnapshot {
        advance_windows: None,
        contract_version: codex_whisply::CONTEXTUAL_ACTION_CONTRACT_VERSION.to_string(),
        tier: "pro".to_string(),
        windows: vec![
            window(
                codex_whisply::UsageWindowCategory::Usage,
                codex_whisply::UsageWindowKind::FiveHour,
                0.42,
                4.200,
                0.500,
                10.0,
            ),
            window(
                codex_whisply::UsageWindowCategory::Usage,
                codex_whisply::UsageWindowKind::Weekly,
                0.10,
                10.000,
                1.000,
                100.0,
            ),
            window(
                codex_whisply::UsageWindowCategory::Transcription,
                codex_whisply::UsageWindowKind::Weekly,
                0.05,
                1.000,
                0.000,
                20.0,
            ),
        ],
        metering: codex_whisply::ContextualUsageMetering {
            contract_version: codex_whisply::USAGE_METERING_CONTRACT_VERSION.to_string(),
            basis: codex_whisply::USAGE_METERING_BASIS.to_string(),
            currency: "USD".to_string(),
            rate_card_version: "rate-card-9".to_string(),
            normalized_units_per_cash_micro: 9,
            tokens: codex_whisply::ContextualUsageTokenWindows {
                five_hour: token_totals(),
                weekly: token_totals(),
            },
        },
        generated_at: "2024-06-07T08:00:00Z".to_string(),
        stale,
    }
}

async fn managed_status_card(
    temp_home: &TempDir,
    snapshot: Option<&codex_whisply::ContextualUsageSnapshot>,
) -> String {
    let mut config = test_config(temp_home).await;
    // The release verifier runs tests with an isolated HOME, so avoid deriving
    // this usage-meter snapshot's layout from the caller's real workspace.
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    let usage = TokenUsage::default();
    let now = Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let (card, handle) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        /*remote_connection*/ None,
        /*account_display*/ None,
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ &[],
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ true,
    );
    handle.finish_managed_usage_refresh(snapshot, now);
    sanitize_directory(render_lines(&card.display_lines(/*width*/ 100))).join("\n")
}

#[tokio::test]
async fn managed_status_shows_the_same_meters_whisply_usage_prints() {
    let temp_home = TempDir::new().expect("temp home");
    let snapshot = managed_usage_snapshot(/*stale*/ false);
    let rendered = managed_status_card(&temp_home, Some(&snapshot)).await;

    for expected in [
        "5h limit",
        "Weekly limit",
        "Transcription weekly",
        "settled 4.200, reserved 0.500, cap 10.000",
        "Rate card",
        "rate-card-9",
    ] {
        assert!(
            rendered.contains(expected),
            "expected /status to show {expected:?}, got: {rendered}"
        );
    }
    assert!(
        !rendered.contains("data not available yet"),
        "a managed session has meters to show, got: {rendered}"
    );
    insta::assert_snapshot!("managed_status_usage_meters", rendered);
}

#[tokio::test]
async fn managed_status_reports_a_stale_snapshot_as_stale() {
    let temp_home = TempDir::new().expect("temp home");
    let snapshot = managed_usage_snapshot(/*stale*/ true);
    let rendered = managed_status_card(&temp_home, Some(&snapshot)).await;

    assert!(
        rendered.contains("5h limit") && rendered.contains("may be stale"),
        "a stale snapshot must still show its meters and say so, got: {rendered}"
    );
}

#[tokio::test]
async fn managed_status_that_cannot_read_usage_does_not_promise_a_later_refresh() {
    let temp_home = TempDir::new().expect("temp home");
    let rendered = managed_status_card(&temp_home, /*snapshot*/ None).await;

    assert!(
        rendered.contains("Limits:") && rendered.contains("not available for this account"),
        "a failed read must settle the section, got: {rendered}"
    );
    assert!(
        !rendered.contains("run /status again shortly"),
        "nothing else will fill this section in, got: {rendered}"
    );
}
