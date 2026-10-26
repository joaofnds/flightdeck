//! Input-side identities and native bytes, independent of either measured viewer output.
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use collector::ring::{Found, FoundEvent, IdentityObservation, ProgramBuffers, RingReader};
use serde_json::{Value, json};

use super::FrozenRecording;
use super::mapping::FrozenRing;

pub(super) struct Boundary {
    pub(super) publication: u64,
    pub(super) events_first: u64,
    pub(super) events_end: u64,
}

const PUBLICATIONS_MAX: u64 = 65_536;
const CONTEXT_INPUTS_MAX: usize = 6_000_000;
const MANIFEST_BYTES_MAX: usize = 256 * 1024 * 1024;

pub(super) fn observations(rings: &[FrozenRing]) -> io::Result<Vec<Value>> {
    let mut observations = Vec::new();
    for ring in rings {
        visit(ring, |_reader, publication, tick, _identities| {
            observations.push(observation(
                &ring.identity,
                publication,
                tick.run_tick,
                tick.times
                    .ok_or_else(|| io::Error::other("native input clock"))?
                    .wall_ns,
            ));
            Ok(())
        })?;
    }
    Ok(observations)
}

pub(super) fn expectation(frozen: &FrozenRecording, path: &Path) -> io::Result<()> {
    let newest = frozen.replay.get().unwrap_or(frozen.active_ring().newest);
    let active = format!(
        "{:016x}{:016x}",
        frozen.active_ring().identity.run.0[0],
        frozen.active_ring().identity.run.0[1]
    );
    let mut observations = frozen.observations.clone();
    export_windows(frozen, &mut observations)?;
    observations.retain(|point| {
        point["run"].as_str() != Some(&active)
            || point["publication"]
                .as_u64()
                .is_some_and(|publication| publication <= newest)
    });
    let (selected, observations) = context(observations)?;
    let bytes = serde_json::to_vec_pretty(&json!({"selected": selected,
        "observations": observations, "payloads": Value::Null,
        "provenance": "native input identities and currently visible committed machine ticks"}))
    .map_err(io::Error::other)?;
    let mut file = collector::recording::private::open_private_file(path, true)?;
    io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()
}

pub(super) fn boundaries(ring: &FrozenRing) -> io::Result<Vec<Boundary>> {
    let mut output = Vec::new();
    visit(ring, |reader, publication, tick, _identities| {
        validate_events(reader, tick, &mut Vec::new())?;
        output.push(Boundary {
            publication,
            events_first: tick.events.start,
            events_end: tick.events.end,
        });
        Ok(())
    })?;
    Ok(output)
}

pub(super) fn export(frozen: &FrozenRecording, directory: &Path) -> io::Result<()> {
    DirBuilder::new().mode(0o700).create(directory)?;
    let mut rings = Vec::new();
    let mut observations = Vec::new();
    for (index, ring) in frozen.rings.iter().enumerate() {
        let backing = directory.join(format!("{index}.ring"));
        ring.export(&backing)?;
        let mut publications = Vec::new();
        visit(ring, |reader, publication, tick, identities| {
            let observation = observation(
                &ring.identity,
                publication,
                tick.run_tick,
                tick.times
                    .ok_or_else(|| io::Error::other("native observation has no clock"))?
                    .wall_ns,
            );
            observations.push(observation.clone());
            let mut events = Vec::new();
            validate_events(reader, tick, &mut events)?;
            let processes = identities
                .iter()
                .map(|identity| identity_value(reader, identity))
                .collect::<io::Result<Vec<_>>>()?;
            publications.push(json!({"observation": observation,
                "events_first": tick.events.start, "events_end": tick.events.end,
                "process_identities": processes, "events": events}));
            Ok(())
        })?;
        rings.push(json!({"identity": format!("{:?}", ring.identity),
            "retained_first": ring.retained_first, "newest": ring.newest,
            "backing": backing, "backing_bytes": ring.backing_bytes(),
            "sha256": file_hash(&backing)?,
            "publications": publications}));
    }
    let windows = export_windows(frozen, &mut observations)?;
    export_manifest(frozen, directory, &rings, &windows, observations)
}

