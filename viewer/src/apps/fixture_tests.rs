use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::machine::Memory;
use collector::processes::{Process, Task};
use collector::recording::{RegisteredRun, Registry, private};
use collector::ring::{
    DictionaryCapacity, Dropped, Identity, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessIdentity, RunIdentity, Shape, UsageAvailability, Writer,
};
use persistence::engine::{Engine, Row};
use persistence::{Manifest, Store};

use super::tests::usage;
use super::*;

static NEXT: AtomicU64 = AtomicU64::new(0);
const PATHS: [&[u8]; 5] = [
    b"/Applications/App A.app/Contents/Helpers/Nested.app/Contents/MacOS/helper\xff",
    b"/Applications/App A.app/Contents/MacOS/main",
    b"/Other/App A.app/Contents/MacOS/main",
    b"/usr/local/bin/claude",
    b"/opt/bin/claude",
];
const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 5,
    coalition_capacity: 1,
    event_capacity: 1,
};

struct Fixture {
    root: PathBuf,
    ring: String,
    manifest: Option<Manifest>,
}

struct Recorded {
    publication: u64,
    pid: i32,
    creation: ProcessCreation,
    name: Vec<u8>,
    app: Option<Vec<u8>>,
    usage: Usage,
}

#[test]
fn reopens_real_v4_publications_after_writer_exit_and_aggregates_original_captured_metadata() {
    // Goal: saved App totals use captured metadata after the source writer has exited.
    // Method: publish through v4, save with the worker, drop writer, reopen Parquet and aggregate.
    let mut fixture = Fixture::new();
    fixture.record();
    let manifest = fixture.manifest.as_ref().unwrap();
    let connection = crate::data::connection();
    let mut records = Vec::with_capacity(10);
    fixture.read(&connection, &mut records);
    let mut clocks = Vec::with_capacity(2);
    fixture.clocks(&connection, &mut clocks);
    let programs: Vec<_> = records
        .iter()
        .map(|record| ProgramBytes {
            executable_name: &record.name,
            app_path: record.app.as_deref(),
        })
        .collect();
    let observations: Vec<_> = records
        .iter()
        .zip(&programs)
        .map(|(record, program)| ProcessObservation {
            stamp: Stamp {
                boot: manifest.identity.run.boot.0,
                run: manifest.identity.run.run.0,
                publication: record.publication,
                continuous_ns: Some(clocks[usize::try_from(record.publication - 1).unwrap()].0),
                awake_ns: Some(clocks[usize::try_from(record.publication - 1).unwrap()].1),
            },
            pid: record.pid,
            creation: Some(record.creation),
            program: Ok(program),
            usage: Ok(&record.usage),
            gpu_ns: None,
            threads: None,
            state: ProcessState::Live,
        })
        .collect();
    let interval = Interval {
        previous: observations[0].stamp,
        selected: observations[5].stamp,
        observations_complete: true,
        events_complete: true,
    };
    let spans: Vec<_> = (0..5)
        .map(|index| ProcessInterval {
            previous: Some(&observations[index]),
            selected: &observations[index + 5],
            start: None,
        })
        .collect();
    let mut result = tests::aggregate();

    result.compute(&interval, &spans).unwrap();

    assert_fixture(&result);
}

fn assert_fixture(result: &Aggregate) {
    let app = &result.apps[&AppKey::Bundle(b"/Applications/App A.app")];
    assert_eq!(
        cpu_percent_one_core(CpuRate {
            cpu_ns: app.cpu_ns.known,
            elapsed_ns: result.elapsed_ns.unwrap()
        }),
        Some(50.0)
    );
    assert_eq!(app.energy_nj.known, 3_000_000_000);
    assert!((energy_joules(app.energy_nj.known) - 3.0).abs() < f64::EPSILON);
    assert_eq!(
        energy_watts(EnergyRate {
            energy_nj: app.energy_nj.known,
            elapsed_ns: result.elapsed_ns.unwrap()
        }),
        Some(3.0)
    );
    assert_eq!(app.resident_bytes.known, 157_286_400);
    assert_eq!(app.members, 2);
    assert_eq!(result.apps[&AppKey::Bundle(b"/Other/App A.app")].members, 1);
    assert_eq!(result.apps[&AppKey::Executable(b"claude")].members, 2);
    assert_eq!(
        result.processes[0].executable_name,
        Some(b"helper\xff".as_slice())
    );
    assert_eq!(result.processes[0].creation.unwrap().unique_id, 700);
    assert_eq!(result.processes[0].cpu_ns, Amount::Known(200_000_000));
    assert_eq!(result.processes[1].cpu_ns, Amount::Known(300_000_000));
}

