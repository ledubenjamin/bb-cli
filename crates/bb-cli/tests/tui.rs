#[cfg(unix)]
#[tokio::test]
async fn terminal_navigation_search_cancellation_and_restore() -> anyhow::Result<()> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        tokio::process::Command::new("python3")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tui_smoke.py"))
            .arg(env!("CARGO_BIN_EXE_bb"))
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn native_console_help_quit_interrupt_and_restore() -> anyhow::Result<()> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        tokio::process::Command::new("python")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tui_windows.py"))
            .arg(env!("CARGO_BIN_EXE_bb"))
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
