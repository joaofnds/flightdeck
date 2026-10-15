use std::io::{self, Cursor, Read, Write};
use std::path::Path;

use super::{identity_text, parse_identity, private, random_identity, write_atomic};
use crate::ring::{
    Identity, Place, RingReader, RunIdentity, previous_generation, unlink_generation,
};

const REGISTRY_BYTES_MAX: usize = 4096;
const RUNS_MAX: usize = 2;

/// The registry plus one byte to distinguish a full valid file from an oversized one.
pub type RegistryBuffer = [u8; REGISTRY_BYTES_MAX + 1];

/// Validated registry text borrowed from caller-owned polling storage.
pub struct RegistryView<'text> {
    text: &'text str,
}

#[derive(Debug)]
pub struct RegisteredRunView<'text> {
    pub identity: RunIdentity,
    pub ring_name: &'text str,
    pub collector_pid: u32,
}

impl RegistryView<'_> {
    pub fn evictions(&self) -> io::Result<u64> {
        if self.text.is_empty() {
            return Ok(0);
        }
        self.text
            .lines()
            .nth(1)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())
    }

    pub fn runs(&self) -> impl Iterator<Item = io::Result<RegisteredRunView<'_>>> {
        self.text.lines().skip(2).take(RUNS_MAX).map(parse_run_view)
    }
}

#[derive(Debug, Clone)]
pub struct RegisteredRun {
    pub identity: RunIdentity,
    pub ring_name: String,
    pub collector_pid: u32,
}

#[derive(Debug, Default)]
pub struct Registry {
    pub runs: Vec<RegisteredRun>,
    pub evictions: u64,
}

impl Registry {
    /// The caller retires only a closed source whose final commits are durable.
    pub fn retire(root: &Path, identity: &RunIdentity) -> io::Result<()> {
        let mut lock_path = [0; private::PATH_MAX];
        let path = private::join_path(root, "registration.lock", &mut lock_path)?;
        let lock = private::FileLock::wait_short(path)?;
        let mut runs_path = [0; private::PATH_MAX];
        let path = private::join_path(root, "runs", &mut runs_path)?;
        let mut buffer = [0; REGISTRY_BYTES_MAX + 1];
        let registry = Self::read_into(path, &mut buffer)?;
        for run in registry.runs() {
            let run = run?;
            if run.identity == *identity {
                retire_receipt(root, &run)?;
            }
        }
        drop(lock);
        Ok(())
    }

    /// A completed source stays registered until bounded retention reclaims its ring.
    pub fn completed(root: &Path, identity: &RunIdentity, ring_name: &str) -> io::Result<bool> {
        is_retired(root, identity, ring_name)
    }

