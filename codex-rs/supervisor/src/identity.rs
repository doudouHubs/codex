use crate::SUPERVISOR_WORKER_ARG;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use std::ffi::OsString;
use uuid::Uuid;

/// supervisor 注入到 worker 命令行中的租约身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerIdentity {
    pub id: Uuid,
    pub lease_token: Uuid,
}

/// 解析 supervisor 注入的隐藏参数，判断当前进程是否是受管 worker。
///
/// 身份放在命令行而不是环境变量中，是为了避免 shell、工具进程和嵌套 Codex
/// 继承父进程租约后误注册到同一条记录。参数解析失败时直接报错，不能把损坏的
/// worker 降级成普通进程。
pub fn worker_identity() -> Result<Option<WorkerIdentity>> {
    parse_worker_args(std::env::args_os()).map(|(identity, _)| identity)
}

/// 返回剔除 supervisor 内部参数后的 argv，供 CLI/TUI 交给 clap 解析和 dashboard 展示。
pub fn filtered_process_args() -> Result<Vec<OsString>> {
    parse_worker_args(std::env::args_os()).map(|(_, args)| args)
}

pub(crate) fn parse_worker_args<I>(args: I) -> Result<(Option<WorkerIdentity>, Vec<OsString>)>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let mut filtered_args = Vec::with_capacity(args.len());
    let mut identity = None;
    let mut index = 0;
    while index < args.len() {
        if args[index].as_os_str() != SUPERVISOR_WORKER_ARG {
            filtered_args.push(args[index].clone());
            index += 1;
            continue;
        }

        if identity.is_some() {
            bail!("duplicate {SUPERVISOR_WORKER_ARG} argument");
        }
        let Some(id_value) = args.get(index + 1).and_then(|value| value.to_str()) else {
            bail!("{SUPERVISOR_WORKER_ARG} requires a process id and lease token");
        };
        let Some(lease_token_value) = args.get(index + 2).and_then(|value| value.to_str()) else {
            bail!("{SUPERVISOR_WORKER_ARG} requires a process id and lease token");
        };
        let id = Uuid::parse_str(id_value)
            .with_context(|| format!("invalid supervisor worker process id {id_value}"))?;
        let lease_token = Uuid::parse_str(lease_token_value).with_context(|| {
            format!("invalid supervisor worker lease token {lease_token_value}")
        })?;
        identity = Some(WorkerIdentity { id, lease_token });
        index += 3;
    }
    Ok((identity, filtered_args))
}
