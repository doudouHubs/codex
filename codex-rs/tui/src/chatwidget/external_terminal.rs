use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::mem::size_of;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tempfile::Builder;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
use windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32FirstW;
use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32NextW;
use windows_sys::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

const MAX_PARENT_PROCESS_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellKind {
    Cmd,
    WindowsPowerShell,
    PowerShellCore,
}

impl ShellKind {
    fn executable(self) -> &'static str {
        match self {
            Self::Cmd => "cmd.exe",
            Self::WindowsPowerShell => "powershell.exe",
            Self::PowerShellCore => "pwsh.exe",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct LaunchSpec {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    creation_flags: u32,
}

#[derive(Debug)]
struct ProcessInfo {
    parent_process_id: u32,
    executable_name: String,
}

pub(super) fn launch(command: &str, cwd: &Path) -> std::io::Result<()> {
    let shell = detect_shell();
    if std::env::var_os("WT_SESSION").is_some() {
        match launch_in_windows_terminal(shell, command, cwd) {
            Ok(()) => return Ok(()),
            Err(wt_error) => {
                // WT 启动失败时按用户确认的策略退回 shell 新窗口；只有两条路径都失败，
                // 才把错误返回给 TUI，避免因为 WT 的单点故障让 Ctrl+Enter 完全失效。
                return launch_direct_shell(shell, command, cwd).map_err(|fallback_error| {
                    io::Error::new(
                        fallback_error.kind(),
                        format!(
                            "Windows Terminal failed: {wt_error}; shell fallback failed: {fallback_error}"
                        ),
                    )
                });
            }
        }
    }

    launch_direct_shell(shell, command, cwd)
}

fn launch_in_windows_terminal(shell: ShellKind, command: &str, cwd: &Path) -> std::io::Result<()> {
    let wrapper_path = create_shell_wrapper(shell, command)?;
    let spec = build_windows_terminal_spec(&wrapper_path, cwd);

    match spawn_launch_spec(&spec) {
        Ok(()) => Ok(()),
        Err(error) => {
            // WT 未成功接管 wrapper 时没有子进程负责清理，失败路径必须立即删除临时文件。
            let _ = fs::remove_file(&wrapper_path);
            Err(error)
        }
    }
}

fn launch_direct_shell(shell: ShellKind, command: &str, cwd: &Path) -> std::io::Result<()> {
    let spec = build_direct_shell_spec(shell, command, cwd);
    spawn_launch_spec(&spec)
}

fn spawn_launch_spec(spec: &LaunchSpec) -> std::io::Result<()> {
    let mut process = Command::new(&spec.program);
    process.args(&spec.args).current_dir(&spec.cwd);
    if spec.creation_flags == 0 {
        // wt.exe 只是把命令转交给现有窗口；隔离它的标准流，避免客户端干扰当前 TUI。
        process
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    if spec.creation_flags != 0 {
        use std::os::windows::process::CommandExt;

        process.creation_flags(spec.creation_flags);
    }
    let program = spec.program.display().to_string();
    process.spawn().map(|_| ()).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to start `{program}`: {error}"),
        )
    })
}

fn build_direct_shell_spec(shell: ShellKind, command: &str, cwd: &Path) -> LaunchSpec {
    let args = match shell {
        ShellKind::Cmd => vec![OsString::from("/k"), OsString::from(command)],
        ShellKind::WindowsPowerShell | ShellKind::PowerShellCore => vec![
            OsString::from("-NoExit"),
            OsString::from("-Command"),
            OsString::from(command),
        ],
    };

    LaunchSpec {
        program: shell_program(shell),
        args,
        cwd: cwd.to_path_buf(),
        creation_flags: CREATE_NEW_CONSOLE,
    }
}

fn build_windows_terminal_spec(wrapper_path: &Path, cwd: &Path) -> LaunchSpec {
    LaunchSpec {
        program: windows_terminal_program(),
        args: vec![
            OsString::from("new-tab"),
            OsString::from("-d"),
            cwd.as_os_str().to_owned(),
            OsString::from("--"),
            wrapper_path.as_os_str().to_owned(),
        ],
        cwd: cwd.to_path_buf(),
        creation_flags: 0,
    }
}

fn create_shell_wrapper(shell: ShellKind, command: &str) -> io::Result<PathBuf> {
    let temp_path = Builder::new()
        .prefix("codex-external-terminal-")
        .suffix(".cmd")
        .tempfile()?
        .into_temp_path();
    let content = build_shell_wrapper_content(shell, command);
    fs::write(&temp_path, content)?;
    temp_path
        .keep()
        .map_err(|error| io::Error::other(format!("failed to keep terminal wrapper: {error}")))
}