fn export_manifest(
    frozen: &FrozenRecording,
    directory: &Path,
    rings: &[Value],
    windows: &[Value],
    observations: Vec<Value>,
) -> io::Result<()> {
    let (selected, observations) = context(observations)?;
    let manifest = json!({"provenance":
        "native readers and committed machine decoder; no measured outputs",
        "identity_sha256": file_hash(&frozen.root().join("identity"))?,
        "rings": rings, "windows": windows, "selected": selected,
        "observations": observations, "payloads": Value::Null,
        "payload_oracle":
            "freeze separately from route contracts or baseline correctness before timing"});
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    if bytes.len() > MANIFEST_BYTES_MAX {
        return Err(io::Error::other("input manifest byte bound"));
    }

    let mut file =
        collector::recording::private::open_private_file(&directory.join("input.json"), true)?;
    io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()
}

fn context(mut observations: Vec<Value>) -> io::Result<(Value, Vec<Value>)> {
    observations.sort_by_key(|point| {
        (
            point["wall_ns"].as_u64(),
            point["run"].as_str().map(str::to_owned),
            point["publication"].as_u64(),
        )
    });
    observations.dedup();
    let selected = observations
        .last()
        .ok_or_else(|| io::Error::other("empty input"))?
        .clone();
    let last = selected["wall_ns"]
        .as_u64()
        .ok_or_else(|| io::Error::other("input clock"))?
        / 1_000_000_000;
    observations.retain(|point| {
        point["wall_ns"].as_u64().is_some_and(|wall| {
            wall / 1_000_000_000 >= last.saturating_sub(viewer::CHARTED_SECONDS - 1)
        })
    });
    Ok((selected, observations))
}

fn visit(
    ring: &FrozenRing,
    mut accept: impl FnMut(
        &mut RingReader,
        u64,
        &collector::ring::Tick,
        &[IdentityObservation],
    ) -> io::Result<()>,
) -> io::Result<()> {
    if ring.newest < ring.retained_first {
        return Err(io::Error::other(
            "native ring contains no retained publications",
        ));
    }

    if ring.newest - ring.retained_first + 1 > PUBLICATIONS_MAX {
        return Err(io::Error::other("native publication bound"));
    }

    let mut reader = RingReader::open_generation(ring.name()?)?;
    let mut tick = persistence::tick_buffer(&reader.shape()).map_err(io::Error::other)?;
    let mut identities = Vec::with_capacity(
        usize::try_from(reader.shape().process_capacity).map_err(io::Error::other)?,
    );
    for expected in ring.retained_first..=ring.newest {
        match reader.read_identity_into(&mut tick, &mut identities)? {
            Found::Publication { publication, .. } if publication == expected => {
                accept(&mut reader, publication, &tick, &identities)?;
            }
            _ => {
                return Err(io::Error::other(
                    "native publication overwritten or incomplete",
                ));
            }
        }
    }
    Ok(())
}

fn validate_events(
    reader: &mut RingReader,
    tick: &collector::ring::Tick,
    events: &mut Vec<Value>,
) -> io::Result<()> {
    if tick.events.end - tick.events.start > reader.shape().event_capacity {
        return Err(io::Error::other("native event range bound"));
    }

    reader.seek_event(tick.events.start)?;
    for expected in tick.events.clone() {
        let mut identity = IdentityObservation::EMPTY;
        match reader.read_event_identity(&mut identity)? {
            FoundEvent::Event {
                number,
                tick: event_tick,
                event,
            } if number == expected && event_tick == tick.run_tick => {
                events.push(json!({"number": number, "run_tick": event_tick,
                    "metadata": format!("{event:?}"),
                    "identity": identity_value(reader, &identity)?}));
            }
            _ => return Err(io::Error::other("native event overwritten or incomplete")),
        }
    }
    Ok(())
}

