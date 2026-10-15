use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    // The engine creates temporary files only inside the private recording worker.
    unsafe extern "C" {
        fn umask(mask: u16) -> u16;
    }
    unsafe {
        umask(0o077);
    }
    let options = collector::arguments("persister").and_then(|words| options(&words));
    match options {
        Ok((root, memory)) => match persistence::worker::run(&root, memory) {
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
