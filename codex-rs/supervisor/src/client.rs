use crate::DAEMON_ARG;
use crate::REQUEST_TIMEOUT;
use crate::START_TIMEOUT;
use crate::SUPERVISOR_DIR_NAME;
use crate::SupervisorReporter;
use crate::TOKEN_FILE_NAME;
use crate::WorkerSpec;
use crate::identity::parse_worker_args;
use crate::protocol::Envelope;
use crate::protocol::RegisterRequest;
use crate::protocol::Request;
use crate::protocol::Response;
use crate::protocol::SpawnWorkerRequest;
use crate::transport::Endpoint;
use crate::transport::connect_endpoint;
use crate::transport::endpoint_for_codex_home;
use crate::transport::read_frame;
use crate::transport::worker_endpoint_for_codex_home;
use crate::transport::write_frame;
use crate::worker::WorkerControlServer;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_home_dir::find_codex_home;
use std::path::Path;
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::Stdio;
#[cfg(not(windows))]
use tokio::process::Command;
use tokio::time::sleep;
use tokio::time::timeout;
use uuid::Uuid;

/// 一个已登记的进程租约。
pub struct SupervisorLease {
    client: SupervisorClient,
    id: Uuid,
    lease_token: Uuid,
    control_server: Option<WorkerControlServer>,
    reporter: SupervisorReporter,
}

impl std::fmt::Debug for SupervisorLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SupervisorLease")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl SupervisorLease {
    /// 返回当前进程在 supervisor 中的稳定 ID，供 app-server thread 做可选关联。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 返回当前 worker 的状态和内容上报句柄。核心 Agent、TUI 和 exec 事件处理器共享
    /// 这个句柄，避免各入口各自维护一份 dashboard 状态。
    pub fn reporter(&self) -> SupervisorReporter {
        self.reporter.clone()
    }

    /// 主动撤销租约。正常退出应调用此方法，异常退出则由下一次按需查询识别。
    pub async fn close(&mut self) -> Result<()> {
        let result = self.client.unregister(self.id, self.lease_token).await;
        if let Some(control_server) = self.control_server.take() {
            control_server.close().await;
        }
        result
    }
}

impl Drop for SupervisorLease {
    fn drop(&mut self) {
        if let Some(a) = self.control_server.take() {
            drop(a)
        }
    }
}

/// 给 CLI/TUI 使用的 supervisor 客户端。
#[derive(Debug, Clone)]
pub struct SupervisorClient {
    endpoint: Endpoint,
    token: String,
    codex_home: PathBuf,
}

/// 返回当前机器上 supervisor 使用的本地 endpoint，便于诊断和测试。
pub async fn endpoint() -> Result<PathBuf> {
    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
    Ok(endpoint_for_codex_home(codex_home.as_path()))
}

/// 让当前 CLI 确保 supervisor 存在；首次启动没有 daemon 时会启动 daemon 并等待 ready。
pub async fn ensure_supervisor(daemon_executable: &Path) -> Result<SupervisorClient> {
    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
    tokio::fs::create_dir_all(codex_home.as_path())
        .await
        .with_context(|| format!("failed to create CODEX_HOME {}", codex_home.display()))?;

    if let Ok(client) = SupervisorClient::connect_at(codex_home.as_path()).await
        && timeout(REQUEST_TIMEOUT, client.ping())
            .await
            .is_ok_and(|result| result.is_ok())
    {
        return Ok(client);
    }

    spawn_daemon(daemon_executable)?;

    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(client) = SupervisorClient::connect_at(codex_home.as_path()).await
            && timeout(REQUEST_TIMEOUT, client.ping())
                .await
                .is_ok_and(|result| result.is_ok())
        {
            return Ok(client);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "codex supervisor did not become ready within {} seconds",
                START_TIMEOUT.as_secs()
            );
        }
        sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(not(windows))]
fn spawn_daemon(daemon_executable: &Path) -> Result<()> {
    let mut command = Command::new(daemon_executable);
    command
        .arg(DAEMON_ARG)
        // supervisor 只负责 IPC 和进程控制，不应持有调用方的管道句柄；否则非交互
        // `codex` 的 stdout/stderr 即使 worker 已退出，也会因为 daemon 持有句柄而无法 EOF。
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().with_context(|| {
        format!(
            "failed to start codex supervisor {}",
            daemon_executable.display()
        )
    })?;
    Ok(())
}

