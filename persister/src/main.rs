use std::ffi::c_int;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(error) = persistence::engine::initialize_process() {
        eprintln!("persister startup: {error}");
        return ExitCode::FAILURE;
    }

    // Every file the persister creates, recording windows included, is private to its user.
    unsafe {
        umask(0o077);
    }
    // At logout and shutdown macOS terminates every process, this one too, while it seals the
    // minute its collector left. It exits once that collector is gone, and the collector kills
    // it after two minutes, so it never needs the signal to stop.
    if unsafe { signal(SIGTERM, SIG_IGN) } == SIG_ERR {
        eprintln!("persister startup: {}", std::io::Error::last_os_error());
        return ExitCode::FAILURE;
    }
    let options = collector::arguments("persister").and_then(|words| options(&words));
    let derive = viewer::SealedApps::default();
    match options {
        Ok((root, memory)) => match persistence::worker::run(&root, memory, &derive) {
            Ok(persistence::worker::Outcome::Complete) => ExitCode::SUCCESS,
            Ok(persistence::worker::Outcome::Owned) => {
                eprintln!("recording owned by surviving persister; CPU not measured");
                ExitCode::from(collector::recording::child::OWNERSHIP_EXIT_STATUS)
            }
            Err(error) => {
                eprintln!("persister failed; unsealed history is at risk: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

const SIGTERM: c_int = 15;
const SIG_IGN: usize = 1;
const SIG_ERR: usize = usize::MAX;

unsafe extern "C" {
    fn signal(signal: c_int, handler: usize) -> usize;
    fn umask(mask: u16) -> u16;
}

fn options(words: &[String]) -> Result<(PathBuf, u64), String> {
    let usage = || "usage: persister --recording PATH [--memory-bytes N]".to_owned();
    let mut root = None;
    let mut memory = persistence::worker::default_memory_limit();
    for pair in words.chunks(2) {
        match pair {
            [flag, value] if flag == "--recording" => root = Some(PathBuf::from(value)),
            [flag, value] if flag == "--memory-bytes" => {
                memory = value.parse().map_err(|_| usage())?;
            }
            _ => return Err(usage()),
        }
    }
    Ok((root.ok_or_else(usage)?, memory))
}
