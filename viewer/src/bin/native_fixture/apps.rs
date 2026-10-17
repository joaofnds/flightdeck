use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::recording::{Registration, identity_text, private, random_identity};
use collector::ring::{
    DictionaryCapacity, Dropped, IdentityObservation, IdentityPublication, ObservationTimes, Place,
    ProcessCreation, ProcessIdentity, ProgramBytes, Readings, Shape, UsageAvailability, Writer,
};

#[path = "apps/paging.rs"]
mod paging;

pub(super) fn run(checks: &Path) -> io::Result<()> {
    run_scenario(checks, Scenario::Quantities)?;
    run_scenario(checks, Scenario::Paging)
}

fn run_scenario(checks: &Path, scenario: Scenario) -> io::Result<()> {
    let mut fixture = None;
    Fixture::initialize(&mut fixture, scenario)?;
    let fixture = fixture.as_mut().unwrap_or_else(|| unreachable!());
    match scenario {
        Scenario::Quantities => fixture.publish()?,
        Scenario::Paging => paging::publish(fixture)?,
    }

    Check::run(checks, &fixture.root, scenario.flag())
}

#[derive(Clone, Copy)]
enum Scenario {
    Quantities,
    Paging,
}

impl Scenario {
    fn shape(self) -> Shape {
        match self {
            Self::Quantities => SHAPE,
            Self::Paging => Shape {
                process_capacity: 1026,
                ..SHAPE
            },
        }
    }

    fn capacity(self) -> DictionaryCapacity {
        match self {
            Self::Quantities => DictionaryCapacity {
                entries: 2,
                arena_bytes: 128,
            },
            Self::Paging => DictionaryCapacity {
                entries: 513,
                arena_bytes: 65_536,
            },
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Self::Quantities => "--live-apps-fixture",
            Self::Paging => "--live-apps-paging-fixture",
        }
    }
}

struct Fixture {
    root: PathBuf,
    writer: Option<Writer>,
    identities: Vec<IdentityObservation>,
    processes: Vec<Process>,
}

const SHAPE: Shape = Shape {
    slot_count: 4,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 2,
    coalition_capacity: 1,
    event_capacity: 4,
};

