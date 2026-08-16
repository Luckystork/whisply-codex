//! What a plugin's own helper processes are allowed to inherit.
//!
//! Installing or updating a plugin runs `git` and `npm`: real programs,
//! reaching a remote the person named, running as that person. They need the
//! person's own configuration and they must not be told anything of Whisply's.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

fn withheld(command: &Command) -> Vec<OsString> {
    command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name.to_os_string())
        .collect()
}

fn assert_tells_a_helper_nothing_of_whisplys(command: &Command, what: &str) {
    let withheld = withheld(command);
    for name in [
        "WHISPLY_HOME",
        "WHISPLY_GATEWAY_AUTH_FD",
        "WHISPLY_GATEWAY_ENDPOINT_FD",
        "WHISPLY_NATIVE_BROKER_CAPABILITY_FD",
        "WHISPLY_NATIVE_BROKER_SOCKET",
    ] {
        assert!(
            withheld.iter().any(|withheld| withheld == OsStr::new(name)),
            "{what} still inherits {name}"
        );
    }
}

fn assert_keeps_the_persons_own_setup(command: &Command, what: &str) {
    let withheld = withheld(command);
    for name in [
        "PATH",
        "HOME",
        "SSH_AUTH_SOCK",
        "NPM_TOKEN",
        "NPM_CONFIG_REGISTRY",
        "GIT_SSH_COMMAND",
        "HTTPS_PROXY",
    ] {
        assert!(
            !withheld.iter().any(|withheld| withheld == OsStr::new(name)),
            "{what} no longer inherits {name}; a private registry or a git \
             remote the person configured would stop working"
        );
    }
}

#[test]
fn refreshing_a_plugins_source_is_told_nothing_of_whisplys() {
    let command = crate::loader::git_command(&["fetch", "--all"], Some(Path::new("/tmp")));

    assert_tells_a_helper_nothing_of_whisplys(&command, "a plugin source refresh");
    assert_keeps_the_persons_own_setup(&command, "a plugin source refresh");
}

#[test]
fn adding_a_marketplace_is_told_nothing_of_whisplys() {
    let command = crate::marketplace_add::install::git_command(&["clone"], None);

    assert_tells_a_helper_nothing_of_whisplys(&command, "a marketplace clone");
    assert_keeps_the_persons_own_setup(&command, "a marketplace clone");
}

#[test]
fn upgrading_a_marketplace_is_told_nothing_of_whisplys() {
    let command = crate::marketplace_upgrade::git::git_command();

    assert_tells_a_helper_nothing_of_whisplys(&command, "a marketplace upgrade");
    assert_keeps_the_persons_own_setup(&command, "a marketplace upgrade");
}

#[test]
fn fetching_a_plugins_package_is_told_nothing_of_whisplys() {
    let command = crate::npm_source::npm_pack_command(
        Path::new("/tmp"),
        "some-plugin@1.2.3",
        Some("https://registry.example.invalid"),
        OsStr::new("npm"),
    );

    assert_tells_a_helper_nothing_of_whisplys(&command, "an npm plugin fetch");
    assert_keeps_the_persons_own_setup(&command, "an npm plugin fetch");
}

#[test]
fn withholding_is_not_a_way_to_lose_the_argument_that_was_asked_for() {
    let command = crate::npm_source::npm_pack_command(
        Path::new("/tmp"),
        "some-plugin@1.2.3",
        Some("https://registry.example.invalid"),
        OsStr::new("npm"),
    );

    let args: Vec<_> = command.get_args().map(OsStr::to_os_string).collect();
    assert!(args.iter().any(|arg| arg == OsStr::new("--ignore-scripts")));
    assert!(
        args.iter()
            .any(|arg| arg == OsStr::new("https://registry.example.invalid"))
    );
    assert!(
        args.iter()
            .any(|arg| arg == OsStr::new("some-plugin@1.2.3"))
    );
}
