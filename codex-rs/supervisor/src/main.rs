fn main() -> anyhow::Result<()> {
    codex_supervisor::run_daemon_blocking()
}
