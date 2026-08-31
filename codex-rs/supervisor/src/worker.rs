use crate::SupervisorReporter;
use crate::protocol::WorkerEnvelope;
use crate::protocol::WorkerRequest;
use crate::protocol::WorkerResponse;
use crate::transport::Endpoint;
use crate::transport::write_frame;
#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
#[cfg(unix)]
use std::path::PathBuf;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::task::JoinHandle;

/// worker 侧的受控读服务。supervisor 只保留 endpoint/token，不直接持有完整会话内容。
pub(crate) struct WorkerControlServer {
    task: JoinHandle<()>,
    #[cfg(unix)]
    path: PathBuf,
}

impl WorkerControlServer {
    pub(crate) async fn start(
        address: String,
        token: String,
        reporter: SupervisorReporter,
    ) -> Result<Self> {
        let endpoint = Endpoint::from_worker_address(&address);

        #[cfg(unix)]
        {
            let Endpoint::Unix(path) = endpoint else {
                unreachable!();
            };
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if path.exists() {
                let _ = tokio::fs::remove_file(&path).await;
            }
            let listener = tokio::net::UnixListener::bind(&path).with_context(|| {
                format!("failed to bind worker control socket {}", path.display())
            })?;
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
            let task = tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let token = token.clone();
                    let reporter = reporter.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream, token, reporter).await {
                            eprintln!("worker control request failed: {error}");
                        }
                    });
                }
            });
            return Ok(Self { task, path });
        }

        #[cfg(windows)]
        {
            let Endpoint::Windows(name) = endpoint;
            let task = tokio::spawn(async move {
                loop {
                    let server = match crate::transport::create_named_pipe_server(&name, false) {
                        Ok(server) => server,
                        Err(error) => {
                            eprintln!("failed to create worker control pipe: {error}");
                            return;
                        }
                    };
                    if server.connect().await.is_err() {
                        return;
                    }
                    let token = token.clone();
                    let reporter = reporter.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(server, token, reporter).await {
                            eprintln!("worker control request failed: {error}");
                        }
                    });
                }
            });
            Ok(Self { task })
        }
    }

    pub(crate) async fn close(self) {
        self.task.abort();
        #[cfg(unix)]
        {
            let _ = tokio::fs::remove_file(self.path).await;
        }
    }
}

impl Drop for WorkerControlServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_connection<S>(
    mut stream: S,
    token: String,
    reporter: SupervisorReporter,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let envelope: WorkerEnvelope = crate::transport::read_frame(&mut stream).await?;
    if envelope.token != token {
        write_frame(
            &mut stream,
            &WorkerResponse::Error {
                message: "invalid worker control token".to_string(),
            },
        )
        .await?;
        return Ok(());
    }
    let response = match envelope.request {
        WorkerRequest::ReadStatus => WorkerResponse::Status(reporter.status()),
        WorkerRequest::ReadWork(request) => reporter
            .page(
                request.id,
                request.section,
                request.cursor.as_deref(),
                request.limit,
            )
            .map_or_else(
                |message| WorkerResponse::Error { message },
                WorkerResponse::WorkPage,
            ),
    };
    write_frame(&mut stream, &response).await
}
