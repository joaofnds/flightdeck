use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{self, Child, Command, ExitCode, ExitStatus};
use std::thread;
use std::time::Duration;

// Checks the tree the commit would hold, exported from the index, so that unstaged work
// in the shared working tree neither breaks a sound commit nor hides a broken one.
fn main() -> ExitCode {
    let export = env::temp_dir().join(format!("flightdeck-staged-{}", process::id()));
    if let Err(error) = fs::create_dir(&export) {
        eprintln!("check: cannot create {}: {error}", export.display());
        return ExitCode::FAILURE;
    }

    let outcome = check(&export);
    let removal = fs::remove_dir_all(&export);

    if let Err(error) = removal {
        eprintln!("check: cannot remove {}: {error}", export.display());
        return ExitCode::FAILURE;
    }
    assert!(!export.exists());
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("check: refused, since {refusal}");
            ExitCode::FAILURE
        }
    }
}

fn check(export: &Path) -> Result<(), Refusal> {
    assert!(export.is_dir());

    eprintln!("check: exporting the staged tree to {}", export.display());
    export_index(Path::new("."), export)?;
    for step in &STEPS {
        eprintln!("check: cargo {}", step.arguments.join(" "));
        run(
            &mut step_command(step, export, env::vars_os()),
            step.limit_ms,
        )?;
    }
    Ok(())
}

// One cargo command the commit must pass, and how long it may take. A cold check of the
// collector took 2 s on 2026-10-13, so each limit leaves wide room for a busy machine.
struct Step {
    arguments: &'static [&'static str],
    limit_ms: u32,
}

// This package sits outside the workspace, so that an unstaged edit to the workspace
// cannot break it, and so the steps check it by its own manifest.
const HOOK_MANIFEST: &str = ".githooks/staged-check/Cargo.toml";

const STEPS: [Step; 6] = [
    Step {
        arguments: &["fmt", "--all", "--check"],
        limit_ms: 60_000,
    },
    Step {
        arguments: &["fmt", "--check", "--manifest-path", HOOK_MANIFEST],
        limit_ms: 60_000,
    },
    Step {
        arguments: &[
            "clippy",
            "--quiet",
            "--locked",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        limit_ms: 120_000,
    },
    Step {
        arguments: &[
            "clippy",
            "--quiet",
            "--locked",
            "--all-targets",
            "--manifest-path",
            HOOK_MANIFEST,
            "--",
            "-D",
            "warnings",
        ],
        limit_ms: 120_000,
    },
    Step {
        arguments: &["test", "--quiet", "--locked"],
        limit_ms: 120_000,
    },
    Step {
        arguments: &[
            "test",
            "--quiet",
            "--locked",
            "--manifest-path",
            HOOK_MANIFEST,
        ],
        limit_ms: 120_000,
    },
];

fn step_command(
    step: &Step,
    export: &Path,
    environment: impl Iterator<Item = (OsString, OsString)>,
) -> Command {
    assert!(!step.arguments.is_empty());

    let mut command = Command::new("cargo");
    command
        .args(step.arguments)
        .current_dir(export)
        .env("CARGO_TARGET_DIR", export.join("target"));
    for (name, _) in environment {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            command.env_remove(name);
        }
    }
    assert_eq!(command.get_current_dir(), Some(export));
    command
}

// Exports the index, the tree the commit would hold, into an empty directory.
fn export_index(repository: &Path, export: &Path) -> Result<(), Refusal> {
    assert!(export.is_dir());
    assert!(repository.is_dir());

    let mut prefix = OsString::from("--prefix=");
    prefix.push(export);
    prefix.push("/");
    let mut command = Command::new("git");
    command
        .args(["checkout-index", "--all"])
        .arg(prefix)
        .current_dir(repository);
    run(&mut command, EXPORT_LIMIT_MS)
}

