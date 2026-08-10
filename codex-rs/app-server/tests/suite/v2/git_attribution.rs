use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
#[cfg(target_os = "macos")]
use app_test_support::ManagedWhisplyGatewayFixture;
use app_test_support::TestAppServer;
#[cfg(target_os = "macos")]
use app_test_support::create_final_assistant_message_sse_response;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::TurnStartParams;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::UserInput;
#[cfg(target_os = "macos")]
use core_test_support::responses;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
const WHISPLY_MANAGED_ACCOUNT_LOGIN_ERROR: &str =
    "Whisply account login is managed by the installed app. Sign in through Whisply.";
const COMMIT_ATTRIBUTION: &str = "Co-authored-by: Codex <noreply@openai.com>";
const PR_ATTRIBUTION: &str = "Generated with [Codex](https://openai.com/codex/).";
const ATTRIBUTION_DISABLED: &str = "attribution is disabled for the current workspace";

// Workspace attribution previously depended on a direct ChatGPT account,
// backend settings request, and configured base URL. BrokerOnly Whisply has no
// contract for that route, so its v2 coverage is limited to managed runtime
// behavior and the stable rejection of a direct workspace switch.

#[test]
fn managed_git_attribution_fixture_contains_no_direct_auth_or_endpoint() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    for forbidden in [
        "chatgpt_base_url",
        "openai_base_url",
        "model_providers",
        "base_url",
        "api_key",
        "chatgpt",
    ] {
        assert!(
            !config.contains(forbidden),
            "managed git-attribution fixture must not contain `{forbidden}`"
        );
    }
    assert!(config.contains("model_provider = \"whisply\""));
    assert!(!codex_home.path().join("auth.json").exists());
    Ok(())
}

#[tokio::test]
async fn managed_git_attribution_fixture_initializes_and_starts_a_local_thread() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;

    assert!(!thread.id.is_empty());
    Ok(())
}

#[tokio::test]
async fn direct_workspace_switch_for_git_attribution_is_rejected_stably() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request_id = app_server
        .send_chatgpt_auth_tokens_login_request(
            "e30.e30.c2ln".to_string(),
            "workspace-switch".to_string(),
            Some("enterprise".to_string()),
        )
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, WHISPLY_MANAGED_ACCOUNT_LOGIN_ERROR);
    assert_eq!(error.error.data, None);
    Ok(())
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn broker_only_turn_omits_direct_workspace_attribution_context() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![create_final_assistant_message_sse_response("Done")?],
    )
    .await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let managed_gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_managed_whisply_gateway(managed_gateway)
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    app_server
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id,
            input: vec![UserInput::Text {
                text: "make a local change".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 1);
    let developer_text = requests[0].message_input_texts("developer").join("\n");
    assert!(!developer_text.contains(COMMIT_ATTRIBUTION));
    assert!(!developer_text.contains(PR_ATTRIBUTION));
    assert!(!developer_text.contains(ATTRIBUTION_DISABLED));
    app_server.assert_managed_whisply_gateway_healthy()?;
    Ok(())
}
