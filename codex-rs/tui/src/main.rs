use clap::Parser;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_config::LoaderOverrides;
use codex_supervisor::ProcessKind as SupervisorProcessKind;
use codex_supervisor::SupervisorLease;
use codex_tui::AppExitInfo;
use codex_tui::Cli;
use codex_tui::ExitReason;
use codex_tui::run_main;
use codex_utils_cli::CliConfigOverrides;
use std::io::Write;
use supports_color::Stream;

fn format_exit_messages(exit_info: AppExitInfo, color_enabled: bool) -> Vec<String> {
    let is_fatal = matches!(&exit_info.exit_reason, ExitReason::Fatal(_));
    let AppExitInfo {
        token_usage,
        thread_id,
        resume_hint,
        ..
    } = exit_info;

    let mut lines = Vec::new();
    if !token_usage.is_zero() {
        lines.push(token_usage.to_string());
    }

    if let Some(resume_cmd) = resume_hint {
        let command = if color_enabled {
            format!("\u{1b}[36m{resume_cmd}\u{1b}[39m")
        } else {
            resume_cmd
        };
        lines.push(format!("To continue this session, run {command}"));
    } else if is_fatal && let Some(thread_id) = thread_id {
        lines.push(format!("Session ID: {thread_id}"));
    }

    lines
}

#[derive(Parser, Debug)]
struct TopCli {
    #[clap(flatten)]
    config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    inner: Cli,
}

fn main() -> anyhow::Result<()> {
    // daemon 参数由 launcher 隐式传入；必须在 arg0 dispatch 和 clap 解析前截获，
    // 否则 standalone `codex-tui` 会把 supervisor 当成普通 TUI 参数继续启动第二个界面。
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == codex_supervisor::DAEMON_ARG)
    {
        return codex_supervisor::run_daemon_blocking();
    }
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        let mut supervisor_lease = supervise_standalone_tui().await?;
        let top_cli = TopCli::parse_from(codex_supervisor::filtered_process_args()?);
        let mut inner = top_cli.inner;
        inner
            .config_overrides
            .raw_overrides
            .splice(0..0, top_cli.config_overrides.raw_overrides);
        let result = run_main(
            inner,
            arg0_paths,
            LoaderOverrides::default(),
            /*explicit_remote_endpoint*/ None,
        )
        .await;
        if let Err(error) = supervisor_lease.close().await {
            tracing::warn!(%error, "failed to unregister standalone TUI from supervisor");
        }
        let exit_info = result?;
        let is_fatal = match &exit_info.exit_reason {
            ExitReason::Fatal(message) => {
                eprintln!("ERROR: {message}");
                true
            }
            ExitReason::UserRequested => false,
        };

        let color_enabled = supports_color::on(Stream::Stdout).is_some();
        for line in format_exit_messages(exit_info, color_enabled) {
            println!("{line}");
        }
        if is_fatal {
            std::io::stdout().flush()?;
            std::process::exit(1);
        }
        Ok(())
    })
}

async fn supervise_standalone_tui() -> anyhow::Result<SupervisorLease> {
    let executable = std::env::current_exe()?;
    if codex_supervisor::worker_identity()?.is_some() {
        let client = codex_supervisor::SupervisorClient::connect().await?;
        return client
            .register_current_process(SupervisorProcessKind::Tui, None)
            .await;
    }

    let client = codex_supervisor::ensure_supervisor(&executable).await?;
    client
        .register_current_process(SupervisorProcessKind::Tui, None)
        .await
}
