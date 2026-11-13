//! The tokio runtime under the GUI. The daemon's client (reqwest) and its WebSocket
//! (tokio-tungstenite) need tokio's reactor, and GPUI runs its own executors, so the app keeps
//! one small multi-thread runtime as a global and bridges its tasks back as GPUI [`Task`]s.

use std::future::Future;

use gpui::{App, AppContext as _, Global, Task};

/// The app's tokio runtime.
struct Tokio(tokio::runtime::Runtime);

impl Global for Tokio {}

/// Starts the runtime; call once, before anything is [`spawn`]ed.
pub fn init(cx: &mut App) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("agentdeck-tokio")
        .enable_all()
        .build()
        .expect("the tokio runtime starts");
    cx.set_global(Tokio(runtime));
}

/// Runs `future` on tokio; the returned task resolves on GPUI's side. Dropping the task
/// aborts `future`, as dropping a GPUI task cancels it: a read replaced by a newer one stops
/// instead of finishing its requests unread. A panic in `future` resumes in whoever awaits
/// the task.
pub fn spawn<F>(cx: &App, future: F) -> Task<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut handle = AbortOnDrop(cx.global::<Tokio>().0.spawn(future));
    cx.background_spawn(async move {
        match (&mut handle.0).await {
            Ok(output) => output,
            Err(err) => std::panic::resume_unwind(err.into_panic()),
        }
    })
}

/// Aborts its tokio task when dropped; a no-op once the task has finished.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use gpui::TestAppContext;

    use super::*;

    /// Signals on `dropped` when dropped: when the future holding it ends or is aborted.
    struct Signal(mpsc::Sender<()>);

    impl Drop for Signal {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    mod spawn {
        use super::*;

        #[gpui::test]
        fn should_abort_the_tokio_task_when_its_task_is_dropped(cx: &mut TestAppContext) {
            cx.update(init);
            let (dropped, aborted) = mpsc::channel();
            // Built outside the future, so it signals even if the abort comes before the
            // first poll.
            let signal = Signal(dropped);
            let task = cx.update(|cx| {
                spawn(cx, async move {
                    let _signal = signal;
                    std::future::pending::<()>().await;
                })
            });

            drop(task);
            cx.run_until_parked();

            aborted
                .recv_timeout(Duration::from_secs(5))
                .expect("the tokio task was dropped");
        }
    }
}
