#![cfg(test)]

use crate::status::StatusAccountDisplay;

#[test]
fn managed_account_display_is_in_memory_only() {
    let display = StatusAccountDisplay::ChatGpt {
        email: None,
        plan: Some("Enterprise (Automation)".to_string()),
    };

    assert!(matches!(
        display,
        StatusAccountDisplay::ChatGpt {
            email: None,
            plan: Some(ref plan),
        } if plan == "Enterprise (Automation)"
    ));
}
