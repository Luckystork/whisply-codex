use super::*;
use maplit::hashmap;
use pretty_assertions::assert_eq;
use whisply_protocol::config_types::ShellEnvironmentPolicyInherit;

fn make_vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn inject_permission_profile_env_overrides_policy_value() {
    let mut env = HashMap::from([(
        CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
        "stale-profile".to_string(),
    )]);

    inject_permission_profile_env(
        &mut env,
        Some(&ActivePermissionProfile::new("current-profile")),
    );

    assert_eq!(
        env.get(CODEX_PERMISSION_PROFILE_ENV_VAR)
            .map(String::as_str),
        Some("current-profile")
    );
}

#[test]
fn inject_permission_profile_env_removes_stale_value_without_active_profile() {
    let mut env = HashMap::from([(
        CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
        "stale-profile".to_string(),
    )]);

    inject_permission_profile_env(&mut env, /*active_permission_profile*/ None);

    assert_eq!(env.get(CODEX_PERMISSION_PROFILE_ENV_VAR), None);
}

#[cfg(target_os = "windows")]
#[test]
fn inject_permission_profile_env_replaces_differently_cased_windows_key() {
    let mut env = HashMap::from([(
        "codex_permission_profile".to_string(),
        "stale-profile".to_string(),
    )]);

    inject_permission_profile_env(
        &mut env,
        Some(&ActivePermissionProfile::new("current-profile")),
    );

    assert_eq!(
        env,
        HashMap::from([(
            CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
            "current-profile".to_string(),
        )])
    );
}

#[test]
fn test_core_inherit_defaults_keep_sensitive_vars() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/user"),
        ("API_KEY", "secret"),
        ("SECRET_TOKEN", "t"),
    ]);

    let policy = ShellEnvironmentPolicy::default(); // inherit All, default excludes ignored
    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
        "HOME".to_string() => "/home/user".to_string(),
        "API_KEY".to_string() => "secret".to_string(),
        "SECRET_TOKEN".to_string() => "t".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_core_inherit_with_default_excludes_enabled() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/user"),
        ("API_KEY", "secret"),
        ("SECRET_TOKEN", "t"),
    ]);

    let policy = ShellEnvironmentPolicy {
        ignore_default_excludes: false, // apply KEY/SECRET/TOKEN filter
        ..Default::default()
    };
    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
        "HOME".to_string() => "/home/user".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_include_only() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("FOO", "bar")]);

    let policy = ShellEnvironmentPolicy {
        // skip default excludes so nothing is removed prematurely
        ignore_default_excludes: true,
        include_only: vec![EnvironmentVariablePattern::new_case_insensitive("*PATH")],
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_set_overrides() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);

    let mut policy = ShellEnvironmentPolicy {
        ignore_default_excludes: true,
        ..Default::default()
    };
    policy.r#set.insert("NEW_VAR".to_string(), "42".to_string());

    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
        "NEW_VAR".to_string() => "42".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn create_env_inserts_thread_id() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);
    let policy = ShellEnvironmentPolicy::default();
    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn create_env_omits_thread_id_when_missing() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);
    let policy = ShellEnvironmentPolicy::default();
    let result = create_env_from_vars(vars, &policy, /*thread_id*/ None);

    let expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };

    assert_eq!(result, expected);
}

#[test]
fn test_inherit_all() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("FOO", "bar")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: true, // keep everything
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars.clone(), &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = vars.into_iter().collect();
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());
    assert_eq!(result, expected);
}

#[test]
fn test_inherit_all_with_default_excludes() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("API_KEY", "secret")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: false,
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());
    assert_eq!(result, expected);
}

