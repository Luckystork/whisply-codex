use super::*;

use serde_json::json;

#[test]
fn admission_accepts_only_fixed_native_descriptors() {
    let admission = FirstPartyToolAdmission::new(
        "admission-1".to_string(),
        "client-message-1".to_string(),
        [
            "whisply.screen.context".to_string(),
            "whisply.files".to_string(),
        ],
    )
    .expect("native descriptors should be admitted");

    assert!(admission.allows_tool("whisply.screen.context"));
    assert_eq!(
        admission
            .native_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id)
            .collect::<Vec<_>>(),
        vec![
            "whisply.screen.context".to_string(),
            "whisply.files".to_string(),
        ]
    );
    assert_eq!(
        native_model_tool_name("whisply.screen.context"),
        Some("screen_context")
    );
}

#[test]
fn generic_admission_accepts_the_native_owners() {
    // Computer Use is carried here, not granted here. The Mac owner still
    // requires an approved target and holds the call at the real confirmation
    // window before anything happens, so admitting the name advertises a
    // capability the owner can actually deliver.
    for tool_id in ["whisply.computer_use", "whisply.browser", "whisply.chrome"] {
        assert!(is_generic_admitted_native_tool_id(tool_id));
        let admission = FirstPartyToolAdmission::new(
            "admission-1".to_string(),
            "client-message-1".to_string(),
            [tool_id.to_string()],
        )
        .expect("native owner must be admissible through the generic bridge");
        assert!(admission.allows_tool(tool_id));
        assert!(native_model_tool_name(tool_id).is_some());
    }
}

#[test]
fn admission_rejects_server_and_local_runtime_descriptors() {
    for tool_id in ["whisply.connector.gmail", "whisply.skills"] {
        let error = FirstPartyToolAdmission::new(
            "admission-1".to_string(),
            "client-message-1".to_string(),
            [tool_id.to_string()],
        )
        .expect_err("non-native descriptor must not be admitted");
        assert_eq!(error, FirstPartyToolAdmissionError::NonNativeTool);
    }
}

#[test]
fn execution_rechecks_the_admitted_static_descriptor() {
    let admission = FirstPartyToolAdmission::new(
        "admission-1".to_string(),
        "client-message-1".to_string(),
        ["whisply.screen.context".to_string()],
    )
    .expect("admission");
    let error = FirstPartyToolExecution::new(
        admission,
        WhisplyToolModelCall {
            execution_id: "call-1".to_string(),
            tool_id: "whisply.browser".to_string(),
            schema_version: 1,
            arguments: json!({"operation": "open"}),
        },
        "thread-1".to_string(),
        "turn-1".to_string(),
    )
    .expect_err("unadmitted call must fail");
    assert_eq!(error, FirstPartyToolAdmissionError::CallNotAdmitted);
}

#[test]
fn execution_rejects_unbounded_model_correlation() {
    let admission = FirstPartyToolAdmission::new(
        "admission-1".to_string(),
        "client-message-1".to_string(),
        ["whisply.screen.context".to_string()],
    )
    .expect("admission");
    let error = FirstPartyToolExecution::new(
        admission,
        WhisplyToolModelCall {
            execution_id: "x".repeat(257),
            tool_id: "whisply.screen.context".to_string(),
            schema_version: 1,
            arguments: json!({"scope": "exact_window"}),
        },
        "thread-1".to_string(),
        "turn-1".to_string(),
    )
    .expect_err("model correlation must remain bounded");
    assert_eq!(
        error,
        FirstPartyToolAdmissionError::InvalidRuntimeCorrelation
    );
}

#[test]
fn only_computer_use_is_an_auto_review_decision() {
    assert!(first_party_tool_requires_auto_review(
        "whisply.computer_use"
    ));
    for tool_id in [
        "whisply.screen.context",
        "whisply.files",
        "whisply.browser",
        "whisply.chrome",
    ] {
        assert!(
            !first_party_tool_requires_auto_review(tool_id),
            "{tool_id} is not the native Auto skip hole"
        );
    }
}
