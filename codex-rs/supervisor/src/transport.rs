use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone)]
pub(crate) enum Endpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    #[cfg(windows)]
    Windows(String),
}

impl Endpoint {
    pub(crate) fn from_codex_home(codex_home: &Path) -> Self {
        #[cfg(unix)]
        {
            Self::Unix(endpoint_for_codex_home(codex_home))
        }
        #[cfg(windows)]
        {
            Self::Windows(windows_pipe_name(codex_home))
        }
    }

    pub(crate) fn from_worker_address(address: &str) -> Self {
        #[cfg(unix)]
        {
            Self::Unix(PathBuf::from(address))
        }
        #[cfg(windows)]
        {
            Self::Windows(address.to_string())
        }
    }
}

pub(crate) fn endpoint_for_codex_home(codex_home: &Path) -> PathBuf {
    codex_home
        .join(crate::SUPERVISOR_DIR_NAME)
        .join(crate::SOCKET_FILE_NAME)
}

#[cfg(windows)]
fn windows_pipe_name(codex_home: &Path) -> String {
    use std::hash::Hash;
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    codex_home.to_string_lossy().hash(&mut hasher);
    format!(r"\\.\pipe\codex-supervisor-{:016x}", hasher.finish())
}

pub(crate) fn worker_endpoint_for_codex_home(codex_home: &Path, id: uuid::Uuid) -> String {
    #[cfg(unix)]
    {
        codex_home
            .join(crate::SUPERVISOR_DIR_NAME)
            .join(format!("worker-{id}.sock"))
            .display()
            .to_string()
    }
    #[cfg(windows)]
    {
        use std::hash::Hash;
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        codex_home.to_string_lossy().hash(&mut hasher);
        format!(
            r"\\.\pipe\codex-supervisor-worker-{:016x}-{id}",
            hasher.finish()
        )
    }
}

pub(crate) async fn connect_endpoint(endpoint: &Endpoint) -> Result<BoxedIo> {
    #[cfg(unix)]
    {
        let Endpoint::Unix(path) = endpoint;
        return Ok(Box::new(
            tokio::net::UnixStream::connect(path)
                .await
                .with_context(|| {
                    format!("failed to connect local IPC endpoint {}", path.display())
                })?,
        ));
    }
    #[cfg(windows)]
    {
        let Endpoint::Windows(name) = endpoint;
        use tokio::net::windows::named_pipe::ClientOptions;
        Ok(Box::new(ClientOptions::new().open(name).with_context(
            || format!("failed to connect local IPC named pipe {name}"),
        )?))
    }
    #[cfg(not(any(unix, windows)))]
    unreachable!()
}

pub(crate) trait AsyncIo: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> AsyncIo for T {}
pub(crate) type BoxedIo = Box<dyn AsyncIo + Unpin + Send>;

pub(crate) async fn write_frame<S, T>(stream: &mut S, value: &T) -> Result<()>
where
    S: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value).context("failed to serialize supervisor frame")?;
    if payload.len() > crate::MAX_FRAME_SIZE {
        bail!("supervisor frame exceeds {} bytes", crate::MAX_FRAME_SIZE);
    }
    stream.write_u32_le(payload.len() as u32).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

pub(crate) async fn read_frame<S, T>(stream: &mut S) -> Result<T>
where
    S: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let length = stream.read_u32_le().await? as usize;
    if length > crate::MAX_FRAME_SIZE {
        bail!("supervisor frame exceeds {} bytes", crate::MAX_FRAME_SIZE);
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).context("failed to deserialize supervisor frame")
}
