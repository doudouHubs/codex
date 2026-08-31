use crate::ActivityStatus;
use crate::ProcessMode;
use crate::ProcessRecord;
use crate::ProcessStatus;
use crate::SUPERVISOR_DIR_NAME;
use crate::TOKEN_FILE_NAME;
use crate::process::WorkerControl;
use crate::protocol::Envelope;
use crate::protocol::RegisterRequest;
use crate::protocol::Request;
use crate::protocol::Response;
use crate::transport::Endpoint;
use crate::transport::write_frame;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_home_dir::find_codex_home;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

#[derive(Debug)]
pub(crate) struct LeaseEntry {
    pub(crate) lease_token: uuid::Uuid,
}

#[derive(Debug)]
pub(crate) struct SupervisorState {
    pub(crate) daemon_pid: u32,
    pub(crate) records: BTreeMap<uuid::Uuid, ProcessRecord>,
    pub(crate) leases: HashMap<uuid::Uuid, LeaseEntry>,
    pub(crate) workers: HashMap<uuid::Uuid, WorkerControl>,
}

impl SupervisorState {
    fn new() -> Self {
        Self {
            daemon_pid: std::process::id(),
            records: BTreeMap::new(),
            leases: HashMap::new(),
            workers: HashMap::new(),
        }
    }
}

pub(crate) type SharedState = Arc<Mutex<SupervisorState>>;

/// 运行 daemon。该函数不会返回，除非 listener 或 token 存储出现致命错误。
pub async fn run_daemon() -> Result<()> {
    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
    let supervisor_dir = codex_home.join(SUPERVISOR_DIR_NAME);
    tokio::fs::create_dir_all(&supervisor_dir)
        .await
        .with_context(|| {
            format!(
                "failed to create supervisor directory {}",
                supervisor_dir.display()
            )
        })?;
    let token = load_or_create_token(&supervisor_dir).await?;
    let endpoint = Endpoint::from_codex_home(codex_home.as_path());
    let state = Arc::new(Mutex::new(SupervisorState::new()));

    serve(endpoint, token, state).await
}

/// 为不依赖外部 runtime 的 binary 提供同步入口。
pub fn run_daemon_blocking() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build supervisor runtime")?
        .block_on(run_daemon())
}

async fn load_or_create_token(supervisor_dir: &Path) -> Result<String> {
    let token_path = supervisor_dir.join(TOKEN_FILE_NAME);
    if let Ok(token) = tokio::fs::read_to_string(&token_path).await {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = uuid::Uuid::new_v4().to_string();
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        options.mode(0o600);
    }

    match options.open(&token_path).await {
        Ok(mut file) => {
            // `create_new` 让并发首次启动只允许一个 daemon 成为 token owner，避免
            // 后启动的 daemon 覆盖已绑定 daemon 正在使用的认证 token。
            file.write_all(token.as_bytes()).await?;
            file.sync_all().await?;
            Ok(token)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // 文件可能刚由并发 daemon 创建；读取现有 token，绝不覆盖它。空文件属于
            // 初始化损坏，直接失败比生成第二份 token 更安全，调用方会显式暴露故障。
            let existing = tokio::fs::read_to_string(&token_path)
                .await
                .with_context(|| {
                    format!("failed to read supervisor token {}", token_path.display())
                })?;
            let existing = existing.trim().to_string();
            if existing.is_empty() {
                bail!("codex supervisor token {} is empty", token_path.display());
            }
            Ok(existing)
        }
        Err(error) => Err(error)
            .with_context(|| format!("failed to create supervisor token {}", token_path.display())),
    }
}

