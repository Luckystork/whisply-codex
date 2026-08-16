use whisply_config::HookHandlerConfig;
use whisply_plugin::PluginHookSource;
use whisply_protocol::protocol::HookEventName;
use whisply_protocol::protocol::HookHandlerType;

/// Minimal declaration metadata for one bundled plugin hook handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHookDeclaration {
    pub key: String,
    pub event_name: HookEventName,
    pub handler_type: HookHandlerType,
    /// The shell command a `command` handler will run. `None` for prompt and
    /// agent handlers, which do not execute anything on the user's machine.
    pub command: Option<String>,
    /// Which occurrences of the event the handler is scoped to. `None` means
    /// every occurrence, which is the broader exposure of the two.
    pub matcher: Option<String>,
}

/// Return the hook handlers declared by plugin bundles without projecting live runtime state.
pub fn plugin_hook_declarations(hook_sources: &[PluginHookSource]) -> Vec<PluginHookDeclaration> {
    let mut declarations = Vec::new();

    for source in hook_sources {
        let key_source = plugin_hook_key_source(
            source.plugin_id.as_key().as_str(),
            source.source_relative_path.as_str(),
        );
        for (event_name, groups) in source.hooks.clone().into_matcher_groups() {
            for (group_index, group) in groups.iter().enumerate() {
                for (handler_index, handler) in group.hooks.iter().enumerate() {
                    declarations.push(PluginHookDeclaration {
                        key: crate::hook_key(&key_source, event_name, group_index, handler_index),
                        event_name,
                        handler_type: handler_type_of(handler),
                        command: declared_command(handler),
                        matcher: group.matcher.clone(),
                    });
                }
            }
        }
    }

    declarations
}

pub(crate) fn plugin_hook_key_source(plugin_id: &str, source_relative_path: &str) -> String {
    format!("{plugin_id}:{source_relative_path}")
}

fn handler_type_of(handler: &HookHandlerConfig) -> HookHandlerType {
    match handler {
        HookHandlerConfig::Command { .. } => HookHandlerType::Command,
        HookHandlerConfig::Prompt {} => HookHandlerType::Prompt,
        HookHandlerConfig::Agent {} => HookHandlerType::Agent,
    }
}

