use std::sync::Arc;

use anyhow::bail;
use codex_hooks::HookMcpCall;
use codex_hooks::HookMcpExecutor;
use codex_mcp::McpRuntime;
use codex_protocol::ThreadId;
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;

pub(crate) struct CoreHookMcpExecutor {
    pub(crate) runtime: Arc<McpRuntime>,
    // Hook 调用需要沿用所属线程的 MCP 上下文，尤其是扩展服务依赖的 threadId 元数据。
    pub(crate) thread_id: ThreadId,
}

impl HookMcpExecutor for CoreHookMcpExecutor {
    fn execute(&self, call: HookMcpCall) -> BoxFuture<'_, anyhow::Result<String>> {
        async move {
            // Hook 不能反向触发 MCP server 启动或重连，否则启动阶段会被 hook 自己阻塞。
            let result = self
                .runtime
                .latest_call_tool(
                    &call.server,
                    &call.tool,
                    Some(Value::Object(call.input)),
                    Some(serde_json::json!({
                        "threadId": self.thread_id.to_string(),
                    })),
                    Some(call.timeout),
                    /*wait_for_server*/ false,
                )
                .await?;
            let text = result
                .content
                .iter()
                .filter_map(|content| {
                    (content.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| content.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if result.is_error == Some(true) {
                bail!("MCP tool returned an error: {text}");
            }

            Ok(text)
        }
        .boxed()
    }
}
