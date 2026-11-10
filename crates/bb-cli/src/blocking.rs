//! Boundaries for blocking work owned by this CLI process.

use anyhow::{Context, Result};
use tokio::sync::oneshot;

/// Runs potentially unbounded input or computation outside the runtime's blocking pool.
///
/// The process supervises the receiver. Dropping it abandons only local work; the OS
/// reclaims the dedicated thread when the CLI exits. Never use this for mutations.
pub async fn isolated<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = oneshot::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // Cancellation may drop the receiver while a pipe or jq is still running.
            let _ = sender.send(work());
        })
        .context("cannot start isolated worker")?;
    receiver
        .await
        .context("isolated worker terminated unexpectedly")?
}