/// The command line this handler will actually run on this machine.
///
/// Mirrors the platform choice `discovery` makes when it builds the runnable
/// hook, so what a user is shown before installing is what later executes
/// rather than the other platform's command.
fn declared_command(handler: &HookHandlerConfig) -> Option<String> {
    let HookHandlerConfig::Command {
        command,
        command_windows,
        ..
    } = handler
    else {
        return None;
    };
    if cfg!(windows) {
        Some(command_windows.clone().unwrap_or_else(|| command.clone()))
    } else {
        Some(command.clone())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use whisply_config::HookEventsToml;
    use whisply_config::HookHandlerConfig;
    use whisply_config::MatcherGroup;
    use whisply_plugin::PluginId;
    use whisply_utils_absolute_path::test_support::PathBufExt;
    use whisply_utils_absolute_path::test_support::test_path_buf;

    use super::*;

    #[test]
    fn lists_declared_plugin_handlers_with_persisted_hook_keys() {
        let plugin_root = test_path_buf("/tmp/plugin").abs();
        let source_path = plugin_root.join("hooks/hooks.json");
        let declarations = plugin_hook_declarations(&[PluginHookSource {
            plugin_id: PluginId::parse("demo@test").expect("plugin id"),
            plugin_root: plugin_root.clone(),
            plugin_data_root: plugin_root.join("data"),
            source_path,
            source_relative_path: "hooks/hooks.json".to_string(),
            hooks: HookEventsToml {
                pre_tool_use: vec![MatcherGroup {
                    matcher: None,
                    hooks: vec![
                        HookHandlerConfig::Prompt {},
                        HookHandlerConfig::Command {
                            command: "echo hi".to_string(),
                            command_windows: None,
                            timeout_sec: None,
                            r#async: false,
                            status_message: None,
                            additional_context_limit: None,
                        },
                    ],
                }],
                session_start: vec![MatcherGroup {
                    matcher: None,
                    hooks: vec![HookHandlerConfig::Agent {}],
                }],
                ..Default::default()
            },
        }]);

        assert_eq!(
            declarations,
            vec![
                PluginHookDeclaration {
                    key: "demo@test:hooks/hooks.json:pre_tool_use:0:0".to_string(),
                    event_name: HookEventName::PreToolUse,
                    handler_type: HookHandlerType::Prompt,
                    command: None,
                    matcher: None,
                },
                PluginHookDeclaration {
                    key: "demo@test:hooks/hooks.json:pre_tool_use:0:1".to_string(),
                    event_name: HookEventName::PreToolUse,
                    handler_type: HookHandlerType::Command,
                    command: Some("echo hi".to_string()),
                    matcher: None,
                },
                PluginHookDeclaration {
                    key: "demo@test:hooks/hooks.json:session_start:0:0".to_string(),
                    event_name: HookEventName::SessionStart,
                    handler_type: HookHandlerType::Agent,
                    command: None,
                    matcher: None,
                },
            ]
        );
    }

    fn command_handler(command: &str, command_windows: Option<&str>) -> HookHandlerConfig {
        HookHandlerConfig::Command {
            command: command.to_string(),
            command_windows: command_windows.map(str::to_string),
            timeout_sec: None,
            r#async: false,
            status_message: None,
            additional_context_limit: None,
        }
    }

    fn declarations_for(groups: Vec<MatcherGroup>) -> Vec<PluginHookDeclaration> {
        let plugin_root = test_path_buf("/tmp/plugin").abs();
        let source_path = plugin_root.join("hooks/hooks.json");
        plugin_hook_declarations(&[PluginHookSource {
            plugin_id: PluginId::parse("demo@test").expect("plugin id"),
            plugin_root: plugin_root.clone(),
            plugin_data_root: plugin_root.join("data"),
            source_path,
            source_relative_path: "hooks/hooks.json".to_string(),
            hooks: HookEventsToml {
                pre_tool_use: groups,
                ..Default::default()
            },
        }])
    }

    #[test]
    fn a_hook_reports_which_occurrences_it_is_scoped_to() {
        let declarations = declarations_for(vec![
            MatcherGroup {
                matcher: Some("Bash".to_string()),
                hooks: vec![command_handler("guard.py", None)],
            },
            MatcherGroup {
                matcher: None,
                hooks: vec![command_handler("audit.py", None)],
            },
        ]);

        let scopes: Vec<Option<String>> = declarations
            .iter()
            .map(|declaration| declaration.matcher.clone())
            .collect();
        assert_eq!(scopes, vec![Some("Bash".to_string()), None]);
    }

    #[test]
    fn the_disclosed_command_is_the_one_this_platform_will_run() {
        let declarations = declarations_for(vec![MatcherGroup {
            matcher: None,
            hooks: vec![command_handler(
                "./run.sh",
                Some("powershell -File run.ps1"),
            )],
        }]);

        let expected = if cfg!(windows) {
            "powershell -File run.ps1"
        } else {
            "./run.sh"
        };
        assert_eq!(
            declarations[0].command.as_deref(),
            Some(expected),
            "the pre-install disclosure must name the command that later executes"
        );
    }

    #[test]
    fn a_windows_only_override_does_not_replace_the_command_elsewhere() {
        let with_override = declarations_for(vec![MatcherGroup {
            matcher: None,
            hooks: vec![command_handler("./run.sh", Some("run.ps1"))],
        }]);
        let without_override = declarations_for(vec![MatcherGroup {
            matcher: None,
            hooks: vec![command_handler("./run.sh", None)],
        }]);

        if !cfg!(windows) {
            assert_eq!(
                with_override[0].command, without_override[0].command,
                "declaring a Windows command must not change what is shown on other platforms"
            );
        }
    }
}