impl Fixture {
    fn new() -> Box<Self> {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-app-totals-{}-{serial}",
            std::process::id(),
        ));
        private::create_private_dir(&root).unwrap();
        Box::new(Self {
            root,
            ring: String::new(),
            manifest: None,
        })
    }

    fn record(&mut self) {
        let store = Store::open(&self.root).unwrap();
        let identity = RunIdentity {
            recording: store.recording(),
            run: Identity([3, 4]),
            boot: Identity([1, 2]),
        };
        let place = Place {
            lock_path: self.root.join("ring.lock"),
            name_prefix: format!(
                "/fdapps{}x{}",
                std::process::id(),
                NEXT.load(Ordering::Relaxed)
            ),
        };
        let mut writer = Writer::create_with_identity(
            &place,
            SHAPE,
            DictionaryCapacity {
                entries: 5,
                arena_bytes: 4096,
            },
            &identity,
            0,
        )
        .unwrap();
        self.ring = writer.name().to_owned();
        Registry {
            runs: vec![RegisteredRun {
                identity,
                ring_name: self.ring.clone(),
                collector_pid: 0,
            }],
            evictions: 0,
        }
        .write(&self.root)
        .unwrap();
        publish(&mut writer, 0);
        publish(&mut writer, 1);
        writer.finish();
        drop(writer);
        let registered = RegisteredRun {
            identity,
            ring_name: self.ring.clone(),
            collector_pid: 0,
        };
        persistence::worker::seal_held_windows(
            &store,
            &registered,
            None,
            &mut persistence::worker::HeldWindows::default(),
            &crate::live::identity_fixture::Underived,
        )
        .unwrap();
        self.manifest = Some(crate::live::identity_fixture::saved_window(
            &self.root, &identity, 0,
        ));
    }

    fn read(&self, connection: &Engine, output: &mut Vec<Recorded>) {
        let path = self.table("processes");
        let source = persistence::query::table_source(
            connection,
            persistence::schema::Table::Processes,
            persistence::query::TableSource::Parquet(&path),
        )
        .unwrap();
        let frame = connection
            .query(&format!(
                "SELECT publication,pid,process_unique_id,pid_version,executable_name,app_path,\
             user_ns,system_ns,energy_nj,resident_bytes,footprint_bytes,awake_ns_at_start \
             FROM {source} ORDER BY publication,pid",
            ))
            .unwrap();
        assert_eq!(frame.height(), 10);
        for index in 0..10 {
            let row = Row::new(&frame, index).unwrap();
            let mut usage = usage(
                row.get(6).unwrap(),
                row.get(8).unwrap(),
                row.get(9).unwrap(),
            );
            usage.system_ns = row.get(7).unwrap();
            usage.footprint_bytes = row.get(10).unwrap();
            usage.awake_ns_at_start = row.get(11).unwrap();
            output.push(Recorded {
                publication: row.get(0).unwrap(),
                pid: row.get(1).unwrap(),
                creation: ProcessCreation {
                    unique_id: row.get(2).unwrap(),
                    pid_version: row.get(3).unwrap(),
                },
                name: row.get(4).unwrap(),
                app: row.get(5).unwrap(),
                usage,
            });
        }
    }

    fn clocks(&self, connection: &Engine, output: &mut Vec<(u64, u64)>) {
        let frame = connection
            .query(&format!(
                "SELECT continuous_ns,awake_ns FROM read_parquet('{}') ORDER BY publication",
                self.table("ticks").to_str().unwrap(),
            ))
            .unwrap();
        assert_eq!(frame.height(), 2);
        for index in 0..2 {
            let row = Row::new(&frame, index).unwrap();
            output.push((row.get(0).unwrap(), row.get(1).unwrap()));
        }
    }

    fn table(&self, name: &str) -> PathBuf {
        self.root
            .join(&self.manifest.as_ref().unwrap().data_directory)
            .join(format!("{name}.parquet"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.ring.is_empty() {
            collector::ring::unlink_generation(&self.ring).unwrap();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn publish(writer: &mut Writer, second: u64) {
    let mut processes = Vec::with_capacity(5);
    let mut identities = Vec::with_capacity(5);
    publish_processes(writer, second, &mut processes, &mut identities);
    writer
        .publish_with_identity(
            &collector::ring::Readings {
                run_tick: second,
                cpus: &[],
                memory: &Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &processes,
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: 1_800_000_000_000_000_000 + second * 1_000_000_000,
                continuous_ns: 1_000_000_000 + second * 1_000_000_000,
                awake_ns: 1_000_000_000 + second * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
}

fn publish_processes(
    writer: &mut Writer,
    second: u64,
    processes: &mut Vec<Process>,
    identities: &mut Vec<IdentityObservation>,
) {
    for (index, path) in PATHS.iter().enumerate() {
        let mut captured = None;
        collector::programs::split_program(path, &mut captured).unwrap();
        let mut reference = None;
        writer
            .intern_program(&captured.unwrap(), &mut reference)
            .unwrap();
        let pid = i32::try_from(index + 7).unwrap();
        let delta_cpu = [
            200_000_000,
            300_000_000,
            100_000_000,
            10_000_000,
            20_000_000,
        ][index];
        let delta_energy = [1_000_000_000, 2_000_000_000, 100, 200, 300][index];
        let resident = [104_857_600, 52_428_800, 1000, 2000, 3000][index];
        let mut usage = usage(
            9_007_199_254_740_993 + delta_cpu * second,
            9_007_199_254_741_993 + delta_energy * second,
            resident,
        );
        usage.system_ns = 9_007_199_254_740_000;
        processes.push(Process {
            pid,
            usage,
            task: Task {
                threads: 1,
                faults: 0,
                context_switches: 0,
                mach_syscalls: 0,
                unix_syscalls: 0,
            },
        });
        identities.push(IdentityObservation {
            pid,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: u64::try_from(pid).unwrap() * 100,
                    pid_version: pid,
                },
                program: reference.unwrap(),
            },
        });
    }
}
