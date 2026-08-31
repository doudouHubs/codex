use crate::ProcessKind;
use crate::SupervisorSnapshot;
use crate::WorkPage;
use crate::WorkSection;
use crate::WorkerStatus;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Request {
    Ping,
    Register(RegisterRequest),
    Unregister { id: Uuid, lease_token: Uuid },
    List,
    ReadWork(WorkRequest),
    SpawnWorker(SpawnWorkerRequest),
    Terminate { id: Uuid },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegisterRequest {
    pub(crate) id: Option<Uuid>,
    pub(crate) lease_token: Option<Uuid>,
    pub(crate) pid: u32,
    pub(crate) parent_pid: Option<u32>,
    pub(crate) kind: ProcessKind,
    pub(crate) executable: PathBuf,
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) thread_id: Option<String>,
    pub(crate) worker_endpoint: String,
    pub(crate) worker_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkRequest {
    pub(crate) id: Uuid,
    pub(crate) section: WorkSection,
    pub(crate) cursor: Option<String>,
    pub(crate) limit: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpawnWorkerRequest {
    pub(crate) executable: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) kind: ProcessKind,
    pub(crate) thread_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Response {
    Pong,
    Registered {
        id: Uuid,
        lease_token: Uuid,
        #[serde(default)]
        protocol_version: Option<u32>,
    },
    Spawned {
        id: Uuid,
        pid: u32,
    },
    Snapshot(SupervisorSnapshot),
    WorkPage(WorkPage),
    Ack,
    Error {
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Envelope {
    pub(crate) token: String,
    pub(crate) request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum WorkerRequest {
    ReadStatus,
    ReadWork(WorkRequest),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum WorkerResponse {
    Status(WorkerStatus),
    WorkPage(WorkPage),
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WorkerEnvelope {
    pub(crate) token: String,
    pub(crate) request: WorkerRequest,
}
