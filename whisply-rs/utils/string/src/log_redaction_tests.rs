use super::*;
use pretty_assertions::assert_eq;
use std::borrow::Cow;

fn redacted(line: &str) -> String {
    redact_log_line(line).into_owned()
}

#[test]
fn an_authorization_header_keeps_its_scheme_and_loses_its_credential() {
    assert_eq!(
        redacted("sending authorization: Bearer abc123def456 to the gateway"),
        "sending authorization: Bearer <redacted> to the gateway"
    );
}

#[test]
fn a_credential_field_is_scrubbed_whatever_shape_the_call_site_chose() {
    for line in [
        r#"{"access_token":"abc123def456"}"#,
        "access_token=abc123def456",
        "access_token = abc123def456",
        r#"access_token: "abc123def456""#,
        "ACCESS_TOKEN=abc123def456",
    ] {
        assert!(
            !redacted(line).contains("abc123def456"),
            "credential survived in {line}"
        );
    }
}

#[test]
fn the_rest_of_a_structured_line_survives_so_it_is_still_worth_reading() {
    assert_eq!(
        redacted(r#"{"api_key":"abc123def456","model":"gpt-5","attempt":2}"#),
        r#"{"api_key":"<redacted>","model":"gpt-5","attempt":2}"#
    );
}

#[test]
fn a_token_count_is_not_a_credential() {
    let line = "turn complete tokens=1843 cached_tokens=512";
    assert_eq!(redacted(line), line);
}

#[test]
fn browser_execution_lease_is_redacted_without_losing_receipt_or_usage_counts() {
    for key in [
        "x-whisply-contextual-execution-lease",
        "executionLeaseToken",
        "execution_lease_token",
    ] {
        let line = format!(
            r#"{{"{key}":"synthetic-browser-private-lease","tokens":60,"status":"settled"}}"#
        );
        let output = redacted(&line);
        assert!(!output.contains("synthetic-browser-private-lease"));
        assert!(output.contains(r#""tokens":60"#));
        assert!(output.contains(r#""status":"settled""#));
    }
}

#[test]
fn a_sentence_mentioning_a_secret_is_not_treated_as_one() {
    let line = "the client secret was never configured for this provider";
    assert_eq!(redacted(line), line);
}

/// The failure the keyed pass cannot see coming: a call site that logs a body
/// rather than a field. `login` does exactly this with authentication error
/// responses, and an error body can echo the token that produced it.
#[test]
fn a_json_web_token_is_caught_with_no_field_name_to_announce_it() {
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let line = format!("Parsing server error response: {{\"hint\":\"retry with {jwt}\"}}");
    let output = redacted(&line);
    assert!(!output.contains(jwt), "{output}");
    assert!(output.contains("Parsing server error response"), "{output}");
}

#[test]
fn a_provider_api_key_is_caught_by_its_own_shape() {
    for key in [
        "sk-abcdefghijklmnopqrstuvwx",
        "whsec_abcdefghijklmnopqrstuvwx",
        "ghp_abcdefghijklmnopqrstuvwx",
    ] {
        let output = redacted(&format!("provider rejected {key} at the edge"));
        assert!(!output.contains(key), "{key} survived: {output}");
    }
}

#[test]
fn a_short_hyphenated_word_is_not_mistaken_for_a_key() {
    let line = "using sk-test for the fixture";
    assert_eq!(redacted(line), line);
}

/// The product refuses to let an email address become a path segment. A log
/// file that survives the run is the same kind of place.
#[test]
fn an_email_address_does_not_reach_the_log() {
    let output = redacted("resolved account for person@example.com in 12ms");
    assert_eq!(output, "resolved account for <redacted> in 12ms");
}

#[test]
fn an_ordinary_dotted_word_is_left_alone() {
    let line = "whisply_core::session finished in 12.4ms at v1.2.3";
    assert_eq!(redacted(line), line);
}

#[test]
fn several_secrets_on_one_line_are_all_replaced() {
    let output = redacted("user person@example.com key sk-abcdefghijklmnopqrstuvwx done");
    assert_eq!(output, "user <redacted> key <redacted> done");
}

/// A call site that logs a request line or an error body writes the scheme with
/// no field name in front of it, and the credential after it is opaque by
/// design, so neither of the other two passes can see it.
#[test]
fn a_scheme_credential_with_no_field_name_is_still_caught() {
    assert_eq!(
        redacted("refresh failed: Bearer abc123def456xyz"),
        "refresh failed: Bearer <redacted>"
    );
}

/// Sinks hand over whole lines with their terminator attached, so a value that
/// runs to the end of the line is the ordinary case rather than an edge one.
#[test]
fn a_credential_at_the_end_of_a_line_is_still_caught() {
    assert_eq!(
        redacted("refresh failed: Bearer abc123def456xyz\n"),
        "refresh failed: Bearer <redacted>\n"
    );
    assert_eq!(
        redacted("access_token=abc123def456\n"),
        "access_token=<redacted>\n"
    );
}

#[test]
fn an_english_word_after_a_scheme_is_not_a_credential() {
    let line = "Token authentication failed for this provider";
    assert_eq!(redacted(line), line);
}

#[test]
fn scrubbing_the_same_line_twice_does_not_redact_the_marker() {
    let once = redacted("authorization: Bearer abc123def456xyz");
    assert_eq!(redacted(&once), once);
}

#[test]
fn a_clean_line_is_returned_without_copying_it() {
    let line = "starting the app-server for the active account home";
    assert!(matches!(redact_log_line(line), Cow::Borrowed(_)));
}

#[test]
fn exam_usage_proof_is_redacted_bare_or_in_transport_headers() {
    let proof = format!("wus_{}", "a".repeat(43));
    for line in [
        proof.clone(),
        format!("x-whisply-usage-session: {proof}"),
        format!("{{\"sessionToken\":\"{proof}\"}}"),
    ] {
        assert!(!super::redact_log_line(&line).contains(&proof));
    }
}

#[test]
fn exam_transport_reference_is_removed_from_diagnostics() {
    let reference = "70000000-0000-4000-8000-000000000001";
    for key in [
        "whisplyExamTurnReference",
        "whisply_exam_turn_reference",
        "exam_turn_reference",
    ] {
        let line = format!("{{\"{key}\":\"{reference}\",\"tokens\":1234}}");
        let redacted = super::redact_log_line(&line);
        assert!(!redacted.contains(reference));
        assert!(redacted.contains("1234"));
    }
}