fn identity_value(reader: &RingReader, identity: &IdentityObservation) -> io::Result<Value> {
    let mut bytes = Value::Null;
    if let Some(program) = identity.program() {
        let mut name = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
        let mut app = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
        let (name_length, app_length) = reader.copy_program(
            program,
            &mut ProgramBuffers {
                executable_name: &mut name,
                app_path: &mut app,
            },
        )?;
        bytes = json!({"program": program.number(), "name_length": name_length,
            "name_sha256": hash(&name[..name_length])?, "app_length": app_length,
            "app_sha256": hash(&app[..app_length])?});
    }
    Ok(
        json!({"pid": identity.pid, "usage": format!("{:?}", identity.usage),
        "availability": format!("{:?}", identity.availability()),
        "creation": format!("{:?}", identity.creation()), "resolved_bytes": bytes}),
    )
}

fn observation(
    identity: &collector::ring::RunIdentity,
    publication: u64,
    run_tick: u64,
    wall_ns: u64,
) -> Value {
    let text = |value: collector::ring::Identity| format!("{:016x}{:016x}", value.0[0], value.0[1]);
    json!({"run": text(identity.run), "boot": text(identity.boot),
        "publication": publication, "run_tick": run_tick, "wall_ns": wall_ns})
}

fn export_windows(
    frozen: &FrozenRecording,
    observations: &mut Vec<Value>,
) -> io::Result<Vec<Value>> {
    let mut windows = Vec::new();
    for window in &frozen.files.captured {
        if !window.marker.try_exists()? {
            continue;
        }

        let minute = viewer::recording::SealedMinute::open(frozen.root(), &window.marker)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let mut machine = viewer::recording::MachineMinute::default();
        viewer::recording::read_machine(&minute, &mut machine)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        for tick in &machine.ticks {
            if observations.len() == CONTEXT_INPUTS_MAX {
                return Err(io::Error::other("committed input observation bound"));
            }
            observations.push(observation(
                &window.manifest.identity.run,
                tick.publication,
                tick.run_tick,
                tick.wall_ns
                    .ok_or_else(|| io::Error::other("sealed input clock"))?,
            ));
        }
        let directory = frozen.root().join(&window.manifest.data_directory);
        let tables = window
            .manifest
            .tables
            .iter()
            .map(|table| {
                let path = directory.join(format!("{}.parquet", table.name));
                Ok(json!({"path": path, "sha256": file_hash(&path)?}))
            })
            .collect::<io::Result<Vec<_>>>()?;
        windows.push(json!({"manifest": format!("{:?}", window.manifest),
            "marker": window.marker, "sha256": file_hash(&window.marker)?, "tables": tables}));
    }
    Ok(windows)
}

fn file_hash(path: &Path) -> io::Result<String> {
    if fs::metadata(path)?.len() > 1024 * 1024 * 1024 {
        return Err(io::Error::other("input file hash bound"));
    }

    let bytes = fs::read(path)?;
    hash(&bytes)
}

fn hash(bytes: &[u8]) -> io::Result<String> {
    unsafe extern "C" {
        fn CC_SHA256(data: *const u8, length: u32, digest: *mut u8) -> *mut u8;
    }
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    let mut digest = [0_u8; 32];
    let result = unsafe { CC_SHA256(bytes.as_ptr(), length, digest.as_mut_ptr()) };
    if result.is_null() {
        return Err(io::Error::other("SHA256 failed"));
    }

    let mut text = String::with_capacity(64);
    for byte in digest {
        for nibble in [byte >> 4, byte & 0x0f] {
            text.push(char::from(b"0123456789abcdef"[usize::from(nibble)]));
        }
    }
    Ok(text)
}