    /// Opens the precomputed registry file path and validates all entries without allocation.
    pub fn read_into<'buffer>(
        path: &Path,
        buffer: &'buffer mut RegistryBuffer,
    ) -> io::Result<RegistryView<'buffer>> {
        let mut file = match private::open_private_file(path, false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RegistryView { text: "" });
            }
            Err(error) => return Err(error),
        };

        let mut length = 0;
        while length < buffer.len() {
            let count = file.read(&mut buffer[length..])?;
            if count == 0 {
                break;
            }
            length += count;
        }
        if length > REGISTRY_BYTES_MAX {
            return Err(invalid());
        }
        let text = std::str::from_utf8(&buffer[..length]).map_err(|_| invalid())?;
        let view = RegistryView { text };
        let mut lines = text.lines();
        if lines.next() != Some("flightdeck-runs-1") {
            return Err(invalid());
        }
        view.evictions()?;
        if lines.next().is_none() {
            return Err(invalid());
        }
        for (index, line) in lines.take(RUNS_MAX + 1).enumerate() {
            if index == RUNS_MAX {
                return Err(invalid());
            }
            parse_run_view(line)?;
        }
        Ok(view)
    }

    pub fn read(root: &Path) -> io::Result<Self> {
        let mut buffer = [0; REGISTRY_BYTES_MAX + 1];
        let view = Self::read_into(&root.join("runs"), &mut buffer)?;
        let mut registry = Self {
            runs: Vec::with_capacity(RUNS_MAX),
            evictions: view.evictions()?,
        };
        for run in view.runs() {
            let run = run?;
            registry.runs.push(RegisteredRun {
                identity: run.identity,
                ring_name: run.ring_name.to_owned(),
                collector_pid: run.collector_pid,
            });
        }
        Ok(registry)
    }

    pub fn write(&self, root: &Path) -> io::Result<()> {
        assert!(self.runs.len() <= RUNS_MAX);
        let mut text = format!("flightdeck-runs-1\n{}\n", self.evictions);
        for run in &self.runs {
            use std::fmt::Write;
            writeln!(
                text,
                "{} {} {} {} {}",
                identity_text(run.identity.recording),
                identity_text(run.identity.run),
                identity_text(run.identity.boot),
                run.collector_pid,
                run.ring_name
            )
            .map_err(io::Error::other)?;
        }
        write_atomic(root, "runs", &text)
    }

    pub fn add(&mut self, root: &Path, run: RegisteredRun) -> io::Result<()> {
        validate_ring_name(&run.ring_name)?;
        if self
            .runs
            .iter()
            .any(|existing| existing.ring_name == run.ring_name)
        {
            return Ok(());
        }
        if self.runs.len() == RUNS_MAX {
            let oldest = &self.runs[0];
            if oldest.identity.recording != run.identity.recording {
                return Err(invalid());
            }
            if !is_retired(root, &oldest.identity, &oldest.ring_name)? {
                self.evictions = self.evictions.checked_add(1).ok_or_else(invalid)?;
                eprintln!("recording retention: unsealed ring evicted; extent unknown");
            }
            let evicted = self.runs.remove(0);
            match unlink_generation(&evicted.ring_name) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.runs.push(run);
        assert!(self.runs.len() <= RUNS_MAX);
        Ok(())
    }

    pub fn capture_previous(
        &mut self,
        root: &Path,
        place: &Place,
        recording: Identity,
    ) -> io::Result<()> {
        let Some(name) = previous_generation(place)? else {
            return Ok(());
        };
        let name = name.as_str();
        if self.runs.iter().any(|run| run.ring_name == name) {
            return Ok(());
        }
        let Some(identity) = capture_previous_identity(root, name, recording)? else {
            return Ok(());
        };
        if identity.recording != recording {
            eprintln!("recording recovery: previous ring belongs to another recording; retained");
            return Ok(());
        }
        self.add(
            root,
            RegisteredRun {
                identity,
                ring_name: name.to_owned(),
                collector_pid: 0,
            },
        )
    }
}

