use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use collector::ring::{Found, FoundEvent, Reader};

/// The longest the reader waits for the next publication before it looks again.
const WAIT_TIMEOUT: Duration = Duration::from_secs(2);

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
    let mut tick_last = None;
    while read < options.publications {
        match reader.read()? {
            Found::Publication { publication, tick } => {
                let readings = reader.tick();
                println!(
                    "publication {publication} tick {tick} processes {} coalitions {}",
                    readings.processes.len(),
                    readings.coalitions.len()
                );
                // The collector numbers ticks from its run's start and skips those it slipped.
                if let Some(tick_last) = tick_last
                    && tick > tick_last + 1
                {
                    println!("slipped ticks {}-{}", tick_last + 1, tick - 1);
                }
                tick_last = Some(tick);
                run_events(&mut reader)?;
                read += 1;
            }
            Found::Missed { first, end } => println!("missed publications {first}-{}", end - 1),
            Found::Nothing => reader.wait(WAIT_TIMEOUT)?,
        }
    }
    Ok(())
}

/// Reads the events the writer has logged so far, and prints how many it read, which it
/// missed, and how many the tick says the collector dropped.
fn run_events(reader: &mut Reader) -> std::io::Result<()> {
    let events = reader.tick().events.clone();
    let dropped = reader.tick().dropped;
    let mut read = 0;
    // The log holds a bounded count of events, so reading it ends.
    loop {
        match reader.read_event()? {
            FoundEvent::Event { .. } => read += 1,
            FoundEvent::Missed { first, end } => println!("missed events {first}-{}", end - 1),
            FoundEvent::Nothing => break,
        }
    }
    if !events.is_empty() || read > 0 {
        println!(
            "events {}..{} read {read} dropped starts {} execs {} exits {}",
            events.start, events.end, dropped.starts, dropped.execs, dropped.exits
        );
    }
    Ok(())
}
