use crate::SUPERVISOR_WORKER_ARG;
use crate::daemon::SharedState;
use crate::protocol::Response;
use crate::protocol::SpawnWorkerRequest;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use std::process::Stdio;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug)]
pub(crate) struct WorkerControl {
    pub(crate) kill_tx: Option<mpsc::Sender<()>>,
    pub(crate) endpoint: String,
    pub(crate) token: String,
}

/// 让直接注册的 CLI/TUI 成为独立进程组，supervisor 才能在 dashboard 中回收其子进程。
pub(crate) fn prepare_current_process() {
    #[cfg(unix)]
    {
        // 非交互 CLI 没有终端 job-control，可以独立成组让 supervisor 回收工具子树。
        // 交互 TUI 必须留在终端前台组；强行 setpgid 会让 Ctrl-C 只打到 shell，TUI
        // 反而收不到中断。由 supervisor 启动的 worker 仍在 pre_exec 中无条件独立成组。
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 1
                || libc::isatty(libc::STDOUT_FILENO) == 1
                || libc::isatty(libc::STDERR_FILENO) == 1
            {
                return;
            }
            let _ = libc::setpgid(0, 0);
        }
    }
}

pub(crate) fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Foundation::STILL_ACTIVE;
        use windows_sys::Win32::System::Threading::GetExitCodeProcess;
        use windows_sys::Win32::System::Threading::OpenProcess;
        use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process == 0 {
            return false;
        }
        let mut exit_code = 0;
        let result = unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0;
        unsafe {
            CloseHandle(process);
        }
        result && exit_code == STILL_ACTIVE as u32
    }
}

pub(crate) async fn spawn_worker(
    request: SpawnWorkerRequest,
    state: SharedState,
) -> Result<Response> {
    if !request.executable.is_file() {
        bail!(
            "worker executable does not exist: {}",
            request.executable.display()
        );
    }
    if !request.cwd.is_dir() {
        bail!(
            "worker working directory does not exist: {}",
            request.cwd.display()
        );
    }
    let id = Uuid::new_v4();
    let lease_token = Uuid::new_v4();
    let mut command = Command::new(&request.executable);
    command
        // 这个参数既标识 worker 身份，又把预注册租约传给 worker；环境变量会被
        // worker 启动的 shell/tool 继承，可能让嵌套 Codex 错误复用父进程租约。
        .arg(SUPERVISOR_WORKER_ARG)
        .arg(id.to_string())
        .arg(lease_token.to_string())
        .args(&request.args)
        .current_dir(&request.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    configure_worker_process(&mut command);
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to spawn Codex worker using {}",
            request.executable.display()
        )
    })?;
    let pid = child.id().context("spawned Codex worker has no pid")?;
    let (kill_tx, kill_rx) = mpsc::channel(1);
    let now = crate::unix_seconds();
    {
        let mut state = state.lock().await;
        state.records.insert(
            id,
            crate::ProcessRecord {
                id,
                pid,
                parent_pid: Some(std::process::id()),
                kind: request.kind,
                status: crate::ProcessStatus::Starting,
                activity: crate::ActivityStatus::Idle,
                mode: crate::ProcessMode::Default,
                summary: None,
                error: None,
                executable: request.executable,
                argv: request.args,
                cwd: request.cwd,
                thread_id: request.thread_id,
                created_at: now,
                last_observed_at: now,
                last_state_update_at: now,
                exit_code: None,
            },
        );
        state
            .leases
            .insert(id, crate::daemon::LeaseEntry { lease_token });
        state.workers.insert(
            id,
            WorkerControl {
                kill_tx: Some(kill_tx),
                endpoint: String::new(),
                token: String::new(),
            },
        );
    }
    let monitor_state = state.clone();
    tokio::spawn(async move {
        let exit_code = monitor_worker(&mut child, pid, kill_rx).await;
        let mut state = monitor_state.lock().await;
        state.workers.remove(&id);
        if let Some(record) = state.records.get_mut(&id) {
            record.status = crate::ProcessStatus::Exited;
            record.exit_code = exit_code;
            record.last_observed_at = crate::unix_seconds();
        }
    });
    Ok(Response::Spawned { id, pid })
}

async fn monitor_worker(
    child: &mut Child,
    pid: u32,
    mut kill_rx: mpsc::Receiver<()>,
) -> Option<i32> {
    tokio::select! {
        result = child.wait() => result.ok().and_then(|status| status.code()),
        _ = kill_rx.recv() => {
            terminate_child(child, pid).await;
            child.wait().await.ok().and_then(|status| status.code())
        }
    }
}

pub(crate) async fn terminate_child(child: &mut Child, pid: u32) {
    terminate_pid(pid).await;
    let _ = child.start_kill();
}

pub(crate) async fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    {
        if let Ok(pid) = libc::pid_t::try_from(pid) {
            // 只有确认目标 PID 自己是进程组组长时才能 kill 负 PID；否则会误杀终端
            // 所在的共享进程组。交互 TUI 留在前台组，因此这里只终止目标进程本身。
            let has_private_process_group = unsafe { libc::getpgid(pid) == pid };
            let target = if has_private_process_group { -pid } else { pid };
            unsafe {
                libc::kill(target, libc::SIGTERM);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // 工具进程可能忽略 SIGTERM；宽限期后再杀同一目标，避免 dashboard 显示已
            // 终止但子进程仍占资源，同时不扩大到终端的共享进程组。
            unsafe {
                libc::kill(target, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .await;
    }
}

fn configure_worker_process(_command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            _command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}