fn capture_previous_identity(
    root: &Path,
    name: &str,
    recording: Identity,
) -> io::Result<Option<RunIdentity>> {
    match RingReader::open_generation(name) {
        Ok(reader) => {
            if let Some(identity) = reader.identity() {
                return Ok(Some(identity));
            }
            if let Some(retired) = retired_identity(root, name)? {
                if retired.recording != recording {
                    return Err(invalid());
                }
                return Ok(Some(retired));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => eprintln!("recording recovery unavailable: {error}"),
    }
    Ok(Some(RunIdentity {
        recording,
        run: random_identity()?,
        boot: Identity([0, 0]),
    }))
}

fn is_retired(root: &Path, identity: &RunIdentity, name: &str) -> io::Result<bool> {
    match retired_identity(root, name)? {
        Some(retired) if retired == *identity => Ok(true),
        Some(_) => Err(invalid()),
        None => Ok(false),
    }
}

fn retired_identity(root: &Path, name: &str) -> io::Result<Option<RunIdentity>> {
    let mut retired_path = [0; private::PATH_MAX];
    let retired = private::join_path(root, "retired", &mut retired_path)?;
    validate_ring_name(name)?;
    let filename = &name[1..];
    let mut path_buffer = [0; private::PATH_MAX];
    let path = private::join_path(retired, filename, &mut path_buffer)?;
    let mut file = match private::open_private_file(path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = [0; 257];
    let mut length = 0;
    while length < bytes.len() {
        let count = file.read(&mut bytes[length..])?;
        if count == 0 {
            break;
        }
        length += count;
    }
    if length == bytes.len() {
        return Err(invalid());
    }
    let text = std::str::from_utf8(&bytes[..length]).map_err(|_| invalid())?;
    let mut lines = text.lines();
    if lines.next() != Some("flightdeck-retired-1") {
        return Err(invalid());
    }
    let run = parse_run_view(lines.next().ok_or_else(invalid)?)?;
    if run.ring_name != name {
        return Err(invalid());
    }
    if lines.next().is_some() {
        return Err(invalid());
    }
    Ok(Some(run.identity))
}

fn retire_receipt(root: &Path, run: &RegisteredRunView<'_>) -> io::Result<()> {
    let mut directory_path = [0; private::PATH_MAX];
    let directory = private::join_path(root, "retired", &mut directory_path)?;
    private::create_private_dir(directory)?;
    if is_retired(root, &run.identity, run.ring_name)? {
        return Ok(());
    }
    let mut buffer = [0; 256];
    let mut target = Cursor::new(buffer.as_mut_slice());
    writeln!(target, "flightdeck-retired-1")?;
    write_run(&mut target, run)?;
    let length = usize::try_from(target.position()).map_err(|_| invalid())?;
    validate_ring_name(run.ring_name)?;
    let filename = &run.ring_name[1..];
    private::atomic_write(directory, filename, &buffer[..length])
}

fn write_run(target: &mut Cursor<&mut [u8]>, run: &RegisteredRunView<'_>) -> io::Result<()> {
    writeln!(
        target,
        "{} {} {} {} {}",
        identity_text(run.identity.recording),
        identity_text(run.identity.run),
        identity_text(run.identity.boot),
        run.collector_pid,
        run.ring_name
    )
}

fn parse_run_view(line: &str) -> io::Result<RegisteredRunView<'_>> {
    let mut fields = line.split(' ');
    let mut next = || fields.next().ok_or_else(invalid);
    let recording = parse_identity(next()?)?;
    let run = parse_identity(next()?)?;
    let boot = parse_identity(next()?)?;
    let collector_pid = next()?.parse().map_err(|_| invalid())?;
    let ring_name = next()?;
    if fields.next().is_some() {
        return Err(invalid());
    }
    validate_ring_name(ring_name)?;
    Ok(RegisteredRunView {
        identity: RunIdentity {
            recording,
            run,
            boot,
        },
        ring_name,
        collector_pid,
    })
}

fn validate_ring_name(name: &str) -> io::Result<()> {
    if !(2..=31).contains(&name.len()) {
        return Err(invalid());
    }
    if !name.starts_with('/') {
        return Err(invalid());
    }
    if !name[1..]
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    {
        return Err(invalid());
    }
    Ok(())
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;
    use std::io::Write;

    #[test]
    fn marks_a_drained_source_complete_without_allocating_or_untracking_its_ring() {
        // Goal: durable retirement changes bounded metadata and preserves its source ring.
        // Method: refuse its receipt replacement, retry, then capture the same source.
        let root =
            std::env::temp_dir().join(format!("flightdeck-registry-retire-{}", std::process::id()));
        private::create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: "/fdr.".into(),
        };
        let shape = crate::ring::Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 2,
        };
        let (writer, status) = super::super::Registration::start(&root, &place, shape, 0)
            .unwrap()
            .into_parts();
        status.unwrap();
        let identity = writer.identity().unwrap();
        let name = writer.name().to_owned();
        drop(writer);
        private::create_private_dir(&root.join("retired")).unwrap();
        let pending = root.join("retired").join(format!("{}.pending", &name[1..]));
        private::create_private_dir(&pending).unwrap();
        let refused_calls = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::InvalidData,
                Registry::retire(&root, &identity).unwrap_err().kind()
            );
        });
        assert!(!is_retired(&root, &identity, &name).unwrap());
        assert_eq!(1, Registry::read(&root).unwrap().runs.len());
        std::fs::remove_dir(&pending).unwrap();

        let calls = allocator_calls_in(|| {
            Registry::retire(&root, &identity).unwrap();
            Registry::retire(&root, &identity).unwrap();
            assert!(Registry::completed(&root, &identity, &name).unwrap());
        });

        assert_eq!(0, calls);
        assert_eq!(0, refused_calls);
        let mut registry = Registry::read(&root).unwrap();
        assert_eq!(1, registry.runs.len());
        registry
            .capture_previous(&root, &place, identity.recording)
            .unwrap();
        assert_eq!(1, registry.runs.len());
        assert_eq!(
            Some(identity),
            RingReader::open_generation(&name).unwrap().identity()
        );
        unlink_generation(&name).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owning_and_borrowed_registry_reads_refuse_the_same_malformed_pid() {
        // Goal: startup and polling validate precisely the same registry language.
        // Method: feed a negative process identifier to both public read boundaries.
        let root =
            std::env::temp_dir().join(format!("flightdeck-registry-parser-{}", std::process::id()));
        private::create_private_dir(&root).unwrap();
        let path = root.join("runs");
        let mut file = private::open_private_file(&path, true).unwrap();
        writeln!(
            file,
            "flightdeck-runs-1\n0\n{} {} {} -1 /fd.abcdefghijkl.1",
            identity_text(Identity([1, 2])),
            identity_text(Identity([3, 4])),
            identity_text(Identity([5, 6]))
        )
        .unwrap();
        let mut buffer = [0; REGISTRY_BYTES_MAX + 1];

        assert_eq!(
            io::ErrorKind::InvalidData,
            Registry::read(&root).unwrap_err().kind()
        );
        let calls = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::InvalidData,
                Registry::read_into(&path, &mut buffer)
                    .err()
                    .unwrap()
                    .kind()
            );
        });

        assert_eq!(0, calls);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_retired_source_without_header_identity_is_not_assigned_another_run() {
        // Goal: retirement also converges for layout-two and anonymous sources.
        // Method: assign an anonymous source once, retire it, then capture the same name.
        let root = std::env::temp_dir().join(format!(
            "flightdeck-retired-anonymous-{}",
            std::process::id()
        ));
        private::create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: "/fdra.".into(),
        };
        let shape = crate::ring::Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 2,
        };
        let writer = crate::ring::LockedPlace::acquire(&place)
            .unwrap()
            .create_unregistered(shape, 71)
            .unwrap();
        let name = writer.name().to_owned();
        let mut registry = Registry::default();
        registry
            .capture_previous(&root, &place, Identity([1, 2]))
            .unwrap();
        let identity = registry.runs[0].identity;
        registry.write(&root).unwrap();
        drop(writer);
        Registry::retire(&root, &identity).unwrap();
        let mut registry = Registry::read(&root).unwrap();

        registry
            .capture_previous(&root, &place, Identity([1, 2]))
            .unwrap();

        assert_eq!(1, registry.runs.len());
        assert_eq!(identity, registry.runs[0].identity);
        unlink_generation(&name).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_a_registry_in_caller_storage_without_allocating() {
        // Goal: polling the current runs does not allocate in collector library code.
        // Method: read a private registry and inspect its borrowed run under counting.
        let path =
            std::env::temp_dir().join(format!("flightdeck-registry-buffer-{}", std::process::id()));
        let mut file = private::open_private_file(&path, true).unwrap();
        file.write_all(
            concat!(
                "flightdeck-runs-1\n7\n",
                "00000000000000010000000000000002 ",
                "00000000000000030000000000000004 ",
                "00000000000000050000000000000006 42 /fd.abcdefghijkl.1\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let mut buffer = [0; 4097];

        let calls = allocator_calls_in(|| {
            let registry = Registry::read_into(&path, &mut buffer).unwrap();
            assert_eq!(7, registry.evictions().unwrap());
            let mut runs = registry.runs();
            let run = runs.next().unwrap().unwrap();
            assert_eq!(Identity([3, 4]), run.identity.run);
            assert_eq!(42, run.collector_pid);
            assert_eq!("/fd.abcdefghijkl.1", run.ring_name);
            assert!(runs.next().is_none());
        });

        assert_eq!(0, calls);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn refuses_a_malformed_identity_without_allocating() {
        // Goal: rejected registry data does not allocate in the polling path either.
        // Method: count a read whose first identity is not hexadecimal.
        let path =
            std::env::temp_dir().join(format!("flightdeck-registry-refuse-{}", std::process::id()));
        let mut file = private::open_private_file(&path, true).unwrap();
        file.write_all(
            concat!(
                "flightdeck-runs-1\n0\n",
                "x0000000000000010000000000000002 ",
                "00000000000000030000000000000004 ",
                "00000000000000050000000000000006 42 /fd.abcdefghijkl.1\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let mut buffer = [0; 4097];

        let calls = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::InvalidData,
                Registry::read_into(&path, &mut buffer)
                    .err()
                    .unwrap()
                    .kind()
            );
        });

        std::fs::remove_file(path).unwrap();
        assert_eq!(0, calls);
    }

    #[test]
    fn refuses_an_oversized_registry_without_allocating() {
        // Goal: an oversized registry is refused before parsing or growing its storage.
        // Method: read one byte beyond the registry limit into the fixed overflow byte.
        let path = std::env::temp_dir().join(format!(
            "flightdeck-registry-oversize-{}",
            std::process::id()
        ));
        let mut file = private::open_private_file(&path, true).unwrap();
        file.write_all(&[b'x'; 4097]).unwrap();
        let mut buffer = [0; 4097];

        let calls = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::InvalidData,
                Registry::read_into(&path, &mut buffer)
                    .err()
                    .unwrap()
                    .kind()
            );
        });

        std::fs::remove_file(path).unwrap();
        assert_eq!(0, calls);
    }
}