#[cfg(windows)]
fn spawn_daemon(daemon_executable: &Path) -> Result<()> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::CreateProcessW;
    use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
    use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
    use windows_sys::Win32::System::Threading::STARTUPINFOW;

    let mut command_line = quote_windows_arg(daemon_executable.as_os_str());
    command_line.extend(format!(" {DAEMON_ARG}").encode_utf16());
    command_line.push(0);
    let startup_info = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // 不继承任何句柄是 Windows 非交互 CLI 能可靠收到 stdout/stderr EOF 的关键。
    // 仅设置 Stdio::null() 不够，因为父进程的捕获管道仍可能作为可继承句柄泄漏进来。
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS,
            std::ptr::null(),
            std::ptr::null(),
            &startup_info,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to start codex supervisor {}",
                daemon_executable.display()
            )
        });
    }
    unsafe {
        CloseHandle(process_info.hThread);
        CloseHandle(process_info.hProcess);
    }
    Ok(())
}

#[cfg(windows)]
fn quote_windows_arg(argument: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    let mut quoted = Vec::new();
    quoted.push('"' as u16);
    let mut backslashes = 0;
    for unit in argument.encode_wide() {
        if unit == '\\' as u16 {
            backslashes += 1;
        } else {
            quoted.extend(std::iter::repeat_n('\\' as u16, backslashes));
            backslashes = 0;
            if unit == '"' as u16 {
                quoted.push('\\' as u16);
            }
            quoted.push(unit);
        }
    }
    quoted.extend(std::iter::repeat_n('\\' as u16, backslashes * 2));
    quoted.push('"' as u16);
    quoted
}

impl SupervisorClient {
    /// 连接现有 supervisor，不负责拉起新 daemon。
    pub async fn connect() -> Result<Self> {
        let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
        Self::connect_at(codex_home.as_path()).await
    }

    async fn connect_at(codex_home: &Path) -> Result<Self> {
        let token_path = codex_home.join(SUPERVISOR_DIR_NAME).join(TOKEN_FILE_NAME);
        let token = tokio::fs::read_to_string(&token_path)
            .await
            .with_context(|| format!("failed to read supervisor token {}", token_path.display()))?
            .trim()
            .to_string();
        if token.is_empty() {
            bail!("codex supervisor token is empty");
        }
        Ok(Self {
            endpoint: Endpoint::from_codex_home(codex_home),
            token,
            codex_home: codex_home.to_path_buf(),
        })
    }

