use std::io;
use std::path::Path;

use collector::recording::control::Control;
use collector::recording::{
    BUILD, Holder, Registry, holder, identity_text, lifetime_lock_held, read_identity,
};
use collector::ring::{Identity, RingReader, RunIdentity};

use super::policy::{Build, Observation, RecordingStatus, Source};

pub(super) struct Health(Box<Storage>);

struct Storage {
    recorder: Option<super::recorder::Recorder>,
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
            recorder: None,
            registry: Box::new([0; 4097]),
            readers: [None, None],
        }))
    }

    pub fn read(&mut self, root: &Path, owned_pid: Option<u32>) -> io::Result<Source> {
        let previous = self.0.recorder.take();
        if let Some(recorder) = super::recorder::Recorder::read(root, previous.as_ref())? {
            let source = recorder.source()?;
            self.0.recorder = Some(recorder);
            self.0.readers = [None, None];
            return Ok(source);
        }
        // The readings of the control file and the lifetime locks describe helpers someone else
        // runs, which only discovery asks about. A recorder this app started holds its lock
        // before it replaces the control file another build left, and would read as that build.
        if owned_pid.is_some() {
            return self.read_legacy(root, owned_pid);
        }
        if let Some(source) = read_other_build(root)? {
            self.0.readers = [None, None];
            return Ok(source);
        }

        let source = self.read_legacy(root, owned_pid)?;
        read_unreadable(root, source)
    }

    fn read_legacy(&mut self, root: &Path, owned_pid: Option<u32>) -> io::Result<Source> {
        let recording = match read_identity(root) {
            Ok(identity) => identity,
            // A recorder that died before it wrote the identity left a root the next one adopts.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Source {
                    recording: RecordingStatus::Ready,
                    ..Source::default()
                });
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
            // Only the announced run has a writer. Any other run's ring may stay until a reboot in
            // a shape or under a name this build refuses, which must not keep a launch from
            // recording.
            let Some((_, build)) = announced.filter(|(run, _)| *run == registered.identity.run)
            else {
                continue;
            };
            if build != BUILD {
                if followed {
                    source.build = Build::Other(build);
                }
                continue;
            }

            let reader = read_reader(&mut previous, &registered.identity, registered.ring_name)?;
            let Some(reader) = reader else {
                source.recording = RecordingStatus::Pending;
                source.producer_alive = true;
                continue;
            };

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
            self.0.readers[index] = Some(reader);
        }

        Ok(source)
    }

    pub fn clear(&mut self) {
        self.0.recorder = None;
        self.0.readers = [None, None];
    }

    pub fn notice(&self) -> Option<String> {
        self.0
            .recorder
            .as_ref()
            .map(super::recorder::Recorder::notice)
    }
}

/// Reads a control file another build wrote, whose words this build cannot read but whose stop
/// and lifetime locks still say whether that build's helpers run. A file that fails the checks
/// across builds, or names this build after all, is left to the reading from the collector's lock
/// and announcement.
fn read_other_build(root: &Path) -> io::Result<Option<Source>> {
    let written = match Control::read_across_builds(root) {
        Ok(written) => written,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if written.build == BUILD {
        return Ok(None);
    }

    let mut source = Source::default();
    if !lifetime_lock_held(root)? {
        source.recording = RecordingStatus::Ready;
        return Ok(Some(source));
    }

    // Helpers still saving after they were asked to stop keep their build, so a wait that never
    // ends names the command that stops them.
    source.build = Build::Other(written.build);
    if written.stopping {
        source.recording = RecordingStatus::Finishing;
    }
    Ok(Some(source))
}

/// Keeps a launch from starting a recorder beside a recorder or persister whose control file it
/// cannot read. A recording no collector holds reads as ready only when the collector's lock is
/// free, so a lifetime lock held then is the recorder's or the persister's.
fn read_unreadable(root: &Path, mut source: Source) -> io::Result<Source> {
    if source.recording == RecordingStatus::Ready
        && !source.producer_alive
        && source.build == Build::Expected
        && lifetime_lock_held(root)?
    {
        source.recording = RecordingStatus::Finishing;
    }
    Ok(source)
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
        boot: Some(identity_text(reader.identity.boot).to_string()),
        authority: None,
        publication,
    }))
}
