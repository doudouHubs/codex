use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub const CODEX_DASHBOARD_TOOL_NAME: &str = "codex_dashboard";

pub fn create_codex_dashboard_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "process_id".to_string(),
            JsonSchema::string(Some(
                "Supervisor process id. Omit to list all registered Codex processes.".to_string(),
            )),
        ),
        (
            "section".to_string(),
            JsonSchema::string_enum(
                vec![
                    json!("prompt"),
                    json!("plan"),
                    json!("messages"),
                    json!("toolCalls"),
                ],
                Some("Work section to read when process_id is provided.".to_string()),
            ),
        ),
        (
            "cursor".to_string(),
            JsonSchema::string(Some("Cursor returned by a previous page.".to_string())),
        ),
        (
            "limit".to_string(),
            JsonSchema::integer(Some(
                "Maximum number of items to return, capped by supervisor.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: CODEX_DASHBOARD_TOOL_NAME.to_string(),
        description: "Query the local Codex supervisor for every registered Codex process, including lifecycle status, activity, mode, summary, and paginated prompt, plan, message, or tool-call details. Query failures are returned explicitly.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, /*required*/ None, Some(false.into())),
        output_schema: None,
    })
}