fn run(command: &mut Command, limit_ms: u32) -> Result<(), Refusal> {
    assert!(limit_ms > 0);
    assert!(limit_ms.is_multiple_of(POLL_MS));

    // The command leads a process group of its own, so stopping it reaches what it started.
    let mut child = command
        .process_group(0)
        .spawn()
        .map_err(Refusal::Unstartable)?;

    for _ in 0..limit_ms / POLL_MS {
        let exited = child.try_wait().map_err(Refusal::Unwaitable)?;
        if let Some(status) = exited {
            return if status.success() {
                Ok(())
            } else {
                Err(Refusal::Failed(status))
            };
        }
        thread::sleep(Duration::from_millis(u64::from(POLL_MS)));
    }

    stop(&mut child)?;
    Err(Refusal::Stopped { limit_ms })
}

fn stop(child: &mut Child) -> Result<(), Refusal> {
    let group =
        i32::try_from(child.id()).map_err(|error| Refusal::Unstoppable(io::Error::other(error)))?;
    assert!(group > 0);

    if kill(-group, SIGNAL_KILL) != 0 {
        let error = io::Error::last_os_error();
        // The group is already gone when every process in it has exited on its own.
        if error.raw_os_error() != Some(NO_SUCH_PROCESS) {
            return Err(Refusal::Unstoppable(error));
        }
    }

    child.wait().map_err(Refusal::Unwaitable)?;
    Ok(())
}

#[derive(Debug)]
enum Refusal {
    Unstartable(io::Error),
    Unwaitable(io::Error),
    Failed(ExitStatus),
    Stopped { limit_ms: u32 },
    Unstoppable(io::Error),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Refusal::Unstartable(error) => write!(formatter, "it could not start: {error}"),
            Refusal::Unwaitable(error) => write!(formatter, "waiting for it failed: {error}"),
            Refusal::Failed(status) => write!(formatter, "it failed with {status}"),
            Refusal::Stopped { limit_ms } => {
                write!(
                    formatter,
                    "it ran past its limit of {limit_ms} ms and was stopped"
                )
            }
            Refusal::Unstoppable(error) => {
                write!(
                    formatter,
                    "it ran past its limit and could not be stopped: {error}"
                )
            }
        }
    }
}

unsafe extern "C" {
    // kill(2). A negative pid names a process group.
    safe fn kill(pid: i32, signal: i32) -> i32;
}

// SIGKILL in <sys/signal.h> and ESRCH in <sys/errno.h>.
const SIGNAL_KILL: i32 = 9;
const NO_SUCH_PROCESS: i32 = 3;
const POLL_MS: u32 = 10;
const EXPORT_LIMIT_MS: u32 = 30_000;

