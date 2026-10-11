//! RouteForge Telegram bot. Implemented in Phase 7, see `docs/plan.md`.
//!
//! Until then it holds its place in the stack (D56): it starts, says that it has
//! nothing to do, and waits to be told to stop. The bot that is to come does the same
//! when it is given no tokens (D8).

use std::io::IsTerminal;

use tracing::info;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        "the bot is built in Phase 7; until then it idles"
    );
    told_to_stop().await;
    info!("the bot has stopped");
}

/// Returns when the process is asked to stop: Ctrl-C anywhere, and SIGTERM where
/// there is one, which is what `docker stop` sends.
///
/// In a container the bot is the first process, and the first process is not ended by
/// a signal it does not listen for. Without this, stopping the stack would wait out
/// the ten seconds Docker allows and then kill.
async fn told_to_stop() {
    let interrupted = async {
        if tokio::signal::ctrl_c().await.is_err() {
            // Without a handler only a kill ends the process, and that needs no help.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        if let Ok(mut terminated) = signal(SignalKind::terminate()) {
            tokio::select! {
                () = interrupted => {}
                _ = terminated.recv() => {}
            }
            return;
        }
    }
    interrupted.await;
}
