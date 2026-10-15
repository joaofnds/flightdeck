use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use super::{identity_text, parse_identity, private, random_identity, write_atomic};
use crate::ring::{Identity, RingReader, RunIdentity, unlink_generation};

const REGISTRY_BYTES_MAX: u64 = 4_096;
const RUNS_MAX: usize = 2;

/// The registry plus one byte to distinguish a full valid file from an oversized one.
pub type RegistryBuffer = [u8; 4097];

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
            .ok_or_else(|| invalid("registry losses"))?
            .parse()
            .map_err(|_| invalid("registry losses"))
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
        if length > 4096 {
            return Err(invalid("registry too long"));
        }
        let text = std::str::from_utf8(&buffer[..length]).map_err(|_| invalid("registry text"))?;
        let view = RegistryView { text };
        let mut lines = text.lines();
        if lines.next() != Some("flightdeck-runs-1") {
            return Err(invalid("registry version"));
        }
        view.evictions()?;
        if lines.next().is_none() {
            return Err(invalid("registry losses"));
        }
        for (index, line) in lines.take(RUNS_MAX + 1).enumerate() {
            if index == RUNS_MAX {
                return Err(invalid("too many registered rings"));
            }
            parse_run_view(line)?;
        }
        Ok(view)
    }

    pub fn read(root: &Path) -> io::Result<Self> {
        match private::open_private_file(&root.join("runs"), false) {
            Ok(mut file) => {
                let mut text = String::new();
                file.by_ref()
                    .take(REGISTRY_BYTES_MAX + 1)
                    .read_to_string(&mut text)?;
                if u64::try_from(text.len()).map_err(io::Error::other)? > REGISTRY_BYTES_MAX {
                    return Err(invalid("registry too long"));
                }
                Self::parse(&text)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
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

    pub fn add(&mut self, run: RegisteredRun) -> io::Result<()> {
        validate_ring_name(&run.ring_name)?;
        if self
            .runs
            .iter()
            .any(|existing| existing.ring_name == run.ring_name)
        {
            return Ok(());
        }
        if self.runs.len() == RUNS_MAX {
            self.evictions = self
                .evictions
                .checked_add(1)
                .ok_or_else(|| invalid("loss count"))?;
            eprintln!(
                "recording retention: retired ring evicted; any unsealed history is unavailable"
            );
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

    pub fn capture_previous(&mut self, lock_path: &Path, recording: Identity) -> io::Result<()> {
        let mut file = match File::open(lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut name = String::new();
        file.by_ref().take(33).read_to_string(&mut name)?;
        let name = name.trim();
        if name.ends_with(".0") {
            return Ok(());
        }
        if self.runs.iter().any(|run| run.ring_name == name) {
            return Ok(());
        }
        validate_ring_name(name)?;
        let identity = match RingReader::open_generation(name) {
            Ok(reader) => reader.identity().unwrap_or(RunIdentity {
                recording,
                run: random_identity()?,
                boot: Identity([0, 0]),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                eprintln!("recording recovery unavailable: {error}");
                RunIdentity {
                    recording,
                    run: random_identity()?,
                    boot: Identity([0, 0]),
                }
            }
        };
        if identity.recording != recording {
            eprintln!("recording recovery: previous ring belongs to another recording; retained");
            return Ok(());
        }
        self.add(RegisteredRun {
            identity,
            ring_name: name.to_owned(),
            collector_pid: 0,
        })
    }

    fn parse(text: &str) -> io::Result<Self> {
        let mut lines = text.lines();
        if lines.next() != Some("flightdeck-runs-1") {
            return Err(invalid("registry version"));
        }
        let evictions = lines
            .next()
            .ok_or_else(|| invalid("registry losses"))?
            .parse()
            .map_err(io::Error::other)?;
        let mut registry = Self {
            runs: Vec::with_capacity(RUNS_MAX),
            evictions,
        };
        for line in lines.take(RUNS_MAX + 1) {
            if registry.runs.len() == RUNS_MAX {
                return Err(invalid("too many registered rings"));
            }
            registry.runs.push(parse_run(line)?);
        }
        Ok(registry)
    }
}

fn parse_run(line: &str) -> io::Result<RegisteredRun> {
    let mut fields = line.split(' ');
    let mut next = || fields.next().ok_or_else(|| invalid("registry field"));
    let recording = parse_identity(next()?)?;
    let run = parse_identity(next()?)?;
    let boot = parse_identity(next()?)?;
    let collector_pid = next()?.parse().map_err(io::Error::other)?;
    let ring_name = next()?.to_owned();
    if fields.next().is_some() {
        return Err(invalid("registry extra field"));
    }
    validate_ring_name(&ring_name)?;
    Ok(RegisteredRun {
        identity: RunIdentity {
            recording,
            run,
            boot,
        },
        ring_name,
        collector_pid,
    })
}

fn parse_run_view(line: &str) -> io::Result<RegisteredRunView<'_>> {
    let mut fields = line.split(' ');
    let mut next = || fields.next().ok_or_else(|| invalid("registry field"));
    let recording = parse_identity(next()?)?;
    let run = parse_identity(next()?)?;
    let boot = parse_identity(next()?)?;
    let collector_pid = next()?.parse().map_err(|_| invalid("registry process"))?;
    let ring_name = next()?;
    if fields.next().is_some() {
        return Err(invalid("registry extra field"));
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
        return Err(invalid("ring name length"));
    }
    if !name.starts_with('/') {
        return Err(invalid("ring name prefix"));
    }
    if !name[1..]
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    {
        return Err(invalid("ring name characters"));
    }
    Ok(())
}

fn invalid(_message: &'static str) -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;
    use std::io::Write;

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