// `run` polls a limit in whole intervals, so each limit is a positive multiple of one.
const _: () = {
    assert!(EXPORT_LIMIT_MS > 0);
    assert!(EXPORT_LIMIT_MS.is_multiple_of(POLL_MS));
    let mut index = 0;
    while index < STEPS.len() {
        assert!(STEPS[index].limit_ms > 0);
        assert!(STEPS[index].limit_ms.is_multiple_of(POLL_MS));
        index += 1;
    }
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::time::Instant;

    // Goal: a step whose command succeeds lets the commit through.
    // Method: run `true` as a step in the current directory.
    #[test]
    fn passes_a_step_that_exits_zero() {
        let outcome = run(&mut Command::new("true"), 1_000);

        assert!(outcome.is_ok(), "{outcome:?}");
    }

    // Goal: a step whose command fails refuses the commit with the command's status.
    // Method: run `false`, which exits 1.
    #[test]
    fn refuses_a_step_that_exits_non_zero() {
        let outcome = run(&mut Command::new("false"), 1_000);

        assert!(
            matches!(outcome, Err(Refusal::Failed(status)) if status.code() == Some(1)),
            "{outcome:?}"
        );
    }

    // Goal: a step whose program is missing refuses the commit rather than passing it.
    // Method: run a program path that does not exist.
    #[test]
    fn refuses_a_step_it_cannot_start() {
        let outcome = run(&mut Command::new("/nonexistent/program"), 1_000);

        assert!(
            matches!(outcome, Err(Refusal::Unstartable(_))),
            "{outcome:?}"
        );
    }

    // Goal: a step that runs past its limit refuses the commit, and nothing it started
    // outlives it, since a test binary that hangs is a grandchild of the cargo we start.
    // Method: run a shell that starts a background `sleep` and waits on it, give the step
    // a 200 ms limit, then look for the background process by the pid it wrote down.
    #[test]
    fn stops_a_step_and_its_children_past_its_limit() {
        let scratch = Scratch::new("stops_a_step");
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo $! > grandchild; wait"])
            .current_dir(&scratch.path);
        let started = Instant::now();

        let outcome = run(&mut command, 200);

        assert!(
            matches!(outcome, Err(Refusal::Stopped { limit_ms: 200 })),
            "{outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let grandchild: i32 = fs::read_to_string(scratch.path.join("grandchild"))
            .map(|text| text.trim().parse().unwrap())
            .unwrap();
        assert!(
            gone_within(grandchild, Duration::from_secs(2)),
            "pid {grandchild} survived"
        );
    }

    // Goal: the check reads what the commit would hold, never the working tree.
    // Method: in a scratch repository, stage one version of a file, change it again
    // without staging, export, and read the exported copy.
    #[test]
    fn exports_the_staged_content_and_not_the_working_tree() {
        let scratch = Scratch::new("exports");
        let repository = scratch.path.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        fs::write(repository.join("file"), "staged\n").unwrap();
        git(&repository, &["add", "file"]);
        fs::write(repository.join("file"), "unstaged\n").unwrap();
        let export = scratch.path.join("export");
        fs::create_dir(&export).unwrap();

        let outcome = export_index(&repository, &export);

        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(fs::read_to_string(export.join("file")).unwrap(), "staged\n");
    }

    // Goal: no step can reach the repository being committed through the variables git
    // hands the hook, such as a test that runs git writing to the real index.
    // Method: build a step's command from an environment holding git's and other
    // variables, and read which ones the command removes.
    #[test]
    fn runs_steps_without_the_variables_git_hands_the_hook() {
        let environment = [
            (
                OsString::from("GIT_INDEX_FILE"),
                OsString::from(".git/index.lock"),
            ),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
        ];

        let command = step_command(&STEPS[0], Path::new("/export"), environment.into_iter());

        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .collect();
        assert_eq!(removed, [(OsStr::new("GIT_INDEX_FILE"), None)]);
    }

    // Goal: a refused rustfmt step shows the diff it wants, so the refusal says what to fix.
    // Method: lay out an export holding a misformatted workspace and a misformatted package
    // at this package's path, run each rustfmt step there, and read what it printed.
    #[test]
    fn shows_the_diff_of_a_rustfmt_step_that_refuses() {
        let scratch = Scratch::new("shows_the_diff");
        let workspace = scratch.path.clone();
        let hook_package = scratch.path.join(".githooks/staged-check");
        for package in [&workspace, &hook_package] {
            fs::create_dir_all(package.join("src")).unwrap();
            let manifest = "[package]\nname = \"probe\"\nedition = \"2024\"\n\n[workspace]\n";
            fs::write(package.join("Cargo.toml"), manifest).unwrap();
            fs::write(package.join("src/main.rs"), "fn   main( ) {}\n").unwrap();
        }
        let rustfmt_steps: Vec<_> = STEPS
            .iter()
            .filter(|step| step.arguments[0] == "fmt")
            .collect();
        assert_eq!(rustfmt_steps.len(), 2);

        for step in rustfmt_steps {
            let output = step_command(step, &scratch.path, env::vars_os())
                .output()
                .unwrap();

            assert!(!output.status.success(), "{:?}", step.arguments);
            let printed = String::from_utf8_lossy(&output.stdout);
            assert!(
                printed.contains("Diff in"),
                "{:?} printed {printed:?}",
                step.arguments
            );
        }
    }

    // A directory of its own for one test, removed when the test ends, pass or fail.
    struct Scratch {
        path: std::path::PathBuf,
    }

    impl Scratch {
        fn new(test: &str) -> Scratch {
            let path = env::temp_dir().join(format!("staged-check-{}-{test}", process::id()));
            fs::create_dir(&path).unwrap();
            Scratch { path }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }

    // A killed process can stay a zombie until launchd reaps it, so the probe polls.
    fn gone_within(pid: i32, wait: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < wait {
            if kill(pid, 0) != 0 {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    // The test's git must not see the variables git hands a hook, or it would write to the
    // index of the repository being committed instead of the scratch one.
    fn git(repository: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(repository)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
