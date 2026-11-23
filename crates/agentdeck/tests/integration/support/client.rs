//! A long-lived client of the daemon, as the GUI is: this test binary re-run as a child, in a
//! [`TestEnv`]'s environment, that connects through `agentdeck_client::daemon::connect` each
//! time it is asked. The daemon its first connect starts is its child for as long as it runs,
//! so a test can kill that daemon and check the client reaps it and starts the next one.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use super::TestEnv;

/// Set in the client side's environment only: it is the client, not the test.
const CLIENT_SIDE: &str = "AGENTDECK_TEST_CLIENT_SIDE";

/// What the client side prints before each daemon's pid, so libtest's own lines are skipped.
const PID: &str = "client side: daemon pid ";

/// How long one connect gets, a daemon start included.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The client side, when this process is one: connects once per line on stdin and prints the
/// pid of the daemon it reached, until stdin closes. Answers whether it was one; a test calls
/// it first and returns when it was.
pub fn ran_as_client_side() -> bool {
    if std::env::var_os(CLIENT_SIDE).is_none() {
        return false;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    for line in std::io::stdin().lock().lines() {
        line.expect("reading the test's request");
        let daemon = runtime
            .block_on(agentdeck_client::daemon::connect())
            .expect("connecting to the daemon");
        println!("{PID}{}", daemon.info.pid);
        std::io::stdout().flush().expect("flushing stdout");
    }
    true
}

/// The client side of `test`, running. Dropping it kills it.
pub struct LongLivedClient {
    child: Child,
    stdin: ChildStdin,
    pids: Receiver<u32>,
}

impl LongLivedClient {
    /// Starts `test` (its full path) as the client side, in `env`'s environment.
    pub fn start(env: &TestEnv, test: &str) -> LongLivedClient {
        let agentdeck = env.command();
        let mut command = Command::new(std::env::current_exe().expect("the test binary"));
        command.env_clear();
        for (name, value) in agentdeck.get_envs() {
            if let Some(value) = value {
                command.env(name, value);
            }
        }
        let mut child = command
            .args([test, "--exact", "--nocapture"])
            .env(CLIENT_SIDE, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("starting the client side");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (sender, pids) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(pid) = line.strip_prefix(PID)
                    && sender.send(pid.parse().expect("a pid")).is_err()
                {
                    break;
                }
            }
        });
        LongLivedClient { child, stdin, pids }
    }

    /// Its pid.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Has it connect, starting a daemon when none answers; answers that daemon's pid.
    pub fn connect(&mut self) -> u32 {
        writeln!(self.stdin).expect("asking the client side to connect");
        self.stdin
            .flush()
            .expect("flushing the client side's stdin");
        match self.pids.recv_timeout(TIMEOUT) {
            Ok(pid) => pid,
            Err(RecvTimeoutError::Timeout) => {
                panic!("the client side didn't connect in {TIMEOUT:?}")
            }
            Err(RecvTimeoutError::Disconnected) => panic!("the client side exited"),
        }
    }
}

impl Drop for LongLivedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
