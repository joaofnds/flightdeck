use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use collector::ring::{Found, FoundEvent, Place, Reader};

/// The longest the reader waits for the next publication before it looks again.
const WAIT_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest the reader goes without a publication before it gives up on the writer. The
/// collector publishes every second, so a writer this quiet has stopped.
const IDLE_MAX: Duration = Duration::from_secs(30);
/// The most events one look reads, over twice what the collector's log holds, so a look ends
/// while the writer goes on appending. The next look reads on from where this one stopped.
const EVENTS_PER_LOOK_MAX: u64 = 1 << 22;

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
    let usage = || "usage: ring_reader (--lock PATH | --ring NAME) --publications N".to_owned();
    let mut lock_path = None;
    let mut publications = None;

    for pair in arguments.chunks(2) {
        match pair {
            [flag, path] if flag == "--lock" => lock_path = Some(PathBuf::from(path)),
            [flag, name] if flag == "--ring" => {
                lock_path = Some(
                    Place::for_user(name)
                        .map_err(|error| error.to_string())?
                        .lock_path,
                );
            }
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
    let mut last = None;
    // The loop ends on the count read, or on a writer quiet for `IDLE_MAX`.
    let mut idle_since = Instant::now();
    while read < options.publications {
        let found = reader.read()?;
        if found != Found::Nothing {
            idle_since = Instant::now();
        }
        match found {
            Found::Publication { publication, tick } => {
                let readings = reader.tick();
                println!(
                    "publication {publication} tick {tick} processes {} coalitions {}",
                    readings.processes.len(),
                    readings.coalitions.len()
                );
                let seen = Seen { publication, tick };
                if let Some(slipped) = slipped(last, seen) {
                    println!("slipped ticks {}-{}", slipped.start(), slipped.end());
                }
                last = Some(seen);
                run_events(&mut reader)?;
                read += 1;
            }
            Found::Missed { first, end } => println!("missed publications {first}-{}", end - 1),
            Found::Nothing => {
                if reader.follow(&options.lock_path)? {
                    println!("moved to the writer's newer ring");
                } else {
                    if idle_since.elapsed() > IDLE_MAX {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("the writer published nothing for {IDLE_MAX:?}"),
                        ));
                    }

                    reader.wait(WAIT_TIMEOUT)?;
                }
            }
        }
    }
    Ok(())
}

/// A publication the reader read, and the tick it holds.
#[derive(Clone, Copy)]
struct Seen {
    publication: u64,
    tick: u64,
}

/// The ticks the collector slipped between the publication `last` and `seen`. The collector
/// numbers ticks from its run's start and skips those it slipped, so a gap in ticks between
/// publications that follow each other is a slip. Across a gap in publications the reader
/// missed the ticks between, and cannot tell.
fn slipped(last: Option<Seen>, seen: Seen) -> Option<RangeInclusive<u64>> {
    let last = last?;
    if seen.publication != last.publication + 1 {
        return None;
    }

    (seen.tick > last.tick + 1).then(|| last.tick + 1..=seen.tick - 1)
}

/// Reads the events up to the end of the tick the reader read last, and prints how many it
/// read, which it missed, and how many the tick says the collector dropped. Events the log
/// holds past the tick's wait for the tick that drained them. A look reads at most
/// `EVENTS_PER_LOOK_MAX`, so the events an earlier look left are read and counted beside the
/// next tick, whose count can then pass the length of its own range.
fn run_events(reader: &mut Reader) -> std::io::Result<()> {
    let events = reader.tick().events.clone();
    let dropped = reader.tick().dropped;
    let mut read = 0;
    for _ in 0..EVENTS_PER_LOOK_MAX {
        if reader.next_event() >= events.end {
            break;
        }

        match reader.read_event()? {
            FoundEvent::Event { .. } => read += 1,
            FoundEvent::Missed { first, end } => println!("missed events {first}-{}", end - 1),
            FoundEvent::Nothing => break,
        }
    }
    // A tick that logged events, or a read of events an earlier tick logged, is printed.
    if events.is_empty() {
        if read == 0 {
            return Ok(());
        }
    }

    println!(
        "events {}..{} read {read} dropped starts {} execs {} exits {}",
        events.start, events.end, dropped.starts, dropped.execs, dropped.exits
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tick_gap_between_consecutive_publications_is_a_slip() {
        // Goal: the reader reports the ticks the collector skipped between two publications
        // that follow each other.
        // Method: find the slip from publication 4 of tick 10 to publication 5 of tick 13.
        let last = Seen {
            publication: 4,
            tick: 10,
        };
        let seen = Seen {
            publication: 5,
            tick: 13,
        };

        assert_eq!(Some(11..=12), slipped(Some(last), seen));
    }

    #[test]
    fn a_tick_gap_across_missed_publications_is_no_slip() {
        // Goal: ticks the reader missed are not reported as ticks the collector slipped.
        // Method: find the slip from publication 4 of tick 10 to publication 25 of tick 31.
        let last = Seen {
            publication: 4,
            tick: 10,
        };
        let seen = Seen {
            publication: 25,
            tick: 31,
        };

        assert_eq!(None, slipped(Some(last), seen));
    }
}
