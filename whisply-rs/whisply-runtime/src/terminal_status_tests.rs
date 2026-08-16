use super::*;
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;

#[test]
fn generic_failure_keeps_its_historical_exit_code() {
    // Existing automation and the checked-in exec suites assert a generic
    // terminal failure exits 1. Reassigning it would break callers.
    assert_eq!(WhisplyTerminalStatus::Failed.exit_code(), 1);
    assert_eq!(WhisplyTerminalStatus::Succeeded.exit_code(), 0);
}

#[test]
fn every_status_has_a_distinct_stable_identity() {
    let ids: BTreeSet<&str> = WhisplyTerminalStatus::ALL
        .iter()
        .map(|status| status.status_id())
        .collect();
    let codes: BTreeSet<i32> = WhisplyTerminalStatus::ALL
        .iter()
        .map(|status| status.exit_code())
        .collect();

    assert_eq!(ids.len(), WhisplyTerminalStatus::ALL.len());
    assert_eq!(codes.len(), WhisplyTerminalStatus::ALL.len());
}

#[test]
fn no_exit_code_collides_with_the_shell_reserved_range() {
    // 126 is "not executable", 127 is "not found", and 128+n encodes signals.
    // A product code landing in that range would be indistinguishable from a
    // shell-level failure.
    for status in WhisplyTerminalStatus::ALL {
        let code = status.exit_code();
        assert!(
            (0..126).contains(&code),
            "{} uses reserved exit code {code}",
            status.status_id()
        );
    }
}

#[test]
fn only_success_is_a_zero_exit() {
    for status in WhisplyTerminalStatus::ALL {
        assert_eq!(
            status.exit_code() == 0,
            status.is_success(),
            "{} disagrees about success",
            status.status_id()
        );
    }
}

#[test]
fn statuses_round_trip_through_their_wire_form() {
    for status in WhisplyTerminalStatus::ALL {
        let encoded = serde_json::to_string(&status).expect("encode");
        assert_eq!(encoded, format!("\"{}\"", status.status_id()));
        let decoded: WhisplyTerminalStatus = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, status);
    }
}
