//! PROTOTYPE (issue #2): GPUI client for spike-daemon.
//!
//! - `spike-gui window --session <id>`: a live window (typing, resize, colours, cursor).
//! - `spike-gui attach --session <id> --out <base> [opts]`: attach headlessly, optionally
//!   resize / type, wait until the output goes quiet, then render the grid with the same
//!   `TerminalView` through GPUI's headless renderer into `<base>.png`, plus `<base>.txt`
//!   (the grid as text) and stats JSON on stdout. Options:
//!   `--replay-limit N`, `--resize CxR`, `--send STR` (with \r \e \t \n escapes, repeatable),
//!   `--quiet-ms N` (default 600), `--max-ms N` (default 8000), `--hold-until FILE`,
//!   `--naive CxR` (experiment: replay at one fixed size, ignoring in-band resizes),
//!   `--snapshot-check` (experiment: re-encode the final screen and restore it into a fresh grid).

mod term;
mod view;
mod ws;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use futures::{SinkExt as _, StreamExt as _};
use gpui::{
    App, AppContext as _, Bounds, HeadlessAppContext, WindowBounds, WindowOptions, font, px, size,
};
use serde_json::json;

use view::{Attached, FONT_FAMILY, FONT_SIZE, LINE_HEIGHT, PAD, TerminalView};
use ws::{Message, resize_msg};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let session = flag("--session").context("--session <id>")?;
    match args.first().map(String::as_str) {
        Some("window") => {
            window(session);
            Ok(())
        }
        Some("attach") => {
            let sends: Vec<Vec<u8>> = args
                .windows(2)
                .filter(|w| w[0] == "--send")
                .map(|w| unescape(&w[1]))
                .collect();
            let opts = AttachOpts {
                session,
                out: PathBuf::from(flag("--out").context("--out <base>")?),
                replay_limit: flag("--replay-limit").map(|v| v.parse()).transpose()?,
                resize: flag("--resize").map(|v| {
                    let (c, r) = v.split_once('x').expect("CxR");
                    (c.parse().unwrap(), r.parse().unwrap())
                }),
                sends,
                quiet: Duration::from_millis(
                    flag("--quiet-ms").map_or(600, |v| v.parse().unwrap()),
                ),
                max: Duration::from_millis(flag("--max-ms").map_or(8000, |v| v.parse().unwrap())),
                hold_until: flag("--hold-until").map(PathBuf::from),
                snapshot_check: args.iter().any(|a| a == "--snapshot-check"),
                naive: flag("--naive").map(|v| {
                    let (c, r) = v.split_once('x').expect("CxR");
                    (c.parse().unwrap(), r.parse().unwrap())
                }),
            };
            attach(opts)
        }
        _ => bail!("usage: spike-gui window|attach --session <id> ..."),
    }
}

fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut b = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some(o) => out.extend_from_slice(o.to_string().as_bytes()),
            None => {}
        }
    }
    out
}

struct AttachOpts {
    session: String,
    out: PathBuf,
    replay_limit: Option<usize>,
    resize: Option<(u16, u16)>,
    sends: Vec<Vec<u8>>,
    quiet: Duration,
    max: Duration,
    hold_until: Option<PathBuf>,
    naive: Option<(u16, u16)>,
    snapshot_check: bool,
}

/// Reads until no message has arrived for `quiet` (or `max` passed).
async fn drain(
    ws: &mut ws::Ws,
    a: &mut Attached,
    quiet: Duration,
    max: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + max;
    loop {
        let next = futures_lite::future::or(async { Some(ws.next().await) }, async {
            async_io::Timer::after(quiet).await;
            None
        })
        .await;
        match next {
            None | Some(None) => return Ok(()),
            Some(Some(msg)) => {
                let replies = a.apply(msg?);
                if !replies.is_empty() {
                    ws.send(Message::binary(replies)).await?;
                }
            }
        }
        if Instant::now() > deadline {
            return Ok(());
        }
    }
}

