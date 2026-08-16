//! Managed Whisply runtime update action.

/// The only update action exposed by the runtime. The owning Whisply app
/// replaces its complete verified Runtime support tree atomically; the CLI
/// never runs a package-manager or standalone installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    ManagedAppBundle,
}

impl UpdateAction {
    /// Public display text only. This is deliberately not an executable shell
    /// command, because a runtime update cannot be installed independently.
    pub fn command_str(self) -> String {
        let _ = self;
        "Whisply app update".to_string()
    }
}

/// Runtime update discovery belongs to the owning app, not this binary.
#[cfg(not(debug_assertions))]
pub fn get_update_action() -> Option<UpdateAction> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_update_action_never_contains_an_installer_command() {
        assert_eq!(
            UpdateAction::ManagedAppBundle.command_str(),
            "Whisply app update"
        );
    }
}
