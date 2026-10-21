use std::io;
use std::path::Path;

use collector::recording::{BUILD, Holder, Registry, holder, identity_text, read_identity};
use collector::ring::{Identity, RingReader, RunIdentity};

use super::policy::{Build, Observation, RecordingStatus, Source};

pub(super) struct Health(Box<Storage>);

struct Storage {
    registry: Box<[u8; 4097]>,
    readers: [Option<Box<Reader>>; READERS_MAX],
}

struct Reader {
    identity: RunIdentity,
    ring: RingReader,
}

const READERS_MAX: usize = 2;
const MAPPING_BYTES_MAX: u64 = 1_073_741_824;
const _: () = assert!(size_of::<Health>() <= 16);

impl Health {
    pub fn new() -> Self {
        Self(Box::new(Storage {
            registry: Box::new([0; 4097]),
            readers: [None, None],
        }))
    }

    pub fn read(&mut self, root: &Path, owned_pid: Option<u32>) -> io::Result<Source> {
        let recording = match read_identity(root) {
            Ok(identity) => identity,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Source::default());
            }
            Err(error) => return Err(error),
        };
        // A collector registers its run before it announces it, so the registry read after
        // the holder lists any run the holder announces.
        let holder = holder(root)?;
        let registry = Registry::read_into(&root.join("runs"), &mut self.0.registry)?;
        let newest = registry
            .runs()
            .last()
            .transpose()?
            .map(|run| run.identity.run);
        let (mut source, announced) = read_holder(holder, newest);
        let mut previous = std::mem::take(&mut self.0.readers);
        for (index, registered) in registry.runs().enumerate().take(READERS_MAX) {
            let registered = registered?;
            if registered.identity.recording != recording {
                return Err(io::Error::other(
                    "Collector registration belongs to another recording",
                ));
            }

            let followed = owned_pid.is_none_or(|pid| pid == registered.collector_pid);
            let alive = match announced {
                Some((run, build)) if run == registered.identity.run => {
                    if build != BUILD {
                        if followed {
                            source.build = Build::Other(build);
                        }
                        continue;
                    }

                    true
                }
                _ => false,
            };
            let reader = read_reader(&mut previous, &registered.identity, registered.ring_name)?;
            let Some(reader) = reader else {
                if alive {
                    source.recording = RecordingStatus::Pending;
                    source.producer_alive = true;
                }
                continue;
            };

            if alive {
                if reader.ring.completed() {
                    // The run ended, and either its collector still saves its last readings, or
                    // the collector now holding the recording has yet to withdraw the
                    // announcement the run left.
                    source.recording = RecordingStatus::Finishing;
                } else {
                    source.producer_alive = true;
                    if followed {
                        source.observation = read_observation(&reader, registered.collector_pid)?;
                    }
                }
            }
            self.0.readers[index] = Some(reader);
        }

        Ok(source)
    }

    pub fn clear(&mut self) {
        self.0.readers = [None, None];
    }
}

/// Starts the source from what the holder says, and returns the run it announces when that
/// is the newest registered run, the only one the holder can be writing.
fn read_holder(holder: Holder, newest: Option<Identity>) -> (Source, Option<(Identity, u64)>) {
    let mut source = Source {
        recording: RecordingStatus::Ready,
        ..Source::default()
    };
    match holder {
        Holder::Absent => (source, None),
        Holder::Announced { run, build } if Some(run) == newest => (source, Some((run, build))),
        // An announcement naming an older run outlives its collector until the next holder
        // withdraws it, so that holder has not announced itself yet.
        Holder::Announced { .. } | Holder::Unannounced => {
            source.recording = RecordingStatus::Pending;
            source.build = Build::Unannounced;
            (source, None)
        }
        Holder::Unrecognized => {
            source.recording = RecordingStatus::Pending;
            source.build = Build::Unrecognized;
            (source, None)
        }
    }
}

fn read_reader(
    previous: &mut [Option<Box<Reader>>; READERS_MAX],
    identity: &RunIdentity,
    ring_name: &str,
) -> io::Result<Option<Box<Reader>>> {
    for slot in previous.iter_mut().take(READERS_MAX) {
        if slot
            .as_ref()
            .is_some_and(|reader| reader.identity == *identity && reader.ring.name() == ring_name)
        {
            return Ok(slot.take());
        }
    }

    let ring = match RingReader::open_generation(ring_name) {
        Ok(ring) => ring,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if ring.identity() != Some(*identity) {
        return Err(io::Error::other(
            "Collector ring identity disagrees with its registration",
        ));
    }

    if u64::try_from(ring.mapped_size_bytes()).map_err(io::Error::other)? > MAPPING_BYTES_MAX {
        return Err(io::Error::other("Collector health mapping exceeds one GiB"));
    }

    Ok(Some(Box::new(Reader {
        identity: *identity,
        ring,
    })))
}

fn read_observation(reader: &Reader, pid: u32) -> io::Result<Option<Observation>> {
    let publication = reader.ring.publication_newest()?;
    if publication == 0 {
        return Ok(None);
    }

    Ok(Some(Observation {
        pid,
        run: identity_text(reader.identity.run).to_string(),
        boot: identity_text(reader.identity.boot).to_string(),
        publication,
    }))
}
