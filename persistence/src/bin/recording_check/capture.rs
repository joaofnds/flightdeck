use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use collector::recording::private;
use collector::ring::{Found, FoundEvent, RingReader, RunIdentity, Tick};
use persistence::{Error, Result};

use super::{identity_reference, rows};

pub fn run(lock: &Path, reference: &Path, seconds: u64) -> Result<()> {
    if !(1..=3600).contains(&seconds) {
        return Err(Error::Bound("capture duration"));
    }

    let started = Instant::now();
    let mut reader = open(lock, started, seconds)?;
    let identity_capable = reader.source_layout_version() == 4;
    let identity = reader
        .identity()
        .ok_or(Error::Bound("source run identity unavailable"))?;
    let mut tick = persistence::tick_buffer(&reader.shape())?;
    if reader.next_publication() != reader.publication_first() {
        return Err(Error::Bound(
            "capture opened after initial publications were overwritten",
        ));
    }

    let deadline = started + Duration::from_secs(seconds);
    let mut writer = reference_writer(reference, &identity, identity_capable, deadline)?;
    let mut processes = Vec::with_capacity(
        usize::try_from(reader.shape().process_capacity)
            .map_err(|_| Error::Bound("reference process capacity"))?,
    );
    let mut publications = 0_u64;
    for _ in 0..seconds * 200 + 100 {
        if started.elapsed() > Duration::from_secs(seconds) {
            return Err(Error::Bound(
                "capture ended before orderly source completion",
            ));
        }

        match reader.read_identity_into(&mut tick, &mut processes)? {
            Found::Publication { publication, .. } => {
                run_publication_rows(
                    &mut writer,
                    &mut reader,
                    publication,
                    &tick,
                    if identity_capable {
                        Some(&processes)
                    } else {
                        None
                    },
                )?;
                publications += 1;
            }
            Found::Missed { .. } => {
                return Err(Error::Bound("reference reader missed publications"));
            }
            Found::Nothing => {
                if reader.completed() {
                    if reader.next_publication() > reader.publication_newest()? {
                        return run_complete(&mut writer, publications, started);
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    Err(Error::Bound("capture iteration bound"))
}

fn run_complete(writer: &mut ReferenceWriter, publications: u64, started: Instant) -> Result<()> {
    if publications == 0 {
        return Err(Error::Bound("reference contains no publications"));
    }

    writeln!(writer, "complete {publications}")?;
    writer.flush()?;
    writer.writer.get_ref().sync_all()?;
    println!(
        "captured {publications} publications in {:.3}s",
        started.elapsed().as_secs_f64()
    );

    Ok(())
}

fn reference_writer(
    reference: &Path,
    identity: &RunIdentity,
    identity_capable: bool,
    deadline: Instant,
) -> Result<ReferenceWriter> {
    let parent = reference.parent().ok_or(Error::Bound("reference parent"))?;
    private::verify_directory(parent)?;
    if reference.try_exists()? {
        return Err(Error::Bound("reference already exists"));
    }

    let file = private::open_private_file(reference, true)?;
    let mut writer = ReferenceWriter {
        writer: BufWriter::with_capacity(65_536, file),
        bytes: 0,
        lines: 0,
        line_bytes: 0,
        deadline,
    };
    let version = if identity_capable {
        "flightdeck-reference-2 identity-1"
    } else {
        "flightdeck-reference-1"
    };
    writeln!(
        writer,
        "{version} {} {} {} {} {} {}",
        identity.recording.0[0],
        identity.recording.0[1],
        identity.run.0[0],
        identity.run.0[1],
        identity.boot.0[0],
        identity.boot.0[1]
    )?;

    Ok(writer)
}

fn open(lock: &Path, started: Instant, seconds: u64) -> Result<RingReader> {
    for _ in 0..seconds * 100 {
        match RingReader::open(lock) {
            Ok(reader) => return Ok(reader),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if started.elapsed() > Duration::from_secs(seconds) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(Error::Bound("source ring did not appear"))
}

fn events(
    reader: &mut RingReader,
    tick: &Tick,
    publication: u64,
    writer: &mut impl Write,
    identity_capable: bool,
) -> Result<()> {
    reader.seek_event(tick.events.start)?;
    for _ in 0..collector::tracker::EVENTS_PER_TICK_MAX {
        if reader.next_event() >= tick.events.end {
            return Ok(());
        }

        let mut identity = collector::ring::IdentityObservation::EMPTY;
        match reader.read_event_identity(&mut identity)? {
            FoundEvent::Event {
                number,
                tick,
                event,
            } => {
                rows::event(writer, publication, number, tick, &event)?;
                if identity_capable {
                    identity_reference::event(
                        writer,
                        reader,
                        identity_reference::EventPosition {
                            publication,
                            number,
                        },
                        &identity,
                    )?;
                }
            }
            FoundEvent::Missed { .. } => return Err(Error::Bound("reference missed events")),
            FoundEvent::Nothing => return Err(Error::Bound("source event unavailable")),
        }
    }
    if reader.next_event() == tick.events.end {
        return Ok(());
    }

    Err(Error::Bound("source event bound"))
}

fn run_publication_rows(
    writer: &mut impl Write,
    reader: &mut RingReader,
    publication: u64,
    tick: &Tick,
    processes: Option<&[collector::ring::IdentityObservation]>,
) -> Result<()> {
    let times = tick
        .times
        .ok_or(Error::Bound("observation times unavailable"))?;
    writeln!(writer, "begin {publication} {}", times.wall_ns)?;
    rows::tick(writer, publication, tick)?;
    if let Some(processes) = processes {
        identity_reference::processes(writer, reader, publication, tick, processes)?;
    }

    events(reader, tick, publication, writer, processes.is_some())
}

struct ReferenceWriter {
    writer: BufWriter<File>,
    bytes: u64,
    lines: u64,
    line_bytes: u64,
    deadline: Instant,
}

impl Write for ReferenceWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(std::io::Error::other("reference capture time bound"));
        }

        let count = u64::try_from(buffer.len()).map_err(std::io::Error::other)?;
        if count > rows::REFERENCE_BYTES_MAX - self.bytes {
            return Err(std::io::Error::other("reference file bound"));
        }

        let mut lines = self.lines;
        let mut line_bytes = self.line_bytes;
        for byte in buffer {
            line_bytes += 1;
            if line_bytes > rows::REFERENCE_LINE_BYTES_MAX {
                return Err(std::io::Error::other("reference line bound"));
            }

            if *byte == b'\n' {
                lines += 1;
                line_bytes = 0;
            }
        }
        if lines > rows::REFERENCE_LINES_MAX {
            return Err(std::io::Error::other("reference row bound"));
        }

        self.writer.write_all(buffer)?;
        self.bytes += count;
        self.lines = lines;
        self.line_bytes = line_bytes;

        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_writer_limits_before_accepting_any_part_of_the_chunk() {
        // Goal: capture refuses each output bound without appending a false completion.
        // Method: seed otherwise valid near-limit state instead of writing eight GiB.
        let root = std::env::temp_dir().join(format!("flightdeck-writer-{}", std::process::id()));
        private::create_private_dir(&root).unwrap();
        let identity = RunIdentity {
            recording: collector::ring::Identity([1, 2]),
            run: collector::ring::Identity([3, 4]),
            boot: collector::ring::Identity([5, 6]),
        };
        for limit in ["bytes", "line", "rows", "deadline"] {
            let path = root.join(limit);
            let mut writer = reference_writer(
                &path,
                &identity,
                true,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
            writer.flush().unwrap();
            let original = std::fs::read(&path).unwrap();
            match limit {
                "bytes" => writer.bytes = rows::REFERENCE_BYTES_MAX,
                "line" => writer.line_bytes = rows::REFERENCE_LINE_BYTES_MAX,
                "rows" => writer.lines = rows::REFERENCE_LINES_MAX,
                "deadline" => writer.deadline = Instant::now(),
                _ => unreachable!(),
            }
            let state = (writer.bytes, writer.lines, writer.line_bytes);

            let result = writer.write_all(b"complete 1\n");

            assert!(result.unwrap_err().to_string().contains(match limit {
                "bytes" => "file bound",
                "line" => "line bound",
                "rows" => "row bound",
                "deadline" => "time bound",
                _ => unreachable!(),
            }));
            assert_eq!((writer.bytes, writer.lines, writer.line_bytes), state);
            writer.flush().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
