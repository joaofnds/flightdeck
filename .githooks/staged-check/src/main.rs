use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

// Checks the tree the commit would hold, exported from the index, so that unstaged work
// in the shared working tree neither breaks a sound commit nor hides a broken one.
fn main() -> ExitCode {
    if let Err(error) = catch_interrupts() {
        eprintln!("check: cannot catch interrupts: {error}");
        return ExitCode::FAILURE;
    }
    let export = match create_export(&env::temp_dir(), process::id()) {
        Ok(export) => export,
        Err(error) => {
            eprintln!(
                "check: cannot create the export in {}: {error}",
                env::temp_dir().display()
            );
            return ExitCode::FAILURE;
        }
    };

    let index = env::var_os("GIT_INDEX_FILE");
    let mut outcome = check(&Check {
        repository: Path::new("."),
        index: index.as_deref(),
        export: &export,
        steps: &STEPS,
        interrupted: &INTERRUPTED,
    });
    let removal = fs::remove_dir_all(&export);
    // A signal after the last step's final poll would otherwise let the commit through.
    if INTERRUPTED.load(Ordering::Relaxed) {
        outcome = Err(Refusal::Interrupted);
    }

    // Both are reported, since a step that could not be stopped may be what keeps the
    // export from being removed.
    if let Err(refusal) = &outcome {
        eprintln!("check: refused, since {refusal}");
    }
    if let Err(error) = removal {
        eprintln!("check: cannot remove {}: {error}", export.display());
        return ExitCode::FAILURE;
    }
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// Creates an empty directory for the export, named for this process. Only an earlier hook
// that had this pid and was killed can have left a directory of that name, since no live
// process shares the pid, so it is removed first.
fn create_export(temp: &Path, pid: u32) -> io::Result<PathBuf> {
    // Cargo resolves a relative target directory from the export, git from the repository.
    if temp.is_relative() {
        return Err(io::Error::other(format!(
            "the temporary directory {} is relative",
            temp.display()
        )));
    }

    create_export_private(temp)?;

    let export = temp.join(format!("flightdeck-staged-{pid}"));
    if let Err(error) = fs::create_dir(&export) {
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
        fs::remove_dir_all(&export)?;
        fs::create_dir(&export)?;
    }
    assert!(export.is_absolute());
    assert!(export.starts_with(temp));
    Ok(export)
}

// Cargo reads a configuration from every directory above the one it runs in, and the
// configuration can name a program for cargo to run. So a directory above the export that
// another user can write, as /tmp is, would let them run code as the committer.
fn create_export_private(temp: &Path) -> io::Result<()> {
    let user = getuid();
    for directory in fs::canonicalize(temp)?.ancestors() {
        let metadata = fs::metadata(directory)?;
        let refusal = |reason: &str| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} {reason}, who could have cargo run a program as you",
                    directory.display()
                ),
            )
        };
        if metadata.mode() & WRITABLE_BY_OTHERS != 0 {
            return Err(refusal("is writable by other users"));
        }
        if ![ROOT, user].contains(&metadata.uid()) {
            return Err(refusal("belongs to another user"));
        }
    }
    Ok(())
}

fn check(check: &Check) -> Result<(), Refusal> {
    assert!(check.export.is_absolute());
    assert!(!check.steps.is_empty());

    let tree = check_tree(check)?;
    eprintln!(
        "check: exporting the staged tree {tree} to {}",
        check.export.display()
    );
    run(
        &mut check_export_command(check),
        GIT_LIMIT_MS,
        check.interrupted,
    )?;
    for step in check.steps {
        eprintln!("check: {} {}", step.program, step.arguments.join(" "));
        run(
            &mut check_step_command(step, check.export, env::vars_os()),
            step.limit_ms,
            check.interrupted,
        )?;
    }

    // Git reads the index again once the hook exits, so a file staged while the steps ran
    // would be committed unchecked. One staged after this comparison still gets through.
    if check_tree(check)? != tree {
        return Err(Refusal::IndexChanged);
    }
    Ok(())
}

// What one run of the check reads and runs.
struct Check<'a> {
    // Where git runs, the repository being committed.
    repository: &'a Path,
    // The index git names in GIT_INDEX_FILE, which `git commit -a` and `git commit <path>`
    // point at a temporary one. None leaves git to read the repository's own.
    index: Option<&'a OsStr>,
    export: &'a Path,
    steps: &'a [Step],
    interrupted: &'a AtomicBool,
}

