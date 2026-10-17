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
    let status = Command::new(&checks)
        .arg("--live-fixture")
        .arg(&fixture.root)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "native worker fixture failed: {status}"
        )));
    }
    drop(fixture);
    apps::run(Path::new(&checks))?;
    let mut cpu_fixture = Fixture::new()?;
    cpu_fixture.publish_cpu_checks()?;
    let status = Command::new(&checks)
        .arg("--live-cpu-fixture")
        .arg(&cpu_fixture.root)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "native CPU fixture failed: {status}"
        )));
    }
    drop(cpu_fixture);

    cpu_commit::run(Path::new(&checks))
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
            cpu_capacity: 2,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 2,
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

    fn publish_cpu_checks(&mut self) -> io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("missing fixture writer"))?;
        for tick in 1..=3_u32 {
            let (cpu0_busy, cpu1_busy) = if tick == 3 {
                (25, 275)
            } else {
                (0, tick * 100)
            };
            let cpus = [
                collector::machine::CPUTicks {
                    user: cpu0_busy,
                    idle: tick * 100 - cpu0_busy,
                    ..collector::machine::CPUTicks::default()
                },
                collector::machine::CPUTicks {
                    user: cpu1_busy,
                    idle: tick * 100 - cpu1_busy,
                    ..collector::machine::CPUTicks::default()
                },
            ];
            writer.publish_observed(
                &Readings {
                    run_tick: u64::from(tick),
                    cpus: &cpus,
                    memory: &Memory::default(),
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
                    wall_ns: u64::from(2000 + tick) * 1_000_000_000,
                    continuous_ns: u64::from(tick) * 1_000_000_000,
                    awake_ns: u64::from(tick) * 1_000_000_000,
                },
            );
        }
        Ok(())
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
            let fan_rpm = if index == 0 {
                [0.0, 4321.0]
            } else {
                [1234.5, 0.0]
            };
            let smc = if index == 1 {
                None
            } else {
                Some(collector::ring::SmcReadings {
                    power_watts: 1.0,
                    soc_celsius: 42.0,
                    fan_rpm: &fan_rpm,
                })
            };

            let cpus = fixture_cpus(u32::try_from(tick).map_err(io::Error::other)?);
            writer.publish_observed(
                &Readings {
                    run_tick: tick,
                    cpus: &cpus,
                    memory: &memory,
                    interfaces: &[],
                    ioreport: None,
                    smc,
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

fn fixture_cpus(tick: u32) -> [collector::machine::CPUTicks; 2] {
    assert!((1..=3).contains(&tick));
    [
        collector::machine::CPUTicks {
            user: tick * 25,
            idle: tick * 75,
            ..collector::machine::CPUTicks::default()
        },
        collector::machine::CPUTicks {
            user: tick * 75,
            idle: tick * 25,
            ..collector::machine::CPUTicks::default()
        },
    ]
}

fn fixture_remove(root: &Path) {
    if let Err(error) = std::fs::remove_dir_all(root) {
        eprintln!("native fixture directory cleanup: {error}");
    }
}

#[path = "native_fixture/apps.rs"]
mod apps;
#[path = "native_fixture/cpu_commit.rs"]
mod cpu_commit;

#[cfg(test)]
mod tests {
    use super::Fixture;
    use serde_json::json;

    #[test]
    fn supplies_genuine_zero_full_and_independent_cpu_intervals() {
        // Goal: native worker checks receive real independently varying CPU readings.
        // Method: exercise exact selection and following through the genuine Rust adapter.
        let mut fixture = Fixture::new().unwrap();
        fixture.publish_cpu_checks().unwrap();
        let mut session = viewer::LiveSession::open(&fixture.root).unwrap();
        session.select(2002, 0).unwrap();

        let pinned = session.cpu_detail().unwrap();
        let pinned_again = session.cpu_detail().unwrap();
        session.follow_latest().unwrap();
        let latest = session.cpu_detail().unwrap();

        assert_eq!(pinned, pinned_again);
        assert_eq!(pinned["observation"]["publication"], 2);
        assert_eq!(
            pinned["cpus"],
            json!([
                {"cpu":0,"busy_percent":0.0}, {"cpu":1,"busy_percent":100.0}
            ])
        );
        assert_eq!(latest["observation"]["publication"], 3);
        assert_eq!(
            latest["cpus"],
            json!([
                {"cpu":0,"busy_percent":25.0}, {"cpu":1,"busy_percent":75.0}
            ])
        );
    }
}
