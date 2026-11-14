//! Running `terminal.file_opener` (spec §2, §11) for a `secondary`-click on a file path.
//!
//! The command is the one the Settings view last read ([`set_file_opener`]); the process is
//! started by the [`Spawn`] in the [`Opener`] global, which `main` sets to [`Processes`] and
//! tests to a fake. With no `Opener` set, nothing is run.

use std::io;
use std::process::{Command, Stdio};
use std::sync::Arc;

use agentdeck_core::opener::{self, FileOpener};
use gpui::{App, Global};

/// Starts a program.
pub trait Spawn: Send + Sync {
    /// Starts `argv[0]` with the rest as its arguments, detached: it isn't waited for, and
    /// outlives the click. An error is the program not starting.
    fn spawn(&self, argv: &[String]) -> io::Result<()>;
}

/// Starts real processes, straight from the argv: never through a shell.
pub struct Processes;

impl Spawn for Processes {
    fn spawn(&self, argv: &[String]) -> io::Result<()> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no program to run"))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Its own process group, so a signal meant for the GUI's group doesn't reach it.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn()?;
        // Reaped on a thread of its own, so it doesn't linger as a zombie.
        std::thread::spawn(move || child.wait());
        Ok(())
    }
}

/// What starts the opener.
#[derive(Clone)]
pub struct Opener(pub Arc<dyn Spawn>);

impl Global for Opener {}

/// `terminal.file_opener` as last read.
struct FileOpenerCommand(FileOpener);

impl Global for FileOpenerCommand {}

/// Starts openers as real processes; `main` calls it once.
pub fn use_processes(cx: &mut App) {
    cx.set_global(Opener(Arc::new(Processes)));
}

/// Makes `command` the opener a click runs; one that isn't an opener leaves the last one.
pub fn set_file_opener(command: &str, cx: &mut App) {
    match FileOpener::parse(command) {
        Ok(opener) => cx.set_global(FileOpenerCommand(opener)),
        Err(err) => tracing::warn!("terminal.file_opener {err}; keeping the last one"),
    }
}

/// The argv the opener runs for `path` at `line` and `col`.
pub fn argv(path: &str, line: Option<u32>, col: Option<u32>, cx: &App) -> Vec<String> {
    match cx.try_global::<FileOpenerCommand>() {
        Some(command) => command.0.argv(path, line, col),
        None => FileOpener::parse(opener::DEFAULT)
            .expect("the default opener is valid")
            .argv(path, line, col),
    }
}

#[cfg(test)]
pub mod fake {
    //! An [`Opener`] that records what it would run.

    use std::sync::Mutex;

    use super::*;

    /// Records each argv; fails instead while `failure` is set.
    #[derive(Default)]
    pub struct FakeSpawn {
        /// Every argv it was asked to start, in order.
        pub started: Mutex<Vec<Vec<String>>>,
        /// What the next spawns fail with.
        pub failure: Mutex<Option<String>>,
    }

    impl Spawn for FakeSpawn {
        fn spawn(&self, argv: &[String]) -> io::Result<()> {
            if let Some(why) = self.failure.lock().unwrap().clone() {
                return Err(io::Error::new(io::ErrorKind::NotFound, why));
            }
            self.started.lock().unwrap().push(argv.to_vec());
            Ok(())
        }
    }

    /// Installs a fake opener and hands it back.
    pub fn install(cx: &mut App) -> Arc<FakeSpawn> {
        let fake = Arc::new(FakeSpawn::default());
        cx.set_global(Opener(fake.clone()));
        fake
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::*;

    mod processes {
        use super::*;

        /// Waits up to 10 s for `path` to hold a whole line, and reads it.
        fn written(path: &Path) -> String {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(text) = std::fs::read_to_string(path)
                    && text.ends_with('\n')
                {
                    return text;
                }
                assert!(
                    Instant::now() < deadline,
                    "{} never written",
                    path.display()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        fn should_start_the_program_with_each_argument_as_given() {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out");
            let script = dir.path().join("record");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf '[%s]' \"$@\" > '{0}.tmp'; echo >> '{0}.tmp'; mv '{0}.tmp' '{0}'\n",
                    out.display()
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let argv = [
                script.display().to_string(),
                "-g".to_owned(),
                "/w/my notes/it's $(x).md:3".to_owned(),
            ];

            Processes.spawn(&argv).expect("the script starts");

            assert_eq!(written(&out), "[-g][/w/my notes/it's $(x).md:3]\n");
        }

        #[test]
        fn should_start_true() {
            Processes.spawn(&["true".to_owned()]).expect("true starts");
        }

        #[test]
        fn should_fail_for_a_program_that_does_not_exist() {
            let dir = tempfile::tempdir().unwrap();
            let missing = dir.path().join("missing").display().to_string();

            let err = Processes.spawn(&[missing]).unwrap_err();

            assert_eq!(err.kind(), io::ErrorKind::NotFound);
        }

        #[test]
        fn should_fail_for_an_empty_argv() {
            assert!(Processes.spawn(&[]).is_err());
        }
    }

    mod argv {
        use super::*;

        #[gpui::test]
        fn should_use_xdg_open_until_a_command_is_set(cx: &mut gpui::TestAppContext) {
            let argv = cx.update(|cx| argv("/w/a.rs", Some(3), None, cx));

            assert_eq!(argv, ["xdg-open", "/w/a.rs"]);
        }

        #[gpui::test]
        fn should_use_the_command_set(cx: &mut gpui::TestAppContext) {
            cx.update(|cx| set_file_opener("code -g {path}:{line}:{col}", cx));

            let argv = cx.update(|cx| argv("/w/a.rs", Some(3), None, cx));

            assert_eq!(argv, ["code", "-g", "/w/a.rs:3:1"]);
        }

        #[gpui::test]
        fn should_keep_the_last_command_for_one_that_is_not_an_opener(
            cx: &mut gpui::TestAppContext,
        ) {
            cx.update(|cx| set_file_opener("code {path}", cx));
            cx.update(|cx| set_file_opener("code", cx));

            let argv = cx.update(|cx| argv("/w/a.rs", None, None, cx));

            assert_eq!(argv, ["code", "/w/a.rs"]);
        }
    }
}