fn build_shell_wrapper_content(shell: ShellKind, command: &str) -> String {
    match shell {
        ShellKind::Cmd => {
            format!("@echo off\r\n{command}\r\ncmd.exe /d /k\r\n")
        }
        ShellKind::WindowsPowerShell | ShellKind::PowerShellCore => {
            let encoded_command = encode_powershell_command(command);
            format!(
                "@echo off\r\ncall {} -NoExit -EncodedCommand {encoded_command}\r\n",
                quote_cmd_argument(&shell_program(shell))
            )
        }
    }
}

fn encode_powershell_command(command: &str) -> String {
    let bytes = command
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    STANDARD.encode(bytes)
}

fn quote_cmd_argument(path: &Path) -> String {
    format!("\"{}\"", path.to_string_lossy())
}

fn shell_program(shell: ShellKind) -> PathBuf {
    match shell {
        ShellKind::Cmd => std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(shell.executable())),
        ShellKind::WindowsPowerShell | ShellKind::PowerShellCore => {
            // WT 服务进程可能继承旧 PATH；把当前 Codex 解析出的 shell 路径写入 wrapper，
            // 确保新 tab 使用和 Codex 相同的 shell，而不是 WT 默认 profile 的 shell。
            which::which(shell.executable()).unwrap_or_else(|_| PathBuf::from(shell.executable()))
        }
    }
}

fn windows_terminal_program() -> PathBuf {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") else {
        return PathBuf::from("wt.exe");
    };

    // Windows Terminal 的应用执行别名通常不在 Codex 启动器继承的 PATH 中，但固定落在
    // 当前用户的 WindowsApps 目录；优先使用绝对路径，才能覆盖 PATH 被裁剪的启动场景。
    let alias = PathBuf::from(local_app_data)
        .join("Microsoft")
        .join("WindowsApps")
        .join("wt.exe");
    if alias.is_file() {
        alias
    } else {
        // 保留裸名称兜底，兼容用户自定义 PATH 或非商店版 Windows Terminal。
        PathBuf::from("wt.exe")
    }
}

fn detect_shell() -> ShellKind {
    let preferred_shell = if which::which(ShellKind::PowerShellCore.executable()).is_ok() {
        Some(ShellKind::PowerShellCore)
    } else {
        None
    };
    let parent_shell = if preferred_shell.is_some() {
        None
    } else {
        detect_parent_shell()
    };
    select_shell(preferred_shell, parent_shell)
}

fn detect_parent_shell() -> Option<ShellKind> {
    let processes = process_table();
    let mut process_id = unsafe { GetCurrentProcessId() };

    for _ in 0..MAX_PARENT_PROCESS_DEPTH {
        let Some(process) = processes.get(&process_id) else {
            break;
        };
        if let Some(shell) = shell_kind_from_process_name(&process.executable_name) {
            return Some(shell);
        }
        if process.parent_process_id == process_id {
            break;
        }
        process_id = process.parent_process_id;
    }

    None
}

fn select_shell(preferred_shell: Option<ShellKind>, parent_shell: Option<ShellKind>) -> ShellKind {
    // 用户要求优先使用 PowerShell 7；只有本机无法解析 pwsh.exe 时才保留当前宿主 shell，
    // 避免在 cmd 或 Windows PowerShell 环境中强行启动不可用的程序。
    preferred_shell
        .or(parent_shell)
        // 用户明确要求识别失败时仍可执行；cmd.exe 是 Windows 上最稳定的最终兜底。
        .unwrap_or(ShellKind::Cmd)
}

fn process_table() -> HashMap<u32, ProcessInfo> {
    let mut processes = HashMap::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return processes;
    }

    unsafe {
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                processes.insert(
                    entry.th32ProcessID,
                    ProcessInfo {
                        parent_process_id: entry.th32ParentProcessID,
                        executable_name: utf16z_to_string(&entry.szExeFile),
                    },
                );
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    processes
}

fn utf16z_to_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

fn shell_kind_from_process_name(name: &str) -> Option<ShellKind> {
    let executable_name = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    match executable_name.as_str() {
        "cmd.exe" => Some(ShellKind::Cmd),
        "powershell.exe" => Some(ShellKind::WindowsPowerShell),
        "pwsh.exe" => Some(ShellKind::PowerShellCore),
        _ => None,
    }
}

#[cfg(test)]
#[path = "external_terminal_tests.rs"]
mod tests;
