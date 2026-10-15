use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use collector::recording::private;
use collector::ring::{Found, FoundEvent, RingReader, RunIdentity, Shape, Tick, TickBuffers};
use persistence::{Error, Limits, Result};

use super::rows;

pub fn run(lock: &Path, reference: &Path, seconds: u64) -> Result<()> {
    if !(1..=3600).contains(&seconds) {
        return Err(Error::Bound("capture duration"));
    }
    let started = Instant::now();
    let mut reader = open(lock, started, seconds)?;
    let identity = reader
        .identity()
        .ok_or(Error::Bound("source run identity unavailable"))?;
    Limits::from_shape(&reader.shape())?;
    let mut tick = buffers(reader.shape())?;
    if reader.next_publication() != reader.publication_first() {
        return Err(Error::Bound(
            "capture opened after initial publications were overwritten",
        ));
    }
    let mut writer = reference_writer(reference, &identity)?;
    let mut publications = 0_u64;
    for _ in 0..seconds * 200 + 100 {
        if started.elapsed() > Duration::from_secs(seconds) {
            return Err(Error::Bound(
                "capture ended before orderly source completion",
            ));
        }
        match reader.read_into(&mut tick)? {
            Found::Publication { publication, .. } => {
                let times = tick
                    .times
                    .ok_or(Error::Bound("observation times unavailable"))?;
                writeln!(writer, "begin {publication} {}", times.wall_ns)?;
                rows::tick(&mut writer, publication, &tick)?;
                events(&mut reader, &tick, publication, &mut writer)?;
                publications += 1;
                if writer.get_ref().metadata()?.len() > 8 * 1024 * 1024 * 1024 {
                    return Err(Error::Bound("reference exceeds 8 GiB"));
                }
            }
            Found::Missed { .. } => {
                return Err(Error::Bound("reference reader missed publications"));
            }
            Found::Nothing => {
                if reader.completed() {
                    if reader.next_publication() > reader.publication_newest()? {
                        writeln!(writer, "complete {publications}")?;
                        writer.flush()?;
                        writer.get_ref().sync_all()?;
                        println!(
                            "captured {publications} publications in {:.3}s",
                            started.elapsed().as_secs_f64()
                        );
                        return Ok(());
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    Err(Error::Bound("capture iteration bound"))
}

fn reference_writer(reference: &Path, identity: &RunIdentity) -> Result<BufWriter<File>> {
    let parent = reference.parent().ok_or(Error::Bound("reference parent"))?;
    private::verify_directory(parent)?;
    if reference.try_exists()? {
        return Err(Error::Bound("reference already exists"));
    }
    let file = private::open_private_file(reference, true)?;
    let mut writer = BufWriter::with_capacity(65_536, file);
    writeln!(
        writer,
        "flightdeck-reference-1 {} {} {} {} {} {}",
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
) -> Result<()> {
    reader.seek_event(tick.events.start)?;
    for _ in 0..collector::tracker::EVENTS_PER_TICK_MAX {
        if reader.next_event() >= tick.events.end {
            return Ok(());
        }
        match reader.read_event()? {
            FoundEvent::Event {
                number,
                tick,
                event,
            } => {
                rows::event(writer, publication, number, tick, &event)?;
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

fn buffers(shape: Shape) -> Result<Tick> {
    let count = |value| usize::try_from(value).map_err(|_| Error::Bound("source capacity"));
    let state_capacity = count(shape.state_capacity)?;
    Ok(Tick::from_buffers(TickBuffers {
        cpus: Vec::with_capacity(count(shape.cpu_capacity)?),
        interfaces: Vec::with_capacity(count(shape.interface_capacity)?),
        gpu_active_ns: Vec::with_capacity(state_capacity),
        core_active_ns: vec![0; count(shape.core_capacity)? * state_capacity],
        core_state_counts: Vec::with_capacity(count(shape.core_capacity)?),
        state_capacity,
        fan_rpm: Vec::with_capacity(count(shape.fan_capacity)?),
        processes: Vec::with_capacity(count(shape.process_capacity)?),
        coalitions: Vec::with_capacity(count(shape.coalition_capacity)?),
    }))
}
