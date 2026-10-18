use std::io;
use std::path::Path;

use collector::recording::{Registry, identity_text, read_identity};
use collector::ring::{RingReader, RunIdentity};

use super::policy::{Observation, RecordingStatus, Source};

pub(super) struct Health(Box<Storage>);

struct Storage {
    registry: Box<[u8]>,
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
            registry: vec![0; 4097].into_boxed_slice(),
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
        let buffer: &mut [u8; 4097] = self
            .0
            .registry
            .as_mut()
            .try_into()
            .map_err(io::Error::other)?;
        let registry = Registry::read_into(&root.join("runs"), buffer)?;
        let mut previous = std::mem::take(&mut self.0.readers);
        let mut source = Source {
            recording: RecordingStatus::Ready,
            ..Source::default()
        };
        for (index, registered) in registry.runs().enumerate().take(READERS_MAX) {
            let registered = registered?;
            if registered.identity.recording != recording {
                return Err(io::Error::other(
                    "Collector registration belongs to another recording",
                ));
            }

            let alive =
                super::producer_alive(registered.collector_pid).map_err(io::Error::other)?;
            let reader = read_reader(&mut previous, &registered.identity, registered.ring_name)?;
            let Some(reader) = reader else {
                if alive {
                    source.recording = RecordingStatus::Pending;
                    source.producer_alive = true;
                }
                continue;
            };
            if alive {
                if !reader.ring.completed() {
                    source.producer_alive = true;
                    if owned_pid.is_none_or(|pid| pid == registered.collector_pid) {
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