impl Fixture {
    fn initialize(output: &mut Option<Box<Self>>, scenario: Scenario) -> io::Result<()> {
        let token = identity_text(random_identity()?).to_string();
        let root = std::env::temp_dir().join(format!("flightdeck-native-apps-fixture-{token}"));
        if root.try_exists()? {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        private::create_private_dir(&root)?;
        let capacity =
            usize::try_from(scenario.shape().process_capacity).map_err(io::Error::other)?;
        *output = Some(Box::new(Self {
            root,
            writer: None,
            identities: Vec::with_capacity(capacity),
            processes: Vec::with_capacity(capacity),
        }));
        let fixture = output.as_mut().unwrap_or_else(|| unreachable!());
        let place = Place {
            lock_path: fixture.root.join("ring.lock"),
            name_prefix: format!("/fdna{}", std::process::id()),
        };
        let (writer, registered) = Registration::start_with_identity(
            &fixture.root,
            &place,
            scenario.shape(),
            scenario.capacity(),
            0,
        )?
        .into_parts();
        fixture.writer = Some(writer);
        registered?;
        match scenario {
            Scenario::Quantities => fixture.initialize_members()?,
            Scenario::Paging => paging::initialize_members(fixture)?,
        }

        Ok(())
    }

    fn initialize_members(&mut self) -> io::Result<()> {
        let writer = self.writer.as_mut().unwrap_or_else(|| unreachable!());
        for (index, executable_name) in [b"main\xff".as_slice(), b"helper\xff"]
            .into_iter()
            .enumerate()
        {
            let mut program = None;
            writer
                .intern_program(
                    &ProgramBytes {
                        executable_name,
                        app_path: Some(b"/Applications/Example\xff.app"),
                    },
                    &mut program,
                )
                .map_err(|reason| {
                    io::Error::other(format!("fixture program unavailable: {reason:?}"))
                })?;
            let program =
                program.ok_or_else(|| io::Error::other("fixture dictionary exhausted"))?;
            self.identities.push(IdentityObservation {
                pid: 42 + i32::try_from(index).map_err(io::Error::other)?,
                usage: UsageAvailability::Measured,
                identity: ProcessIdentity::Measured {
                    creation: ProcessCreation {
                        unique_id: 9_007_199_254_740_993
                            + u64::try_from(index).map_err(io::Error::other)?,
                        pid_version: [-70, 71][index],
                    },
                    program,
                },
            });
        }

        Ok(())
    }

    fn publish(&mut self) -> io::Result<()> {
        for second in 1..=3 {
            self.publish_processes(second)?;
            if second == 3 {
                self.identities.clear();
            }
            self.publish_observation(second)?;
        }

        Ok(())
    }

    fn publish_observation(&mut self, second: u64) -> io::Result<()> {
        let writer = self.writer.as_mut().unwrap_or_else(|| unreachable!());
        writer.publish_with_identity(
            &Readings {
                run_tick: second,
                cpus: &[],
                memory: &Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &self.processes,
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: (999 + second) * 1_000_000_000,
                continuous_ns: second * 1_000_000_000,
                awake_ns: second * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &self.identities,
                events: &[],
            },
        )
    }

    fn publish_processes(&mut self, second: u64) -> io::Result<()> {
        self.processes.clear();
        if second == 3 {
            return Ok(());
        }

        for index in 0..2 {
            let resident_bytes = if second == 1 {
                [83_886_080, 41_943_040][index]
            } else {
                [104_857_600, 52_428_800][index]
            };
            let footprint_bytes = if second == 1 {
                [90_000_000, 45_000_000][index]
            } else {
                [110_000_000, 55_000_000][index]
            };
            self.processes.push(Process {
                pid: 42 + i32::try_from(index).map_err(io::Error::other)?,
                usage: Usage {
                    awake_ns_at_start: 17,
                    user_ns: 9_007_199_254_740_993 + [200_000_000, 300_000_000][index] * second,
                    system_ns: 4_000_000_000_000_000,
                    energy_nj: 9_007_199_254_741_993
                        + [1_000_000_000, 2_000_000_000][index] * second,
                    instructions: 0,
                    super_instructions: 0,
                    cycles: 0,
                    idle_wakeups: 0,
                    interrupt_wakeups: 0,
                    pageins: 0,
                    resident_bytes,
                    footprint_bytes,
                    disk_read_bytes: 0,
                    disk_written_bytes: 0,
                },
                task: Task {
                    threads: 1,
                    faults: 0,
                    context_switches: 0,
                    mach_syscalls: 0,
                    unix_syscalls: 0,
                },
            });
        }

        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut writer) = self.writer.take() {
            writer.finish();
            let name = writer.name().to_owned();
            drop(writer);
            if let Err(error) = collector::ring::unlink_generation(&name) {
                eprintln!("native App fixture ring cleanup: {error}");
            }
        }
        super::fixture_remove(&self.root);
    }
}

struct Check {
    child: Option<Child>,
}

impl Check {
    fn run(checks: &Path, root: &Path, flag: &'static str) -> io::Result<()> {
        let mut check = Self {
            child: Some(Command::new(checks).arg(flag).arg(root).spawn()?),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        for _attempt in 0..3000 {
            if let Some(status) = check
                .child
                .as_mut()
                .unwrap_or_else(|| unreachable!())
                .try_wait()?
            {
                drop(check.child.take());
                if !status.success() {
                    return Err(io::Error::other(format!(
                        "native App worker fixture failed: {status}"
                    )));
                }

                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native App worker fixture deadline",
        ))
    }
}

impl Drop for Check {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(error) = child.kill() {
                eprintln!("native App check termination: {error}");
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            for _attempt in 0..200 {
                match child.try_wait() {
                    Ok(Some(_status)) => return,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => {
                        eprintln!("native App check reap: {error}");
                        return;
                    }
                }
            }
            eprintln!("native App check did not reap within two seconds");
        }
    }
}
