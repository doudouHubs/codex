//! 本机 Codex worker 的唯一生命周期 owner。
//!
//! 这个 crate 只负责本机控制面：注册、租约、状态快照、启动和终止 worker。
//! app-server 的 thread 状态不在这里复制，调用方只通过 `thread_id` 做可选关联。

mod client;
mod daemon;
mod identity;
mod process;
mod protocol;
mod query;
mod reporter;
mod transport;
mod types;
mod worker;

pub use client::SupervisorClient;
pub use client::SupervisorLease;
pub use client::endpoint;
pub use client::ensure_supervisor;
pub use daemon::run_daemon;
pub use daemon::run_daemon_blocking;
pub use identity::WorkerIdentity;
pub use identity::filtered_process_args;
pub use identity::worker_identity;
pub use reporter::SupervisorReporter;
pub use reporter::append_current_plan_delta;
pub use reporter::current_reporter;
pub use reporter::record_current_message;
pub use reporter::record_current_plan;
pub use reporter::record_current_plan_text;
pub use reporter::record_current_prompt;
pub use reporter::record_current_tool_call;
pub use reporter::record_current_tool_call_with_id;
pub use reporter::report_current_activity;
pub use reporter::report_current_error;
pub use reporter::report_current_thread_id;
pub use types::ActivityStatus;
pub use types::PlanStep;
pub use types::ProcessKind;
pub use types::ProcessMode;
pub use types::ProcessRecord;
pub use types::ProcessStatus;
pub use types::SupervisorSnapshot;
pub use types::ToolCallRecord;
pub use types::WorkItem;
pub use types::WorkMessage;
pub use types::WorkPage;
pub use types::WorkSection;
pub use types::WorkerDetails;
pub use types::WorkerSpec;
pub use types::WorkerStatus;

use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const PROTOCOL_VERSION: u32 = 2;
const MAX_FRAME_SIZE: usize = 1024 * 1024;
const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const SUPERVISOR_WORKER_ARG: &str = "--codex-supervisor-worker";
const SUPERVISOR_DIR_NAME: &str = "supervisor";
const SOCKET_FILE_NAME: &str = "supervisor.sock";
const TOKEN_FILE_NAME: &str = "auth.token";

/// 隐藏的 daemon 启动参数。CLI 和独立 `codex-supervisor` binary 都复用同一个实现。
pub const DAEMON_ARG: &str = "--codex-supervisor-daemon";

pub(crate) fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

pub(crate) fn parent_pid() -> Option<u32> {
    #[cfg(unix)]
    {
        u32::try_from(std::process::id()).ok().and_then(|_| {
            let parent = unsafe { libc::getppid() };
            u32::try_from(parent).ok()
        })
    }
    #[cfg(windows)]
    {
        None
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
