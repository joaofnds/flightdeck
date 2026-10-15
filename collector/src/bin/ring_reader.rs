use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use collector::ring::{Found, Reader};

/// How long the reader sleeps when it has read every publication.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

fn main() -> ExitCode {
    let options =
        match collector::arguments("ring_reader").and_then(|arguments| options_from(&arguments)) {
            Ok(options) => options,
            Err(usage) => {
                eprintln!("{usage}");
                return ExitCode::from(2);
            }
        };

    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ring_reader: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    lock_path: PathBuf,
    publications: u64,
}

fn options_from(arguments: &[String]) -> Result<Options, String> {
    let usage = || "usage: ring_reader --lock PATH --publications N".to_owned();
    let mut lock_path = None;
    let mut publications = None;

    for pair in arguments.chunks(2) {
        match pair {
            [flag, path] if flag == "--lock" => lock_path = Some(PathBuf::from(path)),
            [flag, count] if flag == "--publications" => {
                publications = Some(count.parse().map_err(|_| usage())?);
            }
            _ => return Err(usage()),
        }
    }

    match (lock_path, publications) {
        (Some(lock_path), Some(publications)) => Ok(Options {
            lock_path,
            publications,
        }),
        _ => Err(usage()),
    }
}

fn run(options: &Options) -> std::io::Result<()> {
    let mut reader = Reader::open(&options.lock_path)?;

    let mut read = 0;
    while read < options.publications {
        match reader.read()? {
            Found::Publication { publication, tick } => {
                let readings = reader.tick();
                println!(
                    "publication {publication} tick {tick} processes {} coalitions {}",
                    readings.processes.len(),
                    readings.coalitions.len()
                );
                read += 1;
            }
            Found::Missed { first, end } => println!("missed publications {first}-{}", end - 1),
            Found::Nothing => std::thread::sleep(POLL_INTERVAL),
        }
    }
    Ok(())
}
