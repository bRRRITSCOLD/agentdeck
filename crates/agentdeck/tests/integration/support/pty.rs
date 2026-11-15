//! Running `agentdeck` on a terminal, for the prompts it shows only there.

use std::io::{Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use super::TestEnv;

/// How long a run on a terminal gets, prompt and answer included.
const TIMEOUT: Duration = Duration::from_secs(30);

/// What one run on a terminal did: everything it wrote to the terminal, the echoed answer
/// included, and whether it exited 0.
pub struct PtyRun {
    pub output: String,
    pub success: bool,
}

/// Runs `agentdeck <args>` with a terminal as stdin, stdout and stderr, as [`TestEnv::command`]
/// sets it up; once it has written `prompt`, types `answer` and Enter, then waits for it to
/// exit.
pub fn run_answering(env: &TestEnv, args: &[&str], prompt: &str, answer: &str) -> PtyRun {
    let mut command = env.command();
    command.args(args);
    let mut builder = CommandBuilder::new(command.get_program());
    builder.args(command.get_args());
    builder.env_clear();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            builder.env(name, value);
        }
    }
    builder.cwd(env.root());

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("opening a pty");
    let mut child = pair
        .slave
        .spawn_command(builder)
        .expect("starting agentdeck on the pty");
    // Only the child holds the terminal now, so reading ends once it exits.
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("the pty's reader");
    let mut writer = pair.master.take_writer().expect("the pty's writer");
    let (chunks, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0; 4096];
        // Linux answers EIO once the terminal's last holder has closed it.
        while let Ok(n @ 1..) = reader.read(&mut buf) {
            if chunks.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + TIMEOUT;
    let mut output = String::new();
    let mut answered = false;
    loop {
        if !answered && output.contains(prompt) {
            writer
                .write_all(format!("{answer}\n").as_bytes())
                .expect("typing the answer");
            answered = true;
        }
        match received.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(chunk) => output.push_str(&String::from_utf8_lossy(&chunk)),
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                panic!("agentdeck {args:?} didn't finish on the pty in {TIMEOUT:?}:\n{output}");
            }
        }
    }
    assert!(
        answered,
        "agentdeck {args:?} never asked {prompt:?}:\n{output}"
    );
    let status = child.wait().expect("waiting for agentdeck");
    PtyRun {
        output,
        success: status.success(),
    }
}