// One command the commit must pass, and how long it may take. A cold check of the
// collector took 2 s on 2026-10-13, so each limit leaves wide room for a busy machine.
struct Step {
    program: &'static str,
    arguments: &'static [&'static str],
    limit_ms: u32,
}

// This package sits outside the workspace, so that an unstaged edit to the workspace
// cannot break it, and so the steps check it by its own manifest.
const HOOK_MANIFEST: &str = ".githooks/staged-check/Cargo.toml";
const WORKSPACE_MANIFEST: &str = "Cargo.toml";

const STEPS: [Step; 6] = [
    RUSTFMT_WORKSPACE,
    RUSTFMT_HOOK,
    CLIPPY_WORKSPACE,
    CLIPPY_HOOK,
    TESTS_WORKSPACE,
    TESTS_HOOK,
];

// Cargo's --quiet silences only its progress lines in clippy and test, but rustfmt's whole
// diff in fmt, so the rustfmt steps go without it.
const RUSTFMT_WORKSPACE: Step = Step {
    program: "cargo",
    arguments: &[
        "fmt",
        "--manifest-path",
        WORKSPACE_MANIFEST,
        "--all",
        "--check",
    ],
    limit_ms: 60_000,
};
const RUSTFMT_HOOK: Step = Step {
    program: "cargo",
    arguments: &["fmt", "--manifest-path", HOOK_MANIFEST, "--all", "--check"],
    limit_ms: 60_000,
};
const CLIPPY_WORKSPACE: Step = Step {
    program: "cargo",
    arguments: &[
        "clippy",
        "--manifest-path",
        WORKSPACE_MANIFEST,
        "--quiet",
        "--locked",
        "--workspace",
        "--all-targets",
        "--",
        "--deny",
        "warnings",
    ],
    limit_ms: 120_000,
};
const CLIPPY_HOOK: Step = Step {
    program: "cargo",
    arguments: &[
        "clippy",
        "--manifest-path",
        HOOK_MANIFEST,
        "--quiet",
        "--locked",
        "--workspace",
        "--all-targets",
        "--",
        "--deny",
        "warnings",
    ],
    limit_ms: 120_000,
};
const TESTS_WORKSPACE: Step = Step {
    program: "cargo",
    arguments: &[
        "test",
        "--manifest-path",
        WORKSPACE_MANIFEST,
        "--quiet",
        "--locked",
        "--workspace",
    ],
    limit_ms: 120_000,
};
const TESTS_HOOK: Step = Step {
    program: "cargo",
    arguments: &[
        "test",
        "--manifest-path",
        HOOK_MANIFEST,
        "--quiet",
        "--locked",
        "--workspace",
    ],
    limit_ms: 120_000,
};

// The id of the tree the index holds, which names the content the commit would hold.
fn check_tree(check: &Check) -> Result<String, Refusal> {
    let mut command = check_git_command(check);
    command.arg("write-tree").stdout(Stdio::piped());
    let mut child = run_spawn(&mut command)?;
    // The id fits in the pipe's buffer, so git exits without waiting for it to be read.
    run_wait(&mut child, GIT_LIMIT_MS, check.interrupted)?;

    let Some(stdout) = child.stdout.take() else {
        unreachable!("the command's output is piped");
    };
    let printed = io::read_to_string(stdout).map_err(Refusal::Unreadable)?;
    let unreadable = || {
        Refusal::Unreadable(io::Error::other(format!(
            "git write-tree printed {printed:?}"
        )))
    };
    let tree = printed.trim_end();
    // A SHA-1 id has 40 hexadecimal digits and a SHA-256 id has 64. Two empty outputs
    // would compare equal and let any change through.
    if !matches!(tree.len(), 40 | 64) {
        return Err(unreadable());
    }
    if !tree.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(unreadable());
    }
    Ok(tree.to_owned())
}

// Exports the index, the tree the commit would hold, into an empty directory.
fn check_export_command(check: &Check) -> Command {
    assert!(check.export.is_absolute());

    // git checkout-index puts the prefix before each path as it is, so without the closing
    // slash it would write files beside the export rather than into it.
    let mut prefix = OsString::from("--prefix=");
    prefix.push(check.export);
    prefix.push("/");
    assert!(prefix.as_encoded_bytes().starts_with(b"--prefix=/"));
    assert!(prefix.as_encoded_bytes().ends_with(b"/"));
    let mut command = check_git_command(check);
    command
        .args(["checkout-index", "--all"])
        .arg(prefix)
        .stdout(Stdio::inherit());
    command
}

