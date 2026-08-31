use crate::ProcessRecord;
use crate::ProcessStatus;
use crate::SupervisorSnapshot;
use crate::WorkerStatus;
use crate::daemon::SharedState;
use crate::daemon::SupervisorState;
use crate::process::WorkerControl;
use crate::protocol::Response;
use crate::protocol::WorkRequest;
use crate::protocol::WorkerEnvelope;
use crate::protocol::WorkerRequest;
use crate::protocol::WorkerResponse;
use crate::transport::Endpoint;
use crate::transport::connect_endpoint;
use crate::transport::read_frame;
use crate::transport::write_frame;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::timeout;
use uuid::Uuid;

const MAX_CONCURRENT_STATUS_QUERIES: usize = 8;
const SNAPSHOT_QUERY_TIMEOUT: Duration = Duration::from_secs(4);
const WORKER_STATUS_QUERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkerEndpoint {
    address: String,
    token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ControlTarget {
    Missing,
    Pending,
    Endpoint(WorkerEndpoint),
}

#[derive(Clone, Debug)]
struct ProcessQueryTarget {
    id: Uuid,
    pid: u32,
    control: ControlTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkerObservation {
    Exited,
    Starting,
    Status(WorkerStatus),
    Unresponsive(String),
}

/// 查询所有已登记进程，并在返回快照前把本次观察结果写回注册表。
///
/// 查询采用有限并发和总时限：一个失联 worker 不能拖住所有结果，也不能让请求
/// 创建无界数量的连接任务。结果回写前会再次核对进程身份，避免慢查询覆盖重注册记录。
pub(crate) async fn snapshot(state: &SharedState) -> Result<SupervisorSnapshot> {
    let targets = {
        let state = state.lock().await;
        state
            .records
            .iter()
            .map(|(id, record)| process_query_target(*id, record, state.workers.get(id)))
            .collect::<Vec<_>>()
    };
    let (mut observations, timed_out) = observe_targets(targets.clone()).await?;
    if timed_out {
        // 总时限只代表本次查询预算耗尽；仍然返回已有结果，并把未完成的 worker
        // 明确标成失联，避免把上一轮的 Running 误当成当前事实。
        for target in &targets {
            observations.entry(target.id).or_insert_with(|| {
                WorkerObservation::Unresponsive("snapshot query timed out".to_string())
            });
        }
    }

    let observed_at = crate::unix_seconds();
    let mut state_guard = state.lock().await;
    for target in &targets {
        let Some(observation) = observations.remove(&target.id) else {
            continue;
        };
        if !target_is_current(&state_guard, target) {
            continue;
        }
        if let Some(record) = state_guard.records.get_mut(&target.id) {
            apply_observation(record, observation, observed_at);
        }
    }

    Ok(SupervisorSnapshot {
        protocol_version: crate::PROTOCOL_VERSION,
        daemon_pid: state_guard.daemon_pid,
        processes: state_guard.records.values().cloned().collect(),
    })
}

/// 读取指定 worker 的一页工作内容。工作内容始终由 worker 自己持有，Supervisor 只
/// 在收到请求时转发分页请求，不在后台复制或主动读取会话数据。
pub(crate) async fn read_work(request: WorkRequest, state: &SharedState) -> Result<Response> {
    let process_id = request.id;
    let target = target_for_process(process_id, state).await?;
    if !crate::process::is_process_alive(target.pid) {
        apply_target_observation(state, &target, WorkerObservation::Exited).await;
        bail!("supervisor process {process_id} has exited");
    }
    let ControlTarget::Endpoint(endpoint) = target.control else {
        bail!("worker {process_id} has not established its control channel");
    };
    let response = read_worker_response(
        &endpoint,
        WorkerRequest::ReadWork(request),
        crate::REQUEST_TIMEOUT,
    )
    .await?;
    match response {
        WorkerResponse::WorkPage(page) => Ok(Response::WorkPage(page)),
        WorkerResponse::Error { message } => bail!("worker work query failed: {message}"),
        WorkerResponse::Status(_) => bail!("worker returned an invalid work query response"),
    }
}

/// 终止直接注册进程前验证控制端点仍属于目标 worker，降低 PID 复用导致误杀的风险。
pub(crate) async fn verify_worker_control(id: Uuid, state: &SharedState) -> Result<()> {
    let target = target_for_process(id, state).await?;
    if !crate::process::is_process_alive(target.pid) {
        apply_target_observation(state, &target, WorkerObservation::Exited).await;
        bail!("supervisor process {id} has exited");
    }
    let ControlTarget::Endpoint(endpoint) = target.control else {
        bail!("worker {id} has not established its control channel");
    };
    match read_worker_response(&endpoint, WorkerRequest::ReadStatus, crate::REQUEST_TIMEOUT).await?
    {
        WorkerResponse::Status(_) => Ok(()),
        WorkerResponse::Error { message } => {
            bail!("worker control verification failed: {message}")
        }
        WorkerResponse::WorkPage(_) => {
            bail!("worker returned an invalid control verification response")
        }
    }
}

async fn observe_targets(
    targets: Vec<ProcessQueryTarget>,
) -> Result<(BTreeMap<Uuid, WorkerObservation>, bool)> {
    let mut pending = targets.into_iter();
    let mut tasks = JoinSet::new();
    for _ in 0..MAX_CONCURRENT_STATUS_QUERIES {
        let Some(target) = pending.next() else {
            break;
        };
        tasks.spawn(observe_process(target));
    }

    let mut observations = BTreeMap::new();
    let collection = timeout(SNAPSHOT_QUERY_TIMEOUT, async {
        while let Some(result) = tasks.join_next().await {
            let (id, observation) = result.context("status query task failed")?;
            observations.insert(id, observation);
            if let Some(target) = pending.next() {
                tasks.spawn(observe_process(target));
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await;

    match collection {
        Ok(result) => result?,
        Err(_) => {
            tasks.abort_all();
            return Ok((observations, true));
        }
    }
    Ok((observations, false))
}

async fn observe_process(target: ProcessQueryTarget) -> (Uuid, WorkerObservation) {
    let observation = if !crate::process::is_process_alive(target.pid) {
        WorkerObservation::Exited
    } else {
        match target.control {
            ControlTarget::Missing => {
                WorkerObservation::Unresponsive("worker control channel is missing".to_string())
            }
            ControlTarget::Pending => WorkerObservation::Starting,
            ControlTarget::Endpoint(endpoint) => match timeout(
                WORKER_STATUS_QUERY_TIMEOUT,
                read_worker_response(&endpoint, WorkerRequest::ReadStatus, crate::REQUEST_TIMEOUT),
            )
            .await
            {
                Ok(Ok(WorkerResponse::Status(status))) => WorkerObservation::Status(status),
                Ok(Ok(WorkerResponse::Error { message })) => WorkerObservation::Unresponsive(
                    format!("worker status query failed: {message}"),
                ),
                Ok(Ok(WorkerResponse::WorkPage(_))) => WorkerObservation::Unresponsive(
                    "worker returned an invalid status query response".to_string(),
                ),
                Ok(Err(error)) => WorkerObservation::Unresponsive(format!(
                    "worker status query failed: {error:#}"
                )),
                Err(_) => WorkerObservation::Unresponsive(format!(
                    "worker status query timed out after {} seconds",
                    WORKER_STATUS_QUERY_TIMEOUT.as_secs()
                )),
            },
        }
    };
    (target.id, observation)
}

async fn read_worker_response(
    endpoint: &WorkerEndpoint,
    request: WorkerRequest,
    request_timeout: Duration,
) -> Result<WorkerResponse> {
    let mut stream = connect_endpoint(&Endpoint::from_worker_address(&endpoint.address)).await?;
    write_frame(
        &mut stream,
        &WorkerEnvelope {
            token: endpoint.token.clone(),
            request,
        },
    )
    .await?;
    timeout(request_timeout, read_frame(&mut stream))
        .await
        .context("worker request timed out")?
        .context("failed to read worker response")
}

async fn target_for_process(id: Uuid, state: &SharedState) -> Result<ProcessQueryTarget> {
    let state = state.lock().await;
    let Some(record) = state.records.get(&id) else {
        bail!("supervisor process {id} not found");
    };
    Ok(process_query_target(id, record, state.workers.get(&id)))
}

fn process_query_target(
    id: Uuid,
    record: &ProcessRecord,
    worker: Option<&WorkerControl>,
) -> ProcessQueryTarget {
    ProcessQueryTarget {
        id,
        pid: record.pid,
        control: control_target(worker),
    }
}

fn control_target(worker: Option<&WorkerControl>) -> ControlTarget {
    let Some(worker) = worker else {
        return ControlTarget::Missing;
    };
    if worker.endpoint.is_empty() || worker.token.is_empty() {
        return ControlTarget::Pending;
    }
    ControlTarget::Endpoint(WorkerEndpoint {
        address: worker.endpoint.clone(),
        token: worker.token.clone(),
    })
}

fn target_is_current(state: &SupervisorState, target: &ProcessQueryTarget) -> bool {
    state
        .records
        .get(&target.id)
        .is_some_and(|record| record.pid == target.pid)
        && control_target(state.workers.get(&target.id)) == target.control
}

async fn apply_target_observation(
    state: &SharedState,
    target: &ProcessQueryTarget,
    observation: WorkerObservation,
) {
    let mut state = state.lock().await;
    if target_is_current(&state, target)
        && let Some(record) = state.records.get_mut(&target.id)
    {
        apply_observation(record, observation, crate::unix_seconds());
    }
}

fn apply_observation(record: &mut ProcessRecord, observation: WorkerObservation, observed_at: i64) {
    record.last_observed_at = observed_at;
    match observation {
        WorkerObservation::Exited => {
            record.status = ProcessStatus::Exited;
        }
        WorkerObservation::Starting => {
            if !matches!(
                record.status,
                ProcessStatus::Stopping | ProcessStatus::Exited
            ) {
                record.status = ProcessStatus::Starting;
            }
        }
        WorkerObservation::Status(status) => {
            if !matches!(
                record.status,
                ProcessStatus::Stopping | ProcessStatus::Exited
            ) {
                record.status = ProcessStatus::Running;
            }
            record.activity = status.activity;
            record.mode = status.mode;
            record.summary = status.summary;
            record.error = status.error;
            record.thread_id = status.thread_id;
            record.last_state_update_at = status.updated_at.max(observed_at);
        }
        WorkerObservation::Unresponsive(_message) => {
            if !matches!(
                record.status,
                ProcessStatus::Stopping | ProcessStatus::Exited
            ) {
                record.status = ProcessStatus::Unresponsive;
            }
        }
    }
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
