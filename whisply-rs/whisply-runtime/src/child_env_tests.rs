use super::*;

fn withheld_by_authority() -> Vec<String> {
    let mut names = Vec::new();
    withhold_runtime_authority(|name| names.push(name.to_string_lossy().into_owned()));
    names
}

#[test]
fn every_handle_that_names_this_runtimes_authority_is_withheld() {
    let names = withheld_by_authority();

    for expected in [
        "WHISPLY_GATEWAY_AUTH_FD",
        "WHISPLY_GATEWAY_ENDPOINT_FD",
        "WHISPLY_NATIVE_BROKER_CAPABILITY_FD",
        "WHISPLY_NATIVE_BROKER_SOCKET",
        "WHISPLY_RUNTIME_BROKER_CONTROL_ONLY",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} still reaches a child process; it says where this \
             account's credentials can be reached"
        );
    }
}

#[test]
fn a_hook_still_knows_which_account_it_was_configured_in() {
    let names = withheld_by_authority();

    assert!(
        !names.iter().any(|name| name == "WHISPLY_HOME"),
        "a command the person wrote should reach the same account they wrote it \
         in, so the storage root is not part of the authority level"
    );
}

#[test]
fn code_the_product_did_not_write_is_told_nothing_of_whisplys() {
    let mut names = Vec::new();
    withhold_reserved_environment(|name| names.push(name.to_string_lossy().into_owned()));

    for expected in [
        "WHISPLY_GATEWAY_AUTH_FD",
        "WHISPLY_GATEWAY_ENDPOINT_FD",
        "WHISPLY_NATIVE_BROKER_CAPABILITY_FD",
        "WHISPLY_NATIVE_BROKER_SOCKET",
        "WHISPLY_HOME",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} still reaches a plugin's own helper process"
        );
    }
}

#[test]
fn a_variable_added_to_the_namespace_later_is_covered_the_day_it_is_added() {
    let present = reserved_names_in(
        [
            "WHISPLY_SOMETHING_INVENTED_TOMORROW",
            "PATH",
            "HOME",
            "NPM_CONFIG_REGISTRY",
        ]
        .into_iter()
        .map(OsString::from),
    );

    assert_eq!(
        present,
        vec![OsString::from("WHISPLY_SOMETHING_INVENTED_TOMORROW")],
        "the rule is the namespace, not a list someone has to remember to \
         extend"
    );
}

#[test]
fn the_persons_own_environment_is_left_alone() {
    let mut names = Vec::new();
    withhold_reserved_environment(|name| names.push(name.to_string_lossy().into_owned()));

    for kept in [
        "PATH",
        "HOME",
        "SSH_AUTH_SOCK",
        "NPM_TOKEN",
        "GIT_SSH_COMMAND",
    ] {
        assert!(
            !names.iter().any(|name| name == kept),
            "{kept} is the person's own configuration; withholding it would \
             break a private registry or a git remote they set up on purpose"
        );
    }
}
