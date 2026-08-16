use std::collections::BTreeMap;
use whisply_tools::JsonSchema;
use whisply_tools::ResponsesApiTool;
use whisply_tools::ToolSpec;

pub(crate) const NEW_CONTEXT_WINDOW_TOOL_NAME: &str = "new_context";

pub fn create_new_context_window_tool() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: NEW_CONTEXT_WINDOW_TOOL_NAME.to_string(),
        description: "Start a new context window. Does not clear, reset, or otherwise affect environment state.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(BTreeMap::new(), /*required*/ None, Some(false.into())),
        output_schema: None,
    })
}
