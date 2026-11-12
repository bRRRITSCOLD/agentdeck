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

/// Runs `future` on tokio; the returned task resolves on GPUI's side. A panic in `future`
/// resumes in whoever awaits the task.
pub fn spawn<F>(cx: &App, future: F) -> Task<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = cx.global::<Tokio>().0.spawn(future);
    cx.background_spawn(async move {
        match handle.await {
            Ok(output) => output,
            Err(err) => std::panic::resume_unwind(err.into_panic()),
        }
    })
}
