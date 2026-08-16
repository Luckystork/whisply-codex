use super::*;

use serde_json::json;

#[test]
fn server_model_tool_name_maps_icon_id_with_dots_to_underscores() {
    assert_eq!(
        server_model_tool_name("whisply.memory"),
        Some("memory".to_string())
    );
    assert_eq!(
        server_model_tool_name("whisply.web_search"),
        Some("web_search".to_string())
    );
    assert_eq!(
        server_model_tool_name("whisply.connector.gmail"),
        Some("connector_gmail".to_string())
    );
    assert_eq!(
        server_model_tool_name("whisply.connector.github"),
        Some("connector_github".to_string())
    );
    assert_eq!(
        server_model_tool_name("whisply.tasks"),
        Some("tasks".to_string())
    );
}

#[test]
fn server_model_tool_ids_match_the_closed_registry_set() {
    let registry_ids = server_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.id)
        .collect::<Vec<_>>();
    assert_eq!(registry_ids, SERVER_MODEL_TOOL_IDS.to_vec());
    for tool_id in SERVER_MODEL_TOOL_IDS {
        assert!(is_server_model_tool_id(tool_id));
        assert!(server_model_tool_name(tool_id).is_some());
    }
}

#[test]
fn execution_rejects_native_and_local_runtime_descriptors() {
    for tool_id in ["whisply.screen.context", "whisply.skills"] {
        let error = ServerToolExecution::new(
            WhisplyToolModelCall {
                execution_id: "call-1".to_string(),
                tool_id: tool_id.to_string(),
                schema_version: 1,
                arguments: json!({"operation": "list"}),
            },
            "thread-1".to_string(),
            "turn-1".to_string(),
        )
        .expect_err("non-server descriptor must not execute");
        assert_eq!(error, ServerToolExecutionError::NonServerTool);
    }
}

#[test]
fn execution_accepts_every_server_descriptor() {
    for descriptor in server_descriptors() {
        ServerToolExecution::new(
            WhisplyToolModelCall {
                execution_id: "call-1".to_string(),
                tool_id: descriptor.id.clone(),
                schema_version: descriptor.schema_version,
                arguments: json!({"operation": "list"}),
            },
            "thread-1".to_string(),
            "turn-1".to_string(),
        )
        .expect("server descriptor should execute");
    }
}