// Git in the repository being committed, reading the index git names.
fn check_git_command(check: &Check) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(check.repository)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    match check.index {
        Some(index) => command.env("GIT_INDEX_FILE", index),
        None => command.env_remove("GIT_INDEX_FILE"),
    };
    command
}

// A step gets the hook's environment without git's variables, so a test that runs git
// cannot reach the repository being committed. It reads no input, so a test that waits on
// stdin fails rather than hangs.
fn check_step_command(
    step: &Step,
    export: &Path,
    environment: impl Iterator<Item = (OsString, OsString)>,
) -> Command {
    assert!(!step.program.is_empty());
    assert!(export.is_absolute());

    let mut command = Command::new(step.program);
    command
        .args(step.arguments)
        .current_dir(export)
        .env_clear()
        .envs(environment.filter(|(name, _)| !name.as_encoded_bytes().starts_with(b"GIT_")))
        .env("CARGO_TARGET_DIR", export.join("target"))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    assert_eq!(command.get_current_dir(), Some(export));
    command
}

fn run(command: &mut Command, limit_ms: u32, interrupted: &AtomicBool) -> Result<(), Refusal> {
    let mut child = run_spawn(command)?;
    run_wait(&mut child, limit_ms, interrupted)
}

// The command leads a process group of its own, so stopping it reaches what it started.
fn run_spawn(command: &mut Command) -> Result<Child, Refusal> {
    command
        .process_group(0)
        .spawn()
        .map_err(Refusal::Unstartable)
}

fn run_wait(child: &mut Child, limit_ms: u32, interrupted: &AtomicBool) -> Result<(), Refusal> {
    assert!(limit_ms > 0);
    assert!(limit_ms.is_multiple_of(POLL_MS));

    // The standard library has no wait with a time limit, so the loop polls.
    for _ in 0..limit_ms / POLL_MS {
        let exited = match child.try_wait() {
            Ok(exited) => exited,
            Err(error) => {
                run_stop(child)?;
                return Err(Refusal::Unwaitable(error));
            }
        };
        if let Some(status) = exited {
            return if status.success() {
                Ok(())
            } else {
                Err(Refusal::Failed(status))
            };
        }
        if interrupted.load(Ordering::Relaxed) {
            run_stop(child)?;
            return Err(Refusal::Interrupted);
        }
        thread::sleep(Duration::from_millis(u64::from(POLL_MS)));
    }

    run_stop(child)?;
    Err(Refusal::Stopped { limit_ms })
}

fn run_stop(child: &mut Child) -> Result<(), Refusal> {
    let group =
        i32::try_from(child.id()).map_err(|error| Refusal::Unstoppable(io::Error::other(error)))?;
    // Zero or a negative number would make kill(2) signal the hook's own group, git's too.
    if group <= 0 {
        return Err(Refusal::Unstoppable(io::Error::other(format!(
            "the step's pid is {group}"
        ))));
    }

    if kill(-group, SIGNAL_KILL) != 0 {
        let error = io::Error::last_os_error();
        // macOS refuses to signal a group whose every process has exited, with EPERM when
        // the leader is not yet reaped, so a step that has exited needs no stopping.
        if child.try_wait().map_err(Refusal::Unwaitable)?.is_none() {
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
    Unreadable(io::Error),
    Interrupted,
    IndexChanged,
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
            Refusal::Unstoppable(error) => write!(formatter, "it could not be stopped: {error}"),
            Refusal::Unreadable(error) => write!(formatter, "its output was unreadable: {error}"),
            Refusal::Interrupted => write!(formatter, "it was interrupted and stopped"),
            Refusal::IndexChanged => write!(
                formatter,
                "files were staged while it ran, so it did not check what git would commit"
            ),
        }
    }
}

// Set by a signal that would end the hook, Ctrl-C and Ctrl-\ above all, so that the hook
// stops the step running, whose process group the terminal's signal does not reach, and
// removes its export before it exits.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn catch_interrupts_callback(_signal: i32) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

