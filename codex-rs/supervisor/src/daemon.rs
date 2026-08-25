use crate::ActivityStatus;
use crate::PROTOCOL_VERSION;
use crate::ProcessMode;
use crate::ProcessRecord;
use crate::ProcessStatus;
use crate::SUPERVISOR_DIR_NAME;
use crate::SupervisorSnapshot;
use crate::TOKEN_FILE_NAME;
use crate::process::WorkerControl;
use crate::protocol::Envelope;
use crate::protocol::RegisterRequest;
use crate::protocol::Request;
use crate::protocol::Response;
use crate::protocol::WorkerEnvelope;
use crate::protocol::WorkerRequest;
use crate::protocol::WorkerResponse;
use crate::transport::Endpoint;
use crate::transport::connect_endpoint;
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
use tokio::sync::mpsc;
use tokio::time::sleep;

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

    fn snapshot(&self) -> SupervisorSnapshot {
        SupervisorSnapshot {
            protocol_version: PROTOCOL_VERSION,
            daemon_pid: self.daemon_pid,
            processes: self.records.values().cloned().collect(),
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
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        loop {
            sleep(std::time::Duration::from_secs(5)).await;
            cleanup_expired_leases(&cleanup_state).await;
        }
    });

    serve(endpoint, token, state).await
}

/// 为不依赖外部 runtime 的 binary 提供同步入口。
pub fn run_daemon_blocking() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
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

async fn cleanup_expired_leases(state: &SharedState) {
    let now = crate::unix_seconds();
    let expired: Vec<(Option<mpsc::Sender<()>>, u32)> = {
        let mut state = state.lock().await;
        let expired_ids: Vec<uuid::Uuid> = state
            .records
            .iter()
            .filter_map(|(id, record)| {
                (record.status == ProcessStatus::Running
                    && now.saturating_sub(record.last_heartbeat_at)
                        > crate::HEARTBEAT_TIMEOUT.as_secs() as i64)
                    .then_some(*id)
            })
            .collect();
        let mut expired = Vec::with_capacity(expired_ids.len());
        for id in expired_ids {
            let Some(pid) = state.records.get(&id).map(|record| record.pid) else {
                continue;
            };
            let kill_tx = state
                .workers
                .get(&id)
                .and_then(|worker| worker.kill_tx.clone());
            let Some(record) = state.records.get_mut(&id) else {
                continue;
            };
            record.status = ProcessStatus::Unresponsive;
            expired.push((kill_tx, pid));
        }
        expired
    };
    for (kill_tx, pid) in expired {
        if let Some(kill_tx) = kill_tx {
            let _ = kill_tx.send(()).await;
        } else {
            crate::process::terminate_pid(pid).await;
        }
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
        use tokio::net::windows::named_pipe::ServerOptions;
        let mut first_instance = true;
        loop {
            let mut options = ServerOptions::new();
            if first_instance {
                options.first_pipe_instance(true);
                first_instance = false;
            }
            let server = options
                .create(&name)
                .with_context(|| format!("failed to create supervisor named pipe {name}"))?;
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
        Request::Heartbeat {
            id,
            lease_token,
            now,
            status,
        } => {
            let mut state = state.lock().await;
            let Some(lease) = state.leases.get(&id) else {
                bail!("supervisor lease {id} no longer exists");
            };
            if lease.lease_token != lease_token {
                bail!("supervisor lease token mismatch for {id}");
            }
            let Some(record) = state.records.get_mut(&id) else {
                bail!("supervisor process {id} no longer exists");
            };
            record.last_heartbeat_at = now;
            record.activity = status.activity;
            record.mode = status.mode;
            record.summary = status.summary;
            record.error = status.error;
            record.last_state_update_at = status.updated_at.max(now);
            record.thread_id = status.thread_id;
            if record.status == ProcessStatus::Unresponsive {
                record.status = ProcessStatus::Running;
            }
            Ok(Response::Ack)
        }
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
        Request::List => {
            let state = state.lock().await;
            Ok(Response::Snapshot(state.snapshot()))
        }
        Request::ReadWork(request) => read_worker(request, &state).await,
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
                if let Some(record) = state.records.get_mut(&id) {
                    record.status = ProcessStatus::Stopping;
                }
                (kill_tx, pid)
            };
            if let Some(kill_tx) = kill_tx {
                kill_tx.send(()).await?;
            } else {
                // 直接注册的 CLI/TUI 没有由 daemon 持有 Child 句柄，使用其独立进程组回收。
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
    let pid = request.pid;
    let now = crate::unix_seconds();
    let monitor_state = state.clone();
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
    let monitor_direct_process = state
        .workers
        .get(&id)
        .is_none_or(|worker| worker.kill_tx.is_none());
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
            last_heartbeat_at: now,
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
    if monitor_direct_process {
        tokio::spawn(async move {
            monitor_registered_process(monitor_state, id, pid).await;
        });
    }
    Ok(Response::Registered { id, lease_token })
}

async fn read_worker(
    request: crate::protocol::WorkRequest,
    state: &SharedState,
) -> Result<Response> {
    let (endpoint, token) = {
        let state = state.lock().await;
        let Some(worker) = state.workers.get(&request.id) else {
            bail!(
                "supervisor process {} has no worker control channel",
                request.id
            );
        };
        if worker.endpoint.is_empty() || worker.token.is_empty() {
            bail!(
                "worker {} has not established its control channel",
                request.id
            );
        }
        (worker.endpoint.clone(), worker.token.clone())
    };
    let mut stream = connect_endpoint(&Endpoint::from_worker_address(&endpoint)).await?;
    write_frame(
        &mut stream,
        &WorkerEnvelope {
            token,
            request: WorkerRequest::ReadWork(request),
        },
    )
    .await?;
    let response = tokio::time::timeout(
        crate::REQUEST_TIMEOUT,
        crate::transport::read_frame::<_, WorkerResponse>(&mut stream),
    )
    .await
    .context("worker work query timed out")??;
    match response {
        WorkerResponse::WorkPage(page) => Ok(Response::WorkPage(page)),
        WorkerResponse::Error { message } => bail!("worker work query failed: {message}"),
    }
}

async fn monitor_registered_process(state: SharedState, id: uuid::Uuid, pid: u32) {
    loop {
        sleep(std::time::Duration::from_secs(1)).await;
        let process_is_registered = state.lock().await.records.contains_key(&id);
        if !process_is_registered {
            return;
        }
        if crate::process::is_process_alive(pid) {
            continue;
        }
        let mut state = state.lock().await;
        if let Some(record) = state.records.get_mut(&id) {
            record.status = ProcessStatus::Exited;
            record.last_heartbeat_at = crate::unix_seconds();
        }
        return;
    }
}