async fn serve(endpoint: Endpoint, token: String, state: SharedState) -> Result<()> {
    #[cfg(unix)]
    {
        let Endpoint::Unix(path) = endpoint else {
            unreachable!();
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if path.exists() && tokio::net::UnixStream::connect(&path).await.is_ok() {
            bail!("another codex supervisor is already running");
        }
        if path.exists() {
            let _ = tokio::fs::remove_file(&path).await;
        }
        let listener = tokio::net::UnixListener::bind(&path)
            .with_context(|| format!("failed to bind supervisor socket {}", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let state = state.clone();
            let token = token.clone();
            tokio::spawn(async move {
                if let Err(error) = handle_connection(stream, token, state).await {
                    eprintln!("codex supervisor connection failed: {error}");
                }
            });
        }
    }

    #[cfg(windows)]
    {
        let Endpoint::Windows(name) = endpoint;
        let mut first_instance = true;
        loop {
            let server = crate::transport::create_named_pipe_server(&name, first_instance)
                .with_context(|| format!("failed to create supervisor named pipe {name}"))?;
            first_instance = false;
            server.connect().await?;
            let state = state.clone();
            let token = token.clone();
            tokio::spawn(async move {
                if let Err(error) = handle_connection(server, token, state).await {
                    eprintln!("codex supervisor connection failed: {error}");
                }
            });
        }
    }
}

async fn handle_connection<S>(mut stream: S, token: String, state: SharedState) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let envelope: Envelope = crate::transport::read_frame(&mut stream).await?;
    if envelope.token != token {
        write_frame(
            &mut stream,
            &Response::Error {
                message: "invalid supervisor token".to_string(),
            },
        )
        .await?;
        return Ok(());
    }
    let response = handle_request(envelope.request, state).await;
    write_frame(&mut stream, &response).await
}

async fn handle_request(request: Request, state: SharedState) -> Response {
    match handle_request_inner(request, state).await {
        Ok(response) => response,
        Err(error) => Response::Error {
            message: format!("{error:#}"),
        },
    }
}

async fn handle_request_inner(request: Request, state: SharedState) -> Result<Response> {
    match request {
        Request::Ping => Ok(Response::Pong),
        Request::Register(request) => register_process(request, &state).await,
        Request::Unregister { id, lease_token } => {
            let mut state = state.lock().await;
            let Some(lease) = state.leases.get(&id) else {
                bail!("supervisor lease {id} no longer exists");
            };
            if lease.lease_token != lease_token {
                bail!("supervisor lease token mismatch for {id}");
            }
            state.leases.remove(&id);
            state.records.remove(&id);
            state.workers.remove(&id);
            Ok(Response::Ack)
        }
        Request::List => Ok(Response::Snapshot(crate::query::snapshot(&state).await?)),
        Request::ReadWork(request) => crate::query::read_work(request, &state).await,
        Request::SpawnWorker(request) => crate::process::spawn_worker(request, state).await,
        Request::Terminate { id } => {
            let (kill_tx, pid) = {
                let mut state = state.lock().await;
                let Some(pid) = state.records.get(&id).map(|record| record.pid) else {
                    bail!("supervisor process {id} not found");
                };
                let kill_tx = state
                    .workers
                    .get(&id)
                    .and_then(|worker| worker.kill_tx.clone());
                if kill_tx.is_some()
                    && let Some(record) = state.records.get_mut(&id)
                {
                    record.status = ProcessStatus::Stopping;
                }
                (kill_tx, pid)
            };
            if let Some(kill_tx) = kill_tx {
                kill_tx.send(()).await?;
            } else {
                // 直接注册的 CLI/TUI 没有由 daemon 持有 Child 句柄，使用其独立进程组回收。
                if !crate::process::is_process_alive(pid) {
                    {
                        let mut state = state.lock().await;
                        let Some(record) = state.records.get_mut(&id) else {
                            bail!("supervisor process {id} disappeared during termination");
                        };
                        if record.pid != pid {
                            bail!("supervisor process {id} changed during termination");
                        }
                        record.status = ProcessStatus::Exited;
                        record.last_observed_at = crate::unix_seconds();
                    }
                    return Ok(Response::Ack);
                }
                // 先验证 worker endpoint，避免记录中的 PID 已被操作系统复用时误杀别的进程。
                crate::query::verify_worker_control(id, &state).await?;
                {
                    let mut state = state.lock().await;
                    let Some(record) = state.records.get_mut(&id) else {
                        bail!("supervisor process {id} disappeared during termination");
                    };
                    if record.pid != pid {
                        bail!("supervisor process {id} changed during termination");
                    }
                    // 只有控制端点验证成功后才进入 Stopping，校验失败不会留下无法重试的
                    // 假状态；这也是直接注册进程与 daemon 子进程的生命周期差异。
                    record.status = ProcessStatus::Stopping;
                }
                crate::process::terminate_pid(pid).await;
            }
            Ok(Response::Ack)
        }
    }
}

async fn register_process(request: RegisterRequest, state: &SharedState) -> Result<Response> {
    if request.pid == 0
        || request.executable.as_os_str().is_empty()
        || request.worker_endpoint.is_empty()
        || request.worker_token.is_empty()
    {
        bail!("invalid process identity");
    }
    let id = request.id.unwrap_or_else(uuid::Uuid::new_v4);
    let lease_token = request.lease_token.unwrap_or_else(uuid::Uuid::new_v4);
    let now = crate::unix_seconds();
    let mut state = state.lock().await;
    if let Some(existing) = state.leases.get(&id)
        && existing.lease_token != lease_token
    {
        bail!("process id {id} is already owned by another lease");
    }
    if request.id.is_none()
        && state
            .records
            .values()
            .any(|record| record.pid == request.pid && record.status == ProcessStatus::Running)
    {
        bail!("process {} is already registered", request.pid);
    }
    let existing_created_at = state.records.get(&id).map(|record| record.created_at);
    let status = if state.workers.contains_key(&id) {
        ProcessStatus::Running
    } else {
        state
            .records
            .get(&id)
            .map_or(ProcessStatus::Running, |record| record.status)
    };
    if let Some(worker) = state.workers.get(&id)
        && !worker.endpoint.is_empty()
        && (worker.endpoint != request.worker_endpoint || worker.token != request.worker_token)
    {
        bail!("worker control endpoint for process {id} is already owned");
    }
    state.records.insert(
        id,
        ProcessRecord {
            id,
            pid: request.pid,
            parent_pid: request.parent_pid,
            kind: request.kind,
            status,
            activity: ActivityStatus::Idle,
            mode: ProcessMode::Default,
            summary: None,
            error: None,
            executable: request.executable,
            argv: request.argv,
            cwd: request.cwd,
            thread_id: request.thread_id,
            created_at: existing_created_at.unwrap_or(now),
            last_observed_at: now,
            last_state_update_at: now,
            exit_code: None,
        },
    );
    let worker = state.workers.entry(id).or_insert_with(|| WorkerControl {
        kill_tx: None,
        endpoint: String::new(),
        token: String::new(),
    });
    worker.endpoint = request.worker_endpoint;
    worker.token = request.worker_token;
    state
        .leases
        .insert(id, crate::daemon::LeaseEntry { lease_token });
    drop(state);
    Ok(Response::Registered {
        id,
        lease_token,
        protocol_version: Some(crate::PROTOCOL_VERSION),
    })
}