fn attach(o: AttachOpts) -> anyhow::Result<()> {
    let mut a = Attached::new();
    a.naive = o.naive;
    let started = Instant::now();
    futures_lite::future::block_on(async {
        let mut ws = ws::connect(&o.session, o.replay_limit).await?;
        // Replay: read until the server says "live".
        while !a.live {
            let msg = ws.next().await.context("closed during replay")??;
            a.apply(msg);
        }
        let live_ms = started.elapsed().as_secs_f64() * 1000.;
        let replay_parser_ms = a.feed_time.as_secs_f64() * 1000.;
        if let Some((c, r)) = o.resize {
            a.model.resize(c, r);
            ws.send(resize_msg(c, r)).await?;
            drain(&mut ws, &mut a, Duration::from_millis(300), o.max).await?;
        }
        for bytes in &o.sends {
            ws.send(Message::binary(bytes.clone())).await?;
            drain(&mut ws, &mut a, Duration::from_millis(400), o.max).await?;
        }
        if let Some(flag) = &o.hold_until {
            while !flag.exists() {
                drain(
                    &mut ws,
                    &mut a,
                    Duration::from_millis(100),
                    Duration::from_millis(100),
                )
                .await?;
            }
        }
        drain(&mut ws, &mut a, o.quiet, o.max).await?;
        ws.close(None).await.ok();
        let stats = json!({
            "hello": a.hello,
            "live": a.live_msg,
            "attach_to_live_ms": live_ms,
            "replay_parser_ms": replay_parser_ms,
            "fed_bytes_total": a.fed_bytes,
            "alt_screen": a.model.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN),
            "size": a.model.size(),
            "agent_state": a.agent_state,
            "dropped_replies_during_replay": *a.model.listener.dropped_replies.lock().unwrap(),
            "title": *a.model.listener.title.lock().unwrap(),
            "sent_replies": *a.model.listener.sent_replies.lock().unwrap(),
        });
        println!("{}", serde_json::to_string(&stats)?);
        anyhow::Ok(())
    })?;
    std::fs::write(o.out.with_extension("txt"), a.model.text())?;
    if o.snapshot_check {
        // Q2 experiment: re-encode the screen, feed it to a fresh grid, check it matches.
        let snap = a.model.serialize();
        let (c, r) = a.model.size();
        let mut b = Attached::new();
        b.model = term::TermModel::new(c, r);
        b.model.feed(&snap);
        b.hello = json!({"replay_bytes": format!("{} (snapshot)", snap.len())});
        let same = b.model.text() == a.model.text();
        eprintln!(
            "snapshot: {} bytes, restored text identical: {same}",
            snap.len()
        );
        let base = o.out.with_extension("");
        let restored = PathBuf::from(format!("{}-snapshot", base.display()));
        std::fs::write(restored.with_extension("txt"), b.model.text())?;
        render_png(b, &restored.with_extension("png"))?;
    }
    render_png(a, &o.out.with_extension("png"))
}

/// Renders the grid through the same view, offscreen (a Vulkan driver, no display server).
fn render_png(a: Attached, out: &Path) -> anyhow::Result<()> {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let ts = cx.text_system().clone();
    let id = ts.resolve_font(&font(FONT_FAMILY));
    let family = ts.get_font_for_id(id).map(|f| f.family);
    ensure!(
        family.as_deref() == Some(FONT_FAMILY),
        "{FONT_FAMILY} resolved to {family:?}"
    );
    let cell_w = ts.advance(id, FONT_SIZE, 'm')?.width;
    let (cols, rows) = a.model.size();
    let win_size = size(
        cell_w * cols as f32 + PAD * 2.,
        LINE_HEIGHT * (rows as f32 + 1.) + PAD * 2.,
    );
    let window = cx.open_window(win_size, move |_, cx| {
        cx.new(|cx| {
            let mut v = TerminalView::new(a, None, false, cx);
            v.status = "headless".into();
            v
        })
    })?;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))?;
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.refresh();
        window.draw(cx).clear(cx)
    })?;
    let shot = cx.capture_screenshot(window.into())?;
    shot.save(out)
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

fn window(session: String) {
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(560.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let (tx, mut rx) = futures::channel::mpsc::unbounded::<Message>();
                let view = cx.new(|cx| TerminalView::new(Attached::new(), Some(tx), true, cx));
                let focus = view.read(cx).focus.clone();
                focus.focus(window, cx);
                // The socket runs on GPUI's executors: async-net's reactor thread wakes them.
                view.update(cx, |_, cx| {
                    cx.spawn(async move |this, cx| {
                        let ws = match ws::connect(&session, None).await {
                            Ok(ws) => ws,
                            Err(e) => {
                                eprintln!("connect: {e:#}");
                                return;
                            }
                        };
                        let (mut sink, mut stream) = ws.split();
                        let writer = cx.background_spawn(async move {
                            while let Some(m) = rx.next().await {
                                if sink.send(m).await.is_err() {
                                    break;
                                }
                            }
                        });
                        while let Some(Ok(msg)) = stream.next().await {
                            let ok = this.update(cx, |v, cx| {
                                let replies = v.attached.apply(msg);
                                if !replies.is_empty()
                                    && let Some(tx) = &v.tx
                                {
                                    let _ = tx.unbounded_send(Message::binary(replies));
                                }
                                if v.attached.live && v.status.as_ref() != "live" {
                                    eprintln!(
                                        "window: live after {:.1} ms, replayed {} B, grid {:?}",
                                        v.attached.live_at_ms.unwrap_or_default(),
                                        v.attached.fed_bytes,
                                        v.attached.model.size()
                                    );
                                }
                                v.status = if v.attached.live {
                                    "live".into()
                                } else {
                                    "replaying".into()
                                };
                                cx.notify();
                            });
                            if ok.is_err() {
                                break;
                            }
                        }
                        drop(writer);
                    })
                    .detach();
                });
                view
            },
        )
        .expect("window opens");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
        // Spike smoke runs: quit on a timer.
        if let Some(ms) = std::env::var("SPIKE_GUI_EXIT_AFTER_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(ms))
                    .await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
}
