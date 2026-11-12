//! PROTOTYPE: the WebSocket client. async-tungstenite over async-net, so it runs on GPUI's
//! executor (or any `block_on`) without tokio.

use anyhow::Context as _;
use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::client::IntoClientRequest as _;
use serde_json::Value;

pub use async_tungstenite::tungstenite::Message;

pub type Ws = WebSocketStream<async_net::TcpStream>;

pub fn daemon() -> anyhow::Result<(u16, String)> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR")?;
    let file = std::path::Path::new(&runtime).join("agentdeck-spike/daemon.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).context("daemon.json")?)?;
    Ok((
        v["port"].as_u64().context("port")? as u16,
        v["token"].as_str().context("token")?.to_owned(),
    ))
}

pub async fn connect(session: &str, replay_limit: Option<usize>) -> anyhow::Result<Ws> {
    let (port, token) = daemon()?;
    let query = replay_limit
        .map(|n| format!("?replay_limit={n}"))
        .unwrap_or_default();
    let mut req =
        format!("ws://127.0.0.1:{port}/sessions/{session}/ws{query}").into_client_request()?;
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let tcp = async_net::TcpStream::connect(("127.0.0.1", port)).await?;
    tcp.set_nodelay(true)?;
    let (ws, _) = async_tungstenite::client_async(req, tcp).await?;
    Ok(ws)
}

pub fn resize_msg(cols: u16, rows: u16) -> Message {
    Message::text(serde_json::json!({"t": "resize", "cols": cols, "rows": rows}).to_string())
}
