use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::codex_dashboard_spec::CODEX_DASHBOARD_TOOL_NAME;
use crate::tools::handlers::codex_dashboard_spec::create_codex_dashboard_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_supervisor::SupervisorClient;
use codex_supervisor::WorkSection;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;

pub struct CodexDashboardHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexDashboardArgs {
    process_id: Option<String>,
    section: Option<WorkSection>,
    cursor: Option<String>,
    limit: Option<u32>,
}

impl ToolExecutor<ToolInvocation> for CodexDashboardHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(CODEX_DASHBOARD_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_codex_dashboard_tool()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl CodexDashboardHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let ToolPayload::Function { arguments } = invocation.payload else {
            return Err(FunctionCallError::RespondToModel(format!(
                "{CODEX_DASHBOARD_TOOL_NAME} received unsupported payload"
            )));
        };
        let args: CodexDashboardArgs = parse_arguments(&arguments)?;
        let client = SupervisorClient::connect().await.map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "{CODEX_DASHBOARD_TOOL_NAME} could not connect to supervisor: {error:#}"
            ))
        })?;
        let value = match args.process_id {
            None if args.section.is_none() => {
                serde_json::to_value(client.snapshot().await.map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "{CODEX_DASHBOARD_TOOL_NAME} snapshot failed: {error:#}"
                    ))
                })?)
                .map_err(|error| FunctionCallError::Fatal(error.to_string()))?
            }
            None => {
                return Err(FunctionCallError::RespondToModel(
                    "section requires process_id".to_string(),
                ));
            }
            Some(process_id) => {
                let process_id = uuid::Uuid::parse_str(&process_id).map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "invalid supervisor process_id `{process_id}`: {error}"
                    ))
                })?;
                match args.section {
                    None => {
                        let snapshot = client.snapshot().await.map_err(|error| {
                            FunctionCallError::RespondToModel(format!(
                                "{CODEX_DASHBOARD_TOOL_NAME} snapshot failed: {error:#}"
                            ))
                        })?;
                        let process = snapshot
                            .processes
                            .into_iter()
                            .find(|process| process.id == process_id)
                            .ok_or_else(|| {
                                FunctionCallError::RespondToModel(format!(
                                    "supervisor process {process_id} was not found"
                                ))
                            })?;
                        serde_json::to_value(process)
                            .map_err(|error| FunctionCallError::Fatal(error.to_string()))?
                    }
                    Some(section) => serde_json::to_value(
                        client
                            .work_page(process_id, section, args.cursor, args.limit.unwrap_or(20))
                            .await
                            .map_err(|error| {
                                FunctionCallError::RespondToModel(format!(
                                    "{CODEX_DASHBOARD_TOOL_NAME} work query failed: {error:#}"
                                ))
                            })?,
                    )
                    .map_err(|error| FunctionCallError::Fatal(error.to_string()))?,
                }
            }
        };
        let text = serde_json::to_string_pretty(&value)
            .map_err(|error| FunctionCallError::Fatal(error.to_string()))?;
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            text,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for CodexDashboardHandler {}
