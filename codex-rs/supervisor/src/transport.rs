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
        use tokio::time::Instant;
        use tokio::time::sleep;
        use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

        let deadline = Instant::now() + crate::REQUEST_TIMEOUT;
        loop {
            match ClientOptions::new().open(name) {
                Ok(client) => return Ok(Box::new(client)),
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        && Instant::now() < deadline =>
                {
                    // named pipe 的 server 会在请求结束后补建下一个实例；这段窗口内
                    // Windows 返回 ERROR_PIPE_BUSY。它表示暂时没有空闲实例，不代表
                    // supervisor 已退出，重试可以避免心跳把整个 worker 误判成失联。
                    sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to connect local IPC named pipe {name}"));
                }
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    unreachable!()
}

#[cfg(windows)]
pub(crate) fn create_named_pipe_server(
    name: &str,
    first_instance: bool,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use std::ptr::null_mut;
    use tokio::net::windows::named_pipe::ServerOptions;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Foundation::HLOCAL;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::Authorization::SDDL_REVISION_1;
    use windows_sys::Win32::Security::PSECURITY_DESCRIPTOR;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

    // 受限 token 需要能够打开由普通用户创建的 pipe；调用方仍必须通过随机 token
    // 完成协议认证，且 ServerOptions 默认拒绝远程客户端，所以这里只放宽本机句柄
    // 访问，不等于放宽 Supervisor 的操作权限。
    let sddl: Vec<u16> = "D:P(A;;GA;;;WD)\0".encode_utf16().collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(std::io::Error::from_raw_os_error(unsafe {
            GetLastError() as i32
        }));
    }

    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    if first_instance {
        options.first_pipe_instance(true);
    }
    let result = unsafe {
        options.create_with_security_attributes_raw(
            name,
            &mut attributes as *mut SECURITY_ATTRIBUTES as *mut std::ffi::c_void,
        )
    };
    unsafe {
        LocalFree(descriptor as HLOCAL);
    }
    result
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
