use std::ffi::OsString;
use std::path::PathBuf;

use super::LaunchSpec;
use super::ShellKind;
use super::build_direct_shell_spec;
use super::build_shell_wrapper_content;
use super::build_windows_terminal_spec;
use super::encode_powershell_command;
use super::select_shell;
use super::shell_kind_from_process_name;
use super::shell_program;
use super::windows_terminal_program;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pretty_assertions::assert_eq;
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

#[test]
fn command_shell_spec_uses_new_console_and_keeps_cmd_open() {
    let cwd = PathBuf::from(r"C:\workspace");
    let spec = build_direct_shell_spec(ShellKind::Cmd, "echo ready", &cwd);

    assert_eq!(
        spec,
        LaunchSpec {
            program: shell_program(ShellKind::Cmd),
            args: vec![OsString::from("/k"), OsString::from("echo ready")],
            cwd,
            creation_flags: CREATE_NEW_CONSOLE,
        }
    );
}

#[test]
fn windows_terminal_spec_uses_single_wrapper_without_fixed_window() {
    let cwd = PathBuf::from(r"C:\workspace");
    let wrapper = PathBuf::from(r"C:\Temp\codex-external-terminal.cmd");
    let spec = build_windows_terminal_spec(&wrapper, &cwd);

    assert_eq!(
        spec,
        LaunchSpec {
            program: windows_terminal_program(),
            args: vec![
                OsString::from("new-tab"),
                OsString::from("-d"),
                cwd.as_os_str().to_owned(),
                OsString::from("--"),
                wrapper.as_os_str().to_owned(),
            ],
            cwd,
            creation_flags: 0,
        }
    );
}

#[test]
fn powershell_wrapper_encodes_command_and_keeps_shell_open() {
    let command = "Write-Host '中文' | Out-Host";
    let content = build_shell_wrapper_content(ShellKind::PowerShellCore, command);
    let encoded = encode_powershell_command(command);

    assert!(content.contains("-NoExit -EncodedCommand"));
    assert!(content.contains(&encoded));
    assert!(!content.contains(command));
    assert!(!content.contains("Remove-Item"));
    assert!(!content.contains("try"));
    assert!(!content.contains("finally"));

    let bytes = STANDARD.decode(encoded).expect("valid PowerShell encoding");
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    assert_eq!(String::from_utf16(&units).expect("valid UTF-16"), command);
}

#[test]
fn cmd_wrapper_runs_command_then_keeps_cmd_open() {
    let content = build_shell_wrapper_content(ShellKind::Cmd, "echo ready");

    assert_eq!(content, "@echo off\r\necho ready\r\ncmd.exe /d /k\r\n");
    assert!(!content.contains("del /f"));
    assert!(!content.contains("%~f0"));
}

#[test]
fn shell_process_names_are_case_insensitive() {
    assert_eq!(
        shell_kind_from_process_name("C:\\Windows\\System32\\POWERSHELL.EXE"),
        Some(ShellKind::WindowsPowerShell)
    );
    assert_eq!(shell_kind_from_process_name("cargo.exe"), None);
}

#[test]
fn pwsh_is_preferred_over_the_detected_parent_shell() {
    assert_eq!(
        select_shell(Some(ShellKind::PowerShellCore), Some(ShellKind::Cmd)),
        ShellKind::PowerShellCore
    );
    assert_eq!(
        select_shell(
            Some(ShellKind::PowerShellCore),
            Some(ShellKind::WindowsPowerShell),
        ),
        ShellKind::PowerShellCore
    );
}

#[test]
fn detected_parent_shell_is_used_when_pwsh_is_unavailable() {
    assert_eq!(
        select_shell(None, Some(ShellKind::WindowsPowerShell)),
        ShellKind::WindowsPowerShell
    );
    assert_eq!(
        select_shell(None, Some(ShellKind::PowerShellCore)),
        ShellKind::PowerShellCore
    );
}

#[test]
fn cmd_is_the_final_shell_fallback() {
    assert_eq!(select_shell(None, None), ShellKind::Cmd);
}