#[test]
#[cfg(target_os = "windows")]
fn test_core_inherit_respects_case_insensitive_names_on_windows() {
    let vars = make_vars(&[
        ("Path", "C:\\Windows\\System32"),
        ("PathExt", ".COM;.EXE;.BAT;.CMD"),
        ("TEMP", "C:\\Temp"),
        ("FOO", "bar"),
    ]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::Core,
        ignore_default_excludes: true,
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = hashmap! {
        "Path".to_string() => "C:\\Windows\\System32".to_string(),
        "PathExt".to_string() => ".COM;.EXE;.BAT;.CMD".to_string(),
        "TEMP".to_string() => "C:\\Temp".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
#[cfg(target_os = "windows")]
fn create_env_inserts_pathext_on_windows_when_missing() {
    let vars = make_vars(&[]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ignore_default_excludes: true,
        ..Default::default()
    };

    let result = create_env_from_vars(vars, &policy, /*thread_id*/ None);

    let expected: HashMap<String, String> = hashmap! {
        "PATHEXT".to_string() => ".COM;.EXE;.BAT;.CMD".to_string(),
    };
    assert_eq!(result, expected);
}

#[test]
#[cfg(target_os = "windows")]
fn create_env_preserves_existing_pathext_case_insensitively_on_windows() {
    let vars = make_vars(&[("PathExt", ".COM;.EXE;.BAT;.CMD;.PS1")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::Core,
        ignore_default_excludes: true,
        ..Default::default()
    };

    let result = create_env_from_vars(vars, &policy, /*thread_id*/ None);

    let pathext_vars = result
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("PATHEXT"))
        .collect::<Vec<_>>();

    assert_eq!(pathext_vars.len(), 1);
    assert_eq!(pathext_vars[0].1, ".COM;.EXE;.BAT;.CMD;.PS1");
}

#[test]
fn test_inherit_none() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("HOME", "/home")]);

    let mut policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ignore_default_excludes: true,
        ..Default::default()
    };
    policy
        .r#set
        .insert("ONLY_VAR".to_string(), "yes".to_string());

    let thread_id = ThreadId::new();
    let result = create_env_from_vars(vars, &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = hashmap! {
        "ONLY_VAR".to_string() => "yes".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());
    assert_eq!(result, expected);
}

/// WCD-718: the handles that say where this runtime's credentials and native
/// broker can be reached are withheld from every child Whisply starts. The
/// child running a command a model produced was the exception, and is the one
/// with the least claim to them.
#[test]
fn a_command_the_model_wrote_is_not_told_where_this_runtimes_authority_lives() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        ("WHISPLY_GATEWAY_AUTH_FD", "3"),
        ("WHISPLY_GATEWAY_ENDPOINT_FD", "4"),
        ("WHISPLY_NATIVE_BROKER_CAPABILITY_FD", "42"),
        (
            "WHISPLY_NATIVE_BROKER_SOCKET",
            "/tmp/whisply/.native-broker/v1.sock",
        ),
    ]);

    let result = create_env_from_vars(vars, &ShellEnvironmentPolicy::default(), None);

    assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
    for withheld in [
        "WHISPLY_GATEWAY_AUTH_FD",
        "WHISPLY_GATEWAY_ENDPOINT_FD",
        "WHISPLY_NATIVE_BROKER_CAPABILITY_FD",
        "WHISPLY_NATIVE_BROKER_SOCKET",
    ] {
        assert!(
            !result.contains_key(withheld),
            "{withheld} reached a command the model wrote"
        );
    }
}

/// Configuration decides what a command inherits from the person's own
/// environment. It does not get to hand out this process's descriptors: the
/// numbers name descriptors that are closed by the time the command runs, so
/// putting them back describes something that is not there.
#[test]
fn no_configured_policy_can_hand_those_handles_back() {
    let vars = make_vars(&[("WHISPLY_NATIVE_BROKER_SOCKET", "/tmp/inherited.sock")]);
    let mut policy = ShellEnvironmentPolicy {
        include_only: vec![EnvironmentVariablePattern::new_case_insensitive(
            "WHISPLY_*",
        )],
        ..Default::default()
    };
    policy.r#set.insert(
        "WHISPLY_NATIVE_BROKER_CAPABILITY_FD".to_string(),
        "42".to_string(),
    );

    let result = create_env_from_vars(vars, &policy, None);

    assert!(
        result.is_empty(),
        "authority handles survived the policy: {result:?}"
    );
}

/// The person's own environment is theirs. Whisply's storage root is not an
/// authority handle, and a command that shells out to `whisply` should reach
/// the account the person is working in.
#[test]
fn the_rest_of_the_environment_is_left_alone() {
    let vars = make_vars(&[
        ("WHISPLY_HOME", "/Users/someone/Library/Whisply"),
        ("SSH_AUTH_SOCK", "/tmp/ssh.sock"),
        ("NPM_TOKEN", "secret"),
    ]);

    let result = create_env_from_vars(vars, &ShellEnvironmentPolicy::default(), None);

    assert_eq!(
        result.get("WHISPLY_HOME").map(String::as_str),
        Some("/Users/someone/Library/Whisply")
    );
    assert_eq!(
        result.get("SSH_AUTH_SOCK").map(String::as_str),
        Some("/tmp/ssh.sock")
    );
    assert_eq!(result.get("NPM_TOKEN").map(String::as_str), Some("secret"));
}