    /// 注册当前进程并启动 worker 控制服务。
    pub async fn register_current_process(
        &self,
        kind: crate::ProcessKind,
        thread_id: Option<String>,
    ) -> Result<SupervisorLease> {
        crate::process::prepare_current_process();
        let (worker_identity, argv) = parse_worker_args(std::env::args_os())?;
        let id = worker_identity.map_or_else(Uuid::new_v4, |identity| identity.id);
        let lease_token =
            worker_identity.map_or_else(Uuid::new_v4, |identity| identity.lease_token);
        let worker_token = Uuid::new_v4().to_string();
        let worker_endpoint = worker_endpoint_for_codex_home(&self.codex_home, id);
        let reporter = SupervisorReporter::new();
        let control_server = WorkerControlServer::start(
            worker_endpoint.clone(),
            worker_token.clone(),
            reporter.clone(),
        )
        .await?;
        let response = match self
            .call(Request::Register(RegisterRequest {
                id: Some(id),
                lease_token: Some(lease_token),
                pid: std::process::id(),
                parent_pid: crate::parent_pid(),
                kind,
                executable: std::env::current_exe()
                    .context("failed to resolve current executable")?,
                argv: argv
                    .into_iter()
                    .skip(1)
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
                cwd: std::env::current_dir()
                    .context("failed to resolve current working directory")?,
                thread_id,
                worker_endpoint,
                worker_token,
            }))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                control_server.close().await;
                return Err(error);
            }
        };
        let Response::Registered {
            id,
            lease_token,
            protocol_version,
        } = response
        else {
            control_server.close().await;
            bail!("supervisor returned an invalid register response");
        };
        if protocol_version != Some(crate::PROTOCOL_VERSION) {
            // 新版 worker 不能连接旧版 supervisor：旧 daemon 仍依赖已删除的租约维护机制，
            // 最终可能把一个正常 worker 误判为失联并终止。注册成功后立即注销，
            // 将版本漂移转换为可读的启动错误，而不是延迟到运行中崩溃。
            let _ = self.unregister(id, lease_token).await;
            control_server.close().await;
            bail!(
                "incompatible codex supervisor protocol: expected {}, got {protocol_version:?}",
                crate::PROTOCOL_VERSION
            );
        }
        crate::reporter::install_current_reporter(reporter.clone());
        Ok(SupervisorLease {
            client: self.clone(),
            id,
            lease_token,
            control_server: Some(control_server),
            reporter: reporter.clone(),
        })
    }

    /// 启动一个继承当前终端句柄的 worker。
    pub async fn spawn_worker(&self, spec: WorkerSpec) -> Result<Uuid> {
        let args = spec
            .args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let response = self
            .call(Request::SpawnWorker(SpawnWorkerRequest {
                executable: spec.executable,
                args,
                cwd: spec.cwd,
                kind: spec.kind,
                thread_id: spec.thread_id,
            }))
            .await?;
        let Response::Spawned { id, .. } = response else {
            bail!("supervisor returned an invalid spawn response");
        };
        Ok(id)
    }

    /// 获取所有已知 Codex worker 的状态快照。
    pub async fn snapshot(&self) -> Result<crate::SupervisorSnapshot> {
        let response = self.call(Request::List).await?;
        let Response::Snapshot(snapshot) = response else {
            bail!("supervisor returned an invalid list response");
        };
        if snapshot.protocol_version != crate::PROTOCOL_VERSION {
            bail!(
                "incompatible codex supervisor protocol: expected {}, got {}",
                crate::PROTOCOL_VERSION,
                snapshot.protocol_version
            );
        }
        Ok(snapshot)
    }

    /// 从指定 worker 读取一页受控工作内容。worker 失联时返回错误，调用方不得把它
    /// 降级成空内容或“未托管”状态。
    pub async fn work_page(
        &self,
        id: Uuid,
        section: crate::WorkSection,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<crate::WorkPage> {
        let response = self
            .call(Request::ReadWork(crate::protocol::WorkRequest {
                id,
                section,
                cursor,
                limit,
            }))
            .await?;
        let Response::WorkPage(page) = response else {
            bail!("supervisor returned an invalid work page response");
        };
        Ok(page)
    }

    /// 终止指定 worker，终止动作始终由 supervisor 执行。
    pub async fn terminate(&self, id: Uuid) -> Result<()> {
        let response = self.call(Request::Terminate { id }).await?;
        if !matches!(response, Response::Ack) {
            bail!("supervisor returned an invalid terminate response");
        }
        Ok(())
    }

    async fn ping(&self) -> Result<()> {
        let response = self.call(Request::Ping).await?;
        if !matches!(response, Response::Pong) {
            bail!("supervisor returned an invalid ping response");
        }
        Ok(())
    }

    async fn unregister(&self, id: Uuid, lease_token: Uuid) -> Result<()> {
        let response = self.call(Request::Unregister { id, lease_token }).await?;
        if !matches!(response, Response::Ack) {
            bail!("supervisor returned an invalid unregister response");
        }
        Ok(())
    }

    async fn call(&self, request: Request) -> Result<Response> {
        let mut stream = connect_endpoint(&self.endpoint).await?;
        write_frame(
            &mut stream,
            &Envelope {
                token: self.token.clone(),
                request,
            },
        )
        .await?;
        let response = timeout(REQUEST_TIMEOUT, read_frame(&mut stream))
            .await
            .context("supervisor request timed out")??;
        if let Response::Error { message } = response {
            bail!("{message}");
        }
        Ok(response)
    }
}
