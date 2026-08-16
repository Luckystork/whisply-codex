use std::collections::HashMap;
#[cfg(windows)]
use std::fs;

use pretty_assertions::assert_eq;
use tempfile::tempdir;
use whisply_protocol::protocol::HookEventName;
use whisply_protocol::protocol::HookSource;
use whisply_utils_absolute_path::AbsolutePathBuf;

use super::CommandShell;
use super::ConfiguredHandler;
use super::run_command;

#[cfg(windows)]
#[tokio::test]
async fn cmd_shell_runs_quoted_hook_command_path() {
    let temp = tempdir().expect("create temp dir");
    let hook_dir = temp.path().join("hook with spaces");
    fs::create_dir(&hook_dir).expect("create hook dir");
    let hook_path = hook_dir.join("hook.cmd");
    fs::write(
        &hook_path,
        "@echo off\r\nif not \"%~1\"==\"notify\" exit /B 7\r\necho hook-ran\r\n",
    )
    .expect("write hook command");
    let source_path =
        AbsolutePathBuf::try_from(hook_path.clone()).expect("absolute hook command path");
    let handler = ConfiguredHandler {
        event_name: HookEventName::SessionStart,
        matcher: None,
        command: format!(r#""{}" notify"#, hook_path.display()),
        timeout_sec: 10,
        status_message: None,
        additional_context_limit: Default::default(),
        source_path,
        source: HookSource::User,
        display_order: 0,
        env: HashMap::new(),
    };
    let shells = [
        CommandShell {
            program: String::new(),
            args: Vec::new(),
        },
        CommandShell {
            program: std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()),
            args: vec!["/c".to_string()],
        },
    ];

    for shell in shells {
        let result = run_command(
            &shell,
            &handler,
            /*configured_order*/ 0,
            "{}",
            temp.path(),
        )
        .await;

        assert_eq!(result.exit_code, Some(0), "stderr: {}", result.stderr);
        assert_eq!(result.stdout.trim(), "hook-ran");
        assert!(result.error.is_none());
    }
}

#[tokio::test]
async fn fast_exiting_hook_preserves_stdout_when_stdin_is_not_consumed() {
    let temp = tempdir().expect("create temp dir");
    let source_path = AbsolutePathBuf::try_from(temp.path().join("hooks.json"))
        .expect("absolute hook configuration path");
    let handler = ConfiguredHandler {
        event_name: HookEventName::SessionStart,
        matcher: None,
        command: "echo hook-ran".to_string(),
        timeout_sec: 10,
        status_message: None,
        additional_context_limit: Default::default(),
        source_path,
        source: HookSource::User,
        display_order: 0,
        env: HashMap::new(),
    };
    let shell = CommandShell {
        program: String::new(),
        args: Vec::new(),
    };
    let input_json = format!(r#"{{"padding":"{}"}}"#, "x".repeat(1024 * 1024));

    let result = run_command(
        &shell,
        &handler,
        /*configured_order*/ 0,
        &input_json,
        temp.path(),
    )
    .await;

    assert_eq!(result.exit_code, Some(0), "stderr: {}", result.stderr);
    assert_eq!(result.stdout.trim(), "hook-ran");
    assert_eq!(result.error, None);
}

/// A hook is a command the person wrote, so it keeps their environment. What
/// it does not keep are the handles that say where this runtime's own
/// credentials can be reached: a hook has no use for them, and by the time it
/// runs the descriptors those numbers name are already closed.
#[cfg(not(windows))]
mod what_a_hook_inherits {
    use std::collections::HashMap;
    use std::ffi::OsStr;

    use whisply_protocol::protocol::HookEventName;
    use whisply_protocol::protocol::HookSource;
    use whisply_utils_absolute_path::AbsolutePathBuf;

    use super::super::CommandShell;
    use super::super::ConfiguredHandler;
    use super::super::build_command;

    fn handler(env: HashMap<String, String>) -> ConfiguredHandler {
        ConfiguredHandler {
            event_name: HookEventName::SessionStart,
            matcher: None,
            command: "echo hello".to_string(),
            timeout_sec: 10,
            status_message: None,
            additional_context_limit: Default::default(),
            source_path: AbsolutePathBuf::try_from(std::path::PathBuf::from("/tmp/hooks.toml"))
                .expect("absolute hook source path"),
            source: HookSource::User,
            display_order: 0,
            env,
        }
    }

    fn shell() -> CommandShell {
        CommandShell {
            program: "/bin/sh".to_string(),
            args: vec!["-lc".to_string()],
        }
    }

    #[test]
    fn a_hook_is_not_handed_this_runtimes_authority() {
        let command = build_command(&shell(), &handler(HashMap::new()));

        for name in [
            "WHISPLY_GATEWAY_AUTH_FD",
            "WHISPLY_GATEWAY_ENDPOINT_FD",
            "WHISPLY_NATIVE_BROKER_CAPABILITY_FD",
            "WHISPLY_NATIVE_BROKER_SOCKET",
        ] {
            assert!(
                command.as_std().get_envs().any(|(configured, value)| {
                    configured == OsStr::new(name) && value.is_none()
                }),
                "{name} still reaches a hook command"
            );
        }
    }

    #[test]
    fn a_hook_still_runs_against_the_account_it_was_configured_in() {
        let command = build_command(&shell(), &handler(HashMap::new()));

        assert!(
            !command
                .as_std()
                .get_envs()
                .any(|(configured, _)| configured == OsStr::new("WHISPLY_HOME")),
            "a hook that shells out to whisply should reach the same account \
             the person wrote it in, not silently a different one"
        );
    }

    #[test]
    fn a_hook_that_sets_one_of_those_names_itself_still_gets_its_own_value() {
        let declared = HashMap::from([(
            "WHISPLY_NATIVE_BROKER_SOCKET".to_string(),
            "/tmp/the-hooks-own-socket".to_string(),
        )]);

        let command = build_command(&shell(), &handler(declared));

        assert!(
            command.as_std().get_envs().any(|(configured, value)| {
                configured == OsStr::new("WHISPLY_NATIVE_BROKER_SOCKET")
                    && value == Some(OsStr::new("/tmp/the-hooks-own-socket"))
            }),
            "withholding is about what the parent happens to be carrying; it \
             does not overrule what the person configured"
        );
    }
}