fn catch_interrupts() -> io::Result<()> {
    assert!(!INTERRUPTED.load(Ordering::Relaxed));

    for number in [
        SIGNAL_HANGUP,
        SIGNAL_INTERRUPT,
        SIGNAL_QUIT,
        SIGNAL_TERMINATE,
    ] {
        // SAFETY: the callback only stores to an atomic, which a signal handler may do.
        let previous = unsafe { signal(number, catch_interrupts_callback) };
        if previous == SIGNAL_ERROR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

unsafe extern "C" {
    // getuid(2), which cannot fail.
    safe fn getuid() -> u32;
    // kill(2). A negative pid names a process group.
    safe fn kill(pid: i32, signal: i32) -> i32;
    // signal(3), which returns the previous handler as a pointer, or SIG_ERR.
    fn signal(signal: i32, handler: extern "C" fn(i32)) -> isize;
}

// From <sys/signal.h>.
const SIGNAL_HANGUP: i32 = 1;
const SIGNAL_INTERRUPT: i32 = 2;
const SIGNAL_QUIT: i32 = 3;
const SIGNAL_KILL: i32 = 9;
const SIGNAL_TERMINATE: i32 = 15;
const SIGNAL_ERROR: isize = -1;
// From <sys/stat.h>, S_IWGRP and S_IWOTH.
const WRITABLE_BY_OTHERS: u32 = 0o022;
const ROOT: u32 = 0;
const POLL_MS: u32 = 10;
// Exporting this repository's index took 10 ms on 2026-10-13, and writing its tree 7 ms.
const GIT_LIMIT_MS: u32 = 30_000;

// `run` polls a limit in whole intervals, so each limit is a positive multiple of one.
const _: () = {
    assert!(GIT_LIMIT_MS > 0);
    assert!(GIT_LIMIT_MS.is_multiple_of(POLL_MS));
    assert!(!STEPS.is_empty());
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
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    // Goal: a step whose command succeeds lets the commit through.
    // Method: run `true` as a step in the current directory.
    #[test]
    fn passes_a_step_that_exits_zero() {
        let outcome = run(&mut Command::new("true"), 1_000, &NEVER_INTERRUPTED);

        assert!(outcome.is_ok(), "{outcome:?}");
    }

    // Goal: a step whose command fails refuses the commit with the command's status.
    // Method: run `false`, which exits 1.
    #[test]
    fn refuses_a_step_that_exits_non_zero() {
        let outcome = run(&mut Command::new("false"), 1_000, &NEVER_INTERRUPTED);

        assert!(
            matches!(outcome, Err(Refusal::Failed(status)) if status.code() == Some(1)),
            "{outcome:?}"
        );
    }

    // Goal: a step whose program is missing refuses the commit rather than passing it.
    // Method: run a program path that does not exist.
    #[test]
    fn refuses_a_step_it_cannot_start() {
        let outcome = run(
            &mut Command::new("/nonexistent/program"),
            1_000,
            &NEVER_INTERRUPTED,
        );

        assert!(
            matches!(outcome, Err(Refusal::Unstartable(_))),
            "{outcome:?}"
        );
    }

    // Goal: an interrupt, such as Ctrl-C in the terminal running `git commit`, stops the
    // step running, since the step leads a process group the terminal's signal misses.
    // Method: run a 30 s `sleep` and set the interrupt flag from another thread at 100 ms.
    #[test]
    fn stops_a_step_when_interrupted() {
        let interrupted = AtomicBool::new(false);
        let started = Instant::now();

        let outcome = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                interrupted.store(true, Ordering::Relaxed);
            });
            run(Command::new("sleep").arg("30"), 60_000, &interrupted)
        });

        assert!(matches!(outcome, Err(Refusal::Interrupted)), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    // Goal: each signal that would end the hook, Ctrl-C and Ctrl-\ among them, sets the
    // interrupt flag instead, so the hook can stop its step and remove its export.
    // Method: catch the signals, send this process each one in turn, and wait for the
    // flag, clearing it after each. One test sends them all, since the handlers and the
    // flag belong to the whole process.
    #[test]
    fn turns_each_signal_that_would_end_the_hook_into_an_interrupt() {
        catch_interrupts().unwrap();
        let hook = i32::try_from(process::id()).unwrap();

        for number in [
            SIGNAL_HANGUP,
            SIGNAL_INTERRUPT,
            SIGNAL_QUIT,
            SIGNAL_TERMINATE,
        ] {
            let sent = kill(hook, number);

            assert_eq!(sent, 0, "signal {number}");
            assert!(
                interrupted_within(Duration::from_secs(2)),
                "signal {number}"
            );
            INTERRUPTED.store(false, Ordering::Relaxed);
        }
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

        let outcome = run(&mut command, 200, &NEVER_INTERRUPTED);

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

    // Goal: a step that exits on its own just as its limit or an interrupt stops it is
    // stopped, not reported as a step that could not be stopped.
    // Method: start `true` in a process group of its own, let it exit unreaped, and stop it.
    #[test]
    fn stops_a_step_that_has_already_exited() {
        let mut child = Command::new("true").process_group(0).spawn().unwrap();
        thread::sleep(Duration::from_millis(200));

        let outcome = run_stop(&mut child);

        assert!(outcome.is_ok(), "{outcome:?}");
    }

    // Goal: a hook killed before it removed its export does not refuse the commits of a
    // later hook that gets the same pid.
    // Method: leave a directory holding a file where the export goes, then create it.
    #[test]
    fn creates_an_empty_export_over_one_a_killed_hook_left() {
        let scratch = Scratch::new("creates_an_empty_export");
        let leftover = scratch.path.join("flightdeck-staged-7");
        fs::create_dir(&leftover).unwrap();
        fs::write(leftover.join("file"), "left\n").unwrap();

        let export = create_export(&scratch.path, 7).unwrap();

        assert_eq!(export, leftover);
        assert_eq!(fs::read_dir(&export).unwrap().count(), 0);
    }

    // Goal: the first step that fails refuses the commit, and no step after it runs.
    // Method: run a check whose steps are `false` and then a `touch` of a file in the
    // export, and look for the file.
    #[test]
    fn refuses_a_commit_at_the_first_step_that_fails() {
        let scratch = Scratch::new("refuses_a_commit");
        let repository = scratch.repository("staged\n");
        let export = scratch.export();
        let touching = Step {
            program: "touch",
            arguments: &["touched"],
            limit_ms: 1_000,
        };

        let outcome = check(&Check {
            repository: &repository,
            index: None,
            export: &export,
            steps: &[FAILING, touching],
            interrupted: &NEVER_INTERRUPTED,
        });

        assert!(
            matches!(outcome, Err(Refusal::Failed(status)) if status.code() == Some(1)),
            "{outcome:?}"
        );
        assert!(!export.join("touched").exists());
    }

    // Goal: a file staged while the check runs, which git would commit unchecked since it
    // reads the index again after the hook, refuses the commit.
    // Method: run a check whose one step stages a second file in the scratch repository.
    #[test]
    fn refuses_a_commit_whose_index_changed_while_it_ran() {
        let scratch = Scratch::new("refuses_a_commit_whose_index");
        let repository = scratch.repository("staged\n");
        fs::write(repository.join("late"), "late\n").unwrap();
        let export = scratch.export();
        let staging = Step {
            program: "git",
            arguments: &["-C", "../repository", "add", "late"],
            limit_ms: 10_000,
        };

        let outcome = check(&Check {
            repository: &repository,
            index: None,
            export: &export,
            steps: &[staging],
            interrupted: &NEVER_INTERRUPTED,
        });

        assert!(matches!(outcome, Err(Refusal::IndexChanged)), "{outcome:?}");
    }

    // Goal: the check exports the index git names in GIT_INDEX_FILE, which `git commit -a`
    // and `git commit <path>` point at a temporary one, rather than the repository's own.
    // Method: stage one version of a file in the repository's index and another in a
    // second index, run the check naming the second, and read the exported copy.
    #[test]
    fn exports_the_index_git_names() {
        let scratch = Scratch::new("exports_the_index");
        let repository = scratch.repository("own index\n");
        let other = repository.join(".git/other-index");
        fs::write(repository.join("file"), "other index\n").unwrap();
        let status = git_command(&repository)
            .env("GIT_INDEX_FILE", &other)
            .args(["add", "file"])
            .status()
            .unwrap();
        assert!(status.success());
        let export = scratch.export();

        let outcome = check(&Check {
            repository: &repository,
            index: Some(other.as_os_str()),
            export: &export,
            steps: &[PASSING],
            interrupted: &NEVER_INTERRUPTED,
        });

        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(
            fs::read_to_string(export.join("file")).unwrap(),
            "other index\n"
        );
    }

    // Goal: no other user can run code as the committer through a cargo configuration
    // above the export, which cargo reads from every directory above the one it runs in.
    // Method: make a scratch directory that every user can write, as /tmp is, and create
    // the export in it.
    #[test]
    fn refuses_an_export_under_a_directory_other_users_can_write() {
        let scratch = Scratch::new("refuses_an_export_under");
        let shared = scratch.path.join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();

        let outcome = create_export(&shared, 7);

        assert_eq!(
            outcome.map_err(|error| error.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
        assert!(!shared.join("flightdeck-staged-7").exists());
    }

    // Goal: the check reads what the commit would hold, never the working tree.
    // Method: in a scratch repository, stage one version of a file, change it again
    // without staging, run the check, and read the exported copy.
    #[test]
    fn exports_the_staged_content_and_not_the_working_tree() {
        let scratch = Scratch::new("exports");
        let repository = scratch.repository("staged\n");
        fs::write(repository.join("file"), "unstaged\n").unwrap();
        let export = scratch.export();

        let outcome = check(&Check {
            repository: &repository,
            index: None,
            export: &export,
            steps: &[PASSING],
            interrupted: &NEVER_INTERRUPTED,
        });

        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(fs::read_to_string(export.join("file")).unwrap(), "staged\n");
    }

    // Goal: no step can reach the repository being committed through the variables git
    // hands the hook, such as a test that runs git writing to the real index.
    // Method: run `env` as a step, given an environment holding git's variables and others,
    // and read the names of the variables it printed.
    #[test]
    fn runs_steps_without_the_variables_git_hands_the_hook() {
        let scratch = Scratch::new("runs_steps_without");
        let environment = [
            ("GIT_CONFIG_PARAMETERS", "'core.hooksPath'='elsewhere'"),
            ("GIT_DIR", "/repository/.git"),
            ("GIT_INDEX_FILE", "/repository/.git/index.lock"),
            ("PATH", "/usr/bin:/bin"),
            ("STEP_PROBE", "kept"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        let printing = Step {
            program: "env",
            arguments: &[],
            limit_ms: 1_000,
        };

        let output = check_step_command(&printing, &scratch.path, environment.into_iter())
            .stdout(Stdio::piped())
            .output()
            .unwrap();

        let printed = String::from_utf8(output.stdout).unwrap();
        let mut names: Vec<_> = printed
            .lines()
            .map(|line| line.split_once('=').unwrap().0)
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["CARGO_TARGET_DIR", "PATH", "STEP_PROBE"]);
    }

    // Goal: rustfmt, clippy and the tests each check the workspace and then the hook's own
    // package, which sits outside the workspace.
    // Method: read the tool and the manifest each step names, in order.
    #[test]
    fn runs_each_tool_over_the_workspace_and_then_the_hook_package() {
        let runs: Vec<_> = STEPS
            .iter()
            .map(|step| (step.arguments[0], step.arguments[1], step.arguments[2]))
            .collect();

        assert_eq!(
            runs,
            [
                ("fmt", "--manifest-path", WORKSPACE_MANIFEST),
                ("fmt", "--manifest-path", HOOK_MANIFEST),
                ("clippy", "--manifest-path", WORKSPACE_MANIFEST),
                ("clippy", "--manifest-path", HOOK_MANIFEST),
                ("test", "--manifest-path", WORKSPACE_MANIFEST),
                ("test", "--manifest-path", HOOK_MANIFEST),
            ]
        );
    }

    // Goal: rustfmt refusing a file of the workspace refuses the commit and shows the diff
    // it wants, so the refusal says what to fix.
    // Method: lay out an export whose workspace holds a misformatted file and whose hook
    // package is formatted, run the workspace's rustfmt step there, and read its output.
    #[test]
    fn shows_the_diff_rustfmt_wants_in_the_workspace() {
        let scratch = Scratch::new("shows_the_diff_rustfmt_wants_in_the_workspace");
        lay_out_packages(&scratch.path, &scratch.path);

        let output = check_step_command(&RUSTFMT_WORKSPACE, &scratch.path, env::vars_os())
            .stdout(Stdio::piped())
            .output()
            .unwrap();

        assert!(!output.status.success());
        let printed = String::from_utf8_lossy(&output.stdout);
        assert!(printed.contains("Diff in"), "{printed:?}");
    }

    // Goal: rustfmt refusing a file of the hook's own package refuses the commit and shows
    // the diff it wants, though the package sits outside the workspace.
    // Method: lay out an export whose hook package holds a misformatted file and whose
    // workspace is formatted, run the hook's rustfmt step there, and read its output.
    #[test]
    fn shows_the_diff_rustfmt_wants_in_the_hook_package() {
        let scratch = Scratch::new("shows_the_diff_rustfmt_wants_in_the_hook_package");
        let hook_package = scratch.path.join(".githooks/staged-check");
        lay_out_packages(&scratch.path, &hook_package);

        let output = check_step_command(&RUSTFMT_HOOK, &scratch.path, env::vars_os())
            .stdout(Stdio::piped())
            .output()
            .unwrap();

        assert!(!output.status.success());
        let printed = String::from_utf8_lossy(&output.stdout);
        assert!(printed.contains("Diff in"), "{printed:?}");
    }

    // Goal: the hook's package holds itself to the workspace's lints, which it cannot
    // inherit, since it is a workspace of its own.
    // Method: read the lint tables of both manifests, each line under its table's name.
    #[test]
    fn lints_itself_as_the_workspace_does() {
        let workspace = include_str!("../../../Cargo.toml");
        let hook = include_str!("../Cargo.toml");

        assert_eq!(
            lint_lines(hook, "[lints."),
            lint_lines(workspace, "[workspace.lints.")
        );
    }

    static NEVER_INTERRUPTED: AtomicBool = AtomicBool::new(false);
    const PASSING: Step = Step {
        program: "true",
        arguments: &[],
        limit_ms: 1_000,
    };
    const FAILING: Step = Step {
        program: "false",
        arguments: &[],
        limit_ms: 1_000,
    };

    // A directory of its own for one test, removed when the test ends, pass or fail.
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(test: &str) -> Scratch {
            let path = env::temp_dir().join(format!("staged-check-{}-{test}", process::id()));
            fs::create_dir(&path).unwrap();
            Scratch { path }
        }

        // A repository whose index holds one file, `file`, with the content given.
        fn repository(&self, staged: &str) -> PathBuf {
            let repository = self.path.join("repository");
            fs::create_dir(&repository).unwrap();
            git(&repository, &["init", "--quiet"]);
            fs::write(repository.join("file"), staged).unwrap();
            git(&repository, &["add", "file"]);
            repository
        }

        fn export(&self) -> PathBuf {
            let export = self.path.join("export");
            fs::create_dir(&export).unwrap();
            export
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }

    // Lays out an export holding a workspace and the hook's package, each a binary of one
    // file, where the file of the package at `misformatted` is the only one rustfmt refuses.
    fn lay_out_packages(export: &Path, misformatted: &Path) {
        let hook_package = export.join(".githooks/staged-check");
        for package in [export, hook_package.as_path()] {
            fs::create_dir_all(package.join("src")).unwrap();
            let manifest = "[package]\nname = \"probe\"\nedition = \"2024\"\n\n[workspace]\n";
            fs::write(package.join("Cargo.toml"), manifest).unwrap();
            fs::write(package.join("src/main.rs"), "fn main() {}\n").unwrap();
        }
        fs::write(misformatted.join("src/main.rs"), "fn   main( ) {}\n").unwrap();
    }

    // Each line of the tables whose header starts with `prefix`, after the table's name
    // with the prefix taken off, so `[lints.clippy]` and `[workspace.lints.clippy]` match.
    fn lint_lines(manifest: &str, prefix: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut table = None;
        for line in manifest.lines() {
            if line.starts_with('[') {
                table = line
                    .strip_prefix(prefix)
                    .and_then(|name| name.strip_suffix(']'));
                continue;
            }
            let Some(name) = table else {
                continue;
            };
            if !line.is_empty() {
                lines.push(format!("{name}: {line}"));
            }
        }
        lines
    }

    // A signal arrives asynchronously, so the test polls for the flag it sets.
    fn interrupted_within(wait: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < wait {
            if INTERRUPTED.load(Ordering::Relaxed) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
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
        let status = git_command(repository).args(arguments).status().unwrap();
        assert!(status.success());
    }

    fn git_command(repository: &Path) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(repository)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE");
        command
    }
}
