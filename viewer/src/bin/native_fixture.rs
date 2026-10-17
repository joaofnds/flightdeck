//! Opt-in native worker contract fixture, launched only by the native check command.
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use collector::machine::Memory;
use collector::recording::{Registration, identity_text, private, random_identity};
use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, Writer};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("native_fixture: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--checks")) {
        return Err(io::Error::other("native_fixture --checks EXECUTABLE"));
    }
    let checks = arguments
        .next()
        .ok_or_else(|| io::Error::other("missing checks executable"))?;
    if arguments.next().is_some() {
        return Err(io::Error::other("native_fixture --checks EXECUTABLE"));
    }
    let mut fixture = Fixture::new()?;
    fixture.publish()?;
    let status = Command::new(checks)
        .arg("--live-fixture")
        .arg(&fixture.root)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "native worker fixture failed: {status}"
        )));
    }
    Ok(())
}

struct Fixture {
    root: PathBuf,
    writer: Option<Writer>,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let token = identity_text(random_identity()?).to_string();
        let root = std::env::temp_dir().join(format!("flightdeck-native-fixture-{token}"));
        if root.try_exists()? {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        private::create_private_dir(&root)?;

        let mut fixture = Self { root, writer: None };
        let place = Place {
            lock_path: fixture.root.join("ring.lock"),
            name_prefix: format!("/fdnf{}", std::process::id()),
        };
        let shape = Shape {
            slot_count: 4,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 4,
        };
        let (writer, registered) =
            Registration::start(&fixture.root, &place, shape, 0)?.into_parts();
        fixture.writer = Some(writer);
        registered?;
        Ok(fixture)
    }

    fn publish(&mut self) -> io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("missing fixture writer"))?;
        for (index, second) in [1000, 900, 900].into_iter().enumerate() {
            let tick = u64::try_from(index).map_err(io::Error::other)? + 1;
            let memory = Memory {
                total_bytes: tick * 111,
                ..Memory::default()
            };
            writer.publish_observed(
                &Readings {
                    run_tick: tick,
                    cpus: &[],
                    memory: &memory,
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: &[],
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                &ObservationTimes {
                    wall_ns: second * 1_000_000_000,
                    continuous_ns: tick * 1_000_000_000,
                    awake_ns: tick * 1_000_000_000,
                },
            );
        }
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let name = writer.name().to_owned();
            drop(writer);
            if let Err(error) = collector::ring::unlink_generation(&name) {
                eprintln!("native fixture ring cleanup: {error}");
            }
        }
        fixture_remove(&self.root);
    }
}

fn fixture_remove(root: &Path) {
    if let Err(error) = std::fs::remove_dir_all(root) {
        eprintln!("native fixture directory cleanup: {error}");
    }
}
