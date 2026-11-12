//! `cargo xtask ci`: the chain CI runs, one step per CI job, in one local command.

use std::process::{Command, ExitCode};

/// One CI job's command: `cargo <args>`, with `env` set.
struct Step {
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
}

/// The jobs in `.github/workflows/ci.yml`, in its order. A unit test keeps the two in step.
const STEPS: &[Step] = &[
    Step {
        args: &["fmt", "--all", "--check"],
        env: &[],
    },
    Step {
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        env: &[],
    },
    Step {
        args: &["test-unit-ci"],
        env: &[],
    },
    Step {
        args: &["test-integration-ci"],
        env: &[],
    },
    Step {
        args: &["doc", "--workspace", "--no-deps", "--locked"],
        env: &[("RUSTDOCFLAGS", "-D warnings")],
    },
];

/// Runs every step in order and stops at the first that fails, exiting with its code.
pub fn run() -> ExitCode {
    for step in STEPS {
        let mut command = Command::new(crate::cargo());
        command.args(step.args).envs(step.env.iter().copied());
        eprintln!("xtask ci: cargo {}", step.args.join(" "));
        let code = crate::status(command);
        if code != 0 {
            eprintln!("xtask ci: `cargo {}` failed", step.args.join(" "));
            return ExitCode::from(u8::try_from(code).unwrap_or(1));
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKFLOW: &str = include_str!("../../../.github/workflows/ci.yml");

    fn workflow_commands() -> Vec<String> {
        WORKFLOW
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- run: cargo "))
            .map(str::to_owned)
            .collect()
    }

    mod steps {
        use super::*;

        #[test]
        fn should_match_the_ci_workflows_cargo_commands_in_order() {
            let steps: Vec<String> = STEPS.iter().map(|step| step.args.join(" ")).collect();
            assert_eq!(steps, workflow_commands());
        }

        #[test]
        fn should_deny_rustdoc_warnings_as_the_doc_job_does() {
            assert!(WORKFLOW.contains("RUSTDOCFLAGS: -D warnings"));
            let doc = STEPS
                .iter()
                .find(|step| step.args.first() == Some(&"doc"))
                .expect("a doc step");
            assert_eq!(doc.env, &[("RUSTDOCFLAGS", "-D warnings")]);
        }
    }
}
