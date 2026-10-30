use std::sync::Arc;

use collector::machine::{CPUTicks, Memory};
use collector::processes::{Process, Task};
use collector::ring::{
    Dropped, IdentityObservation, IdentityPublication, ObservationTimes, ProcessCreation,
    ProcessIdentity, ProgramBytes, Readings, SmcReadings, UsageAvailability,
};

use crate::apps::Amount;
use crate::cancellation::Cancellation;
use crate::live::forged_source::ForgedSource;
use crate::live::identity_fixture::Fixture;
use crate::store::{ByteBudget, CompleteMachineState, startup_tests::usage};

#[test]
fn prepared_retries_keep_exact_cumulative_reader_losses() {
    // Goal: repeated admission callbacks and cancellation cannot count an event loss twice.
    // Method: overwrite two unread ticks of ten, the first four of five events each, and the
    // early events of the next two in a ring copy whose log claims fifty events, as a writer
    // before the event room guard could, then retry the same prepared tick.
    let mut fixture = Fixture::new();
    let mut forged = ForgedSource::new(&fixture.root, fixture.writer.name());
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let events = std::array::from_fn::<_, 5, _>(|_| collector::tracker::Event::Exec { pid: 7 });
    for step in 1..=4 {
        fixture.publish_with_events(100 + step, &events);
    }
    for step in 5..=10 {
        fixture.publish_with_events(100 + step, &[]);
    }
    forged.claim_event_newest(10 * 5);
    let losses = persistence::Losses {
        publications: 2,
        events: 5,
        unknown: false,
    };

    assert_eq!(
        ring.read_prepared(&budget, &operation, None, |_, _| panic!("missed tick"))
            .unwrap(),
        super::NativeRead::Missed
    );
    for _ in 0..2 {
        assert_eq!(
            ring.read_prepared(&budget, &operation, None, |_, prepared| {
                assert_eq!(prepared.observation.stamp.publication, 3);
                assert_eq!(prepared.apps.metadata.origin, super::Origin::Live(losses));
                false
            })
            .unwrap(),
            super::NativeRead::Blocked
        );
    }
    cancellation.cancel().unwrap();
    assert!(
        matches!(ring.read_prepared(&budget, &operation, None, |_, _| panic!("cancelled")),
        Err(super::PrepareError::Source(persistence::Error::Io(error)))
        if error.kind() == std::io::ErrorKind::Interrupted)
    );
    let operation = Arc::clone(&cancellation)
        .operation(cancellation.epoch())
        .unwrap();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        assert_eq!(prepared.apps.metadata.origin, super::Origin::Live(losses));
        true
    })
    .unwrap();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        assert_eq!(prepared.observation.stamp.publication, 4);
        assert_eq!(
            prepared.apps.metadata.origin,
            super::Origin::Live(persistence::Losses {
                publications: 2,
                events: 8,
                unknown: false,
            })
        );
        true
    })
    .unwrap();
}

#[test]
fn detachment_retains_native_origin_and_the_original_metadata_reservation() {
    // Goal: pinning facts preserves provenance and the owner charged for their App metadata.
    // Method: detach a prepared native second and drop the source and all original retainers.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1, Some(3.0), true);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut second = crate::store::AppsSecond::default();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        second = std::mem::take(&mut prepared.apps);
        true
    })
    .unwrap();
    let metadata = second.metadata.bytes();
    let payload = second.processes.as_ref().unwrap();
    let process_names: usize = payload
        .iter()
        .filter_map(crate::store::ProcessRow::executable_name)
        .map(crate::store::ProcessName::reserved_bytes)
        .sum();
    let payload = payload.bytes();
    let names: usize = second
        .apps
        .iter()
        .map(|app| app.name.reserved_bytes())
        .sum();

    let detached = second.detached(&budget).unwrap();
    drop(second);
    drop(ring);
    drop(fixture);

    assert_eq!(
        detached.metadata.origin,
        super::Origin::Live(persistence::Losses::default())
    );
    assert_eq!(
        budget.used(),
        detached.bytes() + metadata + payload + names + process_names
    );
    drop(detached);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_terminal_workspace_admission_retry_keeps_the_saved_predecessor() {
    // Goal: importing a saved predecessor charges its capacities before consuming it.
    // Method: leave only native workspace room, refuse import, then free room and retry C.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let outcome =
        crate::store::recording::load_payload(&fixture.root, &budget, &operation, |_, _, _| {})
            .unwrap();
    let terminal = outcome.terminal.unwrap();
    let imported = terminal.machine.heap_bytes() + terminal.process.as_ref().unwrap().bytes();
    let mut baseline =
        crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    baseline.follow().unwrap();
    let workspace = budget.used();
    drop(baseline);
    assert_eq!(budget.used(), 0);
    fixture.publish_with_system_rows(102);
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.accept(Some(terminal));
    let pressure = budget.reserve(budget.maximum() - workspace).unwrap();

    assert_eq!(ring.follow(), Err(crate::store::Failure::Budget));
    assert_eq!(budget.used(), pressure.bytes());
    drop(pressure);
    ring.resume().unwrap();
    assert!(budget.used() > workspace);
    assert!(budget.used() <= workspace + imported);
    let mut accepted = None;
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        assert_eq!(prepared.observation.stamp.publication, 3);
        assert!(prepared.machine.as_ref().unwrap().follows);
        accepted = Some(cpu_amount(prepared.apps()));
        true
    })
    .unwrap();

    assert_eq!(accepted, Some(Amount::Known(1)));
    drop(ring);
    assert_eq!(budget.used(), 0);
}

#[test]
fn an_oversized_valid_sample_keeps_its_measured_frame_for_the_next_publication() {
    // Goal: presentation capacity does not invalidate valid measured process input.
    // Method: B has 500 measured long-name processes; C keeps matching PID 7 and its counters.
    let mut fixture = Fixture::new_mode(true, 500, None);
    let executable = vec![b'x'; collector::ring::PROGRAM_PATH_BYTES_MAX];
    publish_many(
        &mut fixture,
        1,
        &Many::new(1, 7, &executable, b"/Apps/Current.app"),
    );
    publish_many(
        &mut fixture,
        2,
        &Many::new(500, 1, &executable, b"/Apps/Current.app"),
    );
    publish_many(
        &mut fixture,
        3,
        &Many::new(1, 7, &executable, b"/Apps/Current.app"),
    );
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut result = None;

    for step in 1..=3 {
        assert_eq!(
            ring.read_prepared(&budget, &operation, None, |_, prepared| {
                if step == 2 {
                    assert_eq!(
                        prepared.app_refusal(),
                        Some(super::Unavailable::Sample("live App sample bytes"))
                    );
                    assert!(matches!(
                        prepared.apps().missing,
                        Some(crate::store::Missing::Refused(_))
                    ));
                    assert!(prepared.raw().is_ok());
                }
                if step == 3 {
                    result = Some(cpu_amount(prepared.apps()));
                }
                true
            })
            .unwrap(),
            super::NativeRead::Accepted
        );
    }

    assert_eq!(result, Some(Amount::Known(100_000_000)));
}

#[test]
fn a_joint_programme_capacity_refusal_keeps_the_current_valid_input() {
    // Goal: joint work capacity stays distinct from a corrupt current Frame.
    // Method: valid large A/B Frames join past the model byte bound; C matches B's PID 7.
    let mut fixture = Fixture::new_mode(true, 3000, None);
    wide_events(&mut fixture);
    let executable = vec![b'x'; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let app = long_app();
    publish_many(&mut fixture, 1, &Many::new(3000, 5000, &executable, &app));
    let mut second = Many::new(3000, 1, &executable, &app);
    second.exits = 3000;
    publish_many(&mut fixture, 2, &second);
    publish_many(&mut fixture, 3, &Many::new(1, 7, &executable, &app));
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut result = None;

    for step in 1..=3 {
        assert_eq!(
            ring.read_prepared(&budget, &operation, None, |_, prepared| {
                if step == 2 {
                    assert_eq!(
                        prepared.app_refusal(),
                        Some(super::Unavailable::Aggregate(
                            crate::apps::Error::ProgramBound
                        ))
                    );
                    assert!(matches!(
                        prepared.apps().missing,
                        Some(crate::store::Missing::Refused(_))
                    ));
                    assert!(prepared.raw().is_ok());
                }
                if step == 3 {
                    result = Some(cpu_amount(prepared.apps()));
                }
                true
            })
            .unwrap(),
            super::NativeRead::Accepted
        );
    }

    assert_eq!(result, Some(Amount::Known(100_000_000)));
}

#[test]
fn genuine_model_integrity_failures_cannot_supply_a_predecessor() {
    // Goal: input integrity and working capacity remain distinct without matching error text.
    // Method: classify the original model variants retained by the shared capture boundary.
    use crate::apps::Error;
    use crate::live_apps::AggregateError;
    assert!(!AggregateError::Input(Error::InvalidProgram).retains_input());
    assert!(!AggregateError::Input(Error::InvalidEndpoint).retains_input());
    assert!(!AggregateError::Input(Error::DuplicateProcess).retains_input());
    assert!(AggregateError::Input(Error::ProgramBound).retains_input());
    assert!(AggregateError::Input(Error::ProcessBound).retains_input());
    assert!(AggregateError::View(Error::ProcessBound).retains_input());
}

fn cpu_amount(apps: &crate::store::AppsSecond) -> Amount {
    apps.processes
        .as_ref()
        .unwrap()
        .iter()
        .find(|process| process.pid == 7)
        .unwrap()
        .cpu_ns()
}

struct Many<'text> {
    count: u32,
    first_pid: i32,
    executable: &'text [u8],
    app: &'text [u8],
    exits: u32,
}

impl<'text> Many<'text> {
    fn new(count: u32, first_pid: i32, executable: &'text [u8], app: &'text [u8]) -> Self {
        Self {
            count,
            first_pid,
            executable,
            app,
            exits: 0,
        }
    }
}

fn publish_many(fixture: &mut Fixture, step: u64, many: &Many<'_>) {
    let mut program = None;
    publish_program(
        fixture,
        &ProgramBytes {
            executable_name: many.executable,
            app_path: Some(many.app),
        },
        &mut program,
    );
    let program = program.unwrap();
    let mut identities = Vec::new();
    publish_many_identities(many, &program, &mut identities);
    let processes = identities
        .iter()
        .map(|identity| Process {
            pid: identity.pid,
            usage: usage(step - 1),
            task: MEASURED_TASK,
        })
        .collect::<Vec<_>>();
    let events = (0..many.exits)
        .map(|index| collector::tracker::Event::Exit {
            pid: 5000 + i32::try_from(index).unwrap(),
            last: Ok(usage(step - 1)),
            noticed: collector::tracker::Noticed::OnExit,
        })
        .collect::<Vec<_>>();
    let event_identities = (0..many.exits)
        .map(|index| identity(5000 + i32::try_from(index).unwrap(), program))
        .collect::<Vec<_>>();
    let memory = Memory::default();
    let cpus = [CPUTicks {
        user: u32::try_from(step).unwrap(),
        idle: 10,
        system: 0,
        nice: 0,
    }];
    let readings = Readings {
        run_tick: step - 1,
        cpus: &cpus,
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &processes,
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &events,
        dropped: Dropped::default(),
    };
    let times = ObservationTimes {
        wall_ns: (1000 + step) * 1_000_000_000,
        continuous_ns: step * 1_000_000_000,
        awake_ns: step * 1_000_000_000,
    };
    fixture
        .writer
        .publish_with_identity(
            &readings,
            Some(&times),
            &IdentityPublication {
                processes: &identities,
                events: &event_identities,
            },
        )
        .unwrap();
}

fn identity(pid: i32, program: collector::ring::ProgramReference) -> IdentityObservation {
    IdentityObservation {
        pid,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: 1,
            },
            program,
        },
    }
}

fn publish_program(
    fixture: &mut Fixture,
    bytes: &ProgramBytes<'_>,
    output: &mut Option<collector::ring::ProgramReference>,
) {
    fixture.writer.intern_program(bytes, output).unwrap();
}

fn publish_many_identities(
    many: &Many<'_>,
    program: &collector::ring::ProgramReference,
    output: &mut Vec<IdentityObservation>,
) {
    output.extend((0..many.count).map(|index| {
        let pid = many.first_pid + i32::try_from(index).unwrap();
        identity(pid, *program)
    }));
}

const MEASURED_TASK: Task = Task {
    threads: 1,
    faults: 0,
    context_switches: 0,
    mach_syscalls: 0,
    unix_syscalls: 0,
};

fn long_app() -> Vec<u8> {
    let mut app = vec![b'a'; collector::ring::PROGRAM_PATH_BYTES_MAX];
    app[0] = b'/';
    let end = app.len();
    app[end - 4..].copy_from_slice(b".app");
    app
}

fn wide_events(fixture: &mut Fixture) {
    use collector::ring::{DictionaryCapacity, Place, RingReader, Writer};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let shape = collector::ring::Shape {
        event_capacity: 4096,
        ..shape
    };
    let writer = Writer::create_with_identity(
        &Place {
            lock_path: fixture.root.join("native.lock"),
            name_prefix: format!("/fdnv{}x{serial}", std::process::id()),
        },
        shape,
        DictionaryCapacity {
            entries: 4,
            arena_bytes: 32768,
        },
        &fixture.writer.identity().unwrap(),
        0,
    )
    .unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    let mut registry = collector::recording::Registry::read(&fixture.root).unwrap();
    registry.runs[0].ring_name = fixture.writer.name().to_owned();
    registry.write(&fixture.root).unwrap();
}

#[test]
fn invalid_power_keeps_cpu_and_process_input_for_the_next_native_publication() {
    // Goal: a failed sensor refuses its summary/raw facets without erasing valid CPU/App input.
    // Method: publish real A/B/C identity ticks, with NaN power only at B and matched PID 7.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1, Some(12.0), true);
    publish(&mut fixture, 2, Some(f32::NAN), true);
    publish(&mut fixture, 3, Some(13.0), true);
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut observed = Vec::new();

    for _ in 0..3 {
        let progress = ring
            .read_prepared(&budget, &operation, None, |_, prepared| {
                let machine = prepared.machine.as_ref().unwrap();
                let process = prepared
                    .apps()
                    .processes
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|process| process.pid == 7)
                    .unwrap();
                observed.push((
                    machine.publication,
                    machine.complete.clone(),
                    machine.cpu_counters().unwrap()[0].ticks.user,
                    process.cpu_ns(),
                    prepared.raw().is_ok(),
                ));
                true
            })
            .unwrap();
        assert_eq!(progress, super::NativeRead::Accepted);
    }

    assert_eq!(observed.len(), 3);
    assert!(matches!(
        observed[1].1,
        CompleteMachineState::Refused("unsupported or nonfinite cell")
    ));
    assert_eq!(observed[1].2, 2);
    assert!(!observed[1].4);
    assert_eq!(observed[2].3, Amount::Known(100_000_000));
    assert!(observed[2].4);
}

#[test]
fn admission_and_pool_retries_reuse_the_prepared_frame_and_single_app_derivation() {
    // Goal: neither resource refusal nor a held pool slot repeats accepted preparation work.
    // Method: deny B's Raw allocation after its valid Sample, free pressure, then block its commit.
    let mut fixture = Fixture::new_mode(true, 6000, None);
    let app = long_app();
    publish_many(&mut fixture, 1, &Many::new(1, 7, b"w", &app));
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let trace =
        persistence::work::ingestion::Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let mut previous = crate::store::AppsSecond::default();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        previous = std::mem::take(&mut prepared.apps);
        true
    })
    .unwrap();
    publish_many(&mut fixture, 2, &Many::new(6000, 1, b"w", &app));
    let frame = frame_bytes(&fixture, 2);
    let pressure = budget
        .reserve(budget.maximum() - budget.used() - 2 * frame)
        .unwrap();

    let refused = ring.read_prepared(&budget, &operation, Some(&previous), |_, _| {
        panic!("admission refusal must not reach acceptance")
    });
    assert!(matches!(
        refused,
        Err(super::PrepareError::Admission(super::Admission::Raw))
    ));
    assert!(!ring.caught_up().unwrap());
    drop(pressure);
    assert_eq!(
        ring.read_prepared(&budget, &operation, Some(&previous), |_, prepared| {
            assert_eq!(prepared.observation.stamp.publication, 2);
            false
        })
        .unwrap(),
        super::NativeRead::Blocked
    );
    assert!(!ring.caught_up().unwrap());
    let mut accepted = crate::store::AppsSecond::default();
    assert_eq!(
        ring.read_prepared(&budget, &operation, Some(&previous), |_, prepared| {
            accepted = std::mem::take(&mut prepared.apps);
            true
        })
        .unwrap(),
        super::NativeRead::Accepted
    );
    publish_many(&mut fixture, 3, &Many::new(1, 7, b"w", &app));
    ring.read_prepared(&budget, &operation, Some(&accepted), |_, prepared| {
        assert_eq!(cpu_amount(prepared.apps()), Amount::Known(100_000_000));
        assert!(Arc::ptr_eq(
            &prepared.apps().apps[0].name,
            &accepted.apps[0].name
        ));
        true
    })
    .unwrap();
    drop(scope);

    assert_prepared_once(&trace, 2);
    drop((ring, previous, accepted));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_clockless_native_tick_keeps_raw_input_without_inventing_a_chart_second() {
    // Goal: missing clocks do not erase admitted native facts or create a false chart position.
    // Method: publish A/B/C with no clocks at B and inspect all three through the real Ring.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1, None, true);
    publish(&mut fixture, 2, None, false);
    publish(&mut fixture, 3, None, true);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();

    for step in 1..=3 {
        assert_eq!(
            ring.read_prepared(&budget, &operation, None, |_, prepared| {
                if step == 2 {
                    assert!(prepared.machine.is_none());
                    assert_eq!(prepared.observation.wall_ns, None);
                    assert_eq!(prepared.raw().unwrap().machine.ticks[0].wall_ns, None);
                    assert_eq!(prepared.raw().unwrap().machine.cpus[0].ticks.user, 2);
                    assert!(prepared.app_refusal().is_none());
                }
                if step == 3 {
                    assert_eq!(
                        cpu_amount(prepared.apps()),
                        Amount::Unavailable(crate::apps::Unavailable::Interval)
                    );
                    assert!(!prepared.machine.as_ref().unwrap().follows);
                }
                true
            })
            .unwrap(),
            super::NativeRead::Accepted
        );
    }
    assert!(ring.caught_up().unwrap());
    assert!(ring.complete(&fixture.writer.identity().unwrap().recording));
}

#[test]
fn an_unadmitted_frame_does_not_let_duplicate_legacy_pids_escape_as_valid_raw() {
    // Goal: Frame capacity cannot skip structural process validation and promote invalid raw.
    // Method: a real legacy publication has too many rows and duplicates the same PID throughout.
    let count = crate::apps::PROCESS_INTERVALS_MAX + 1;
    let mut fixture = Fixture::new_mode(false, u64::try_from(count).unwrap(), None);
    publish_legacy_duplicates(&mut fixture, count);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();

    let progress = ring
        .read_prepared(&budget, &operation, None, |_, prepared| {
            assert_eq!(
                prepared.app_refusal(),
                Some(super::Unavailable::Frame("live App process bound"))
            );
            assert!(matches!(
                prepared.raw(),
                Err(super::Unavailable::UnvalidatedFrame)
            ));
            assert!(prepared.machine.as_ref().unwrap().cpu_counters().is_some());
            true
        })
        .unwrap();

    assert_eq!(progress, super::NativeRead::Accepted);
}

fn frame_bytes(fixture: &Fixture, publication: u64) -> usize {
    use collector::ring::{Found, RingReader};
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    reader.seek_publication(publication).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut identities =
        Vec::with_capacity(usize::try_from(reader.shape().process_capacity).unwrap());
    assert!(matches!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        Found::Publication { .. }
    ));
    crate::live_apps::Frame::capture_bytes(
        &crate::live_apps::Publication {
            reader: &reader,
            tick: &tick,
            identities: &identities,
            publication,
        },
        &crate::live_apps::PublicationEvents {
            events: &[],
            identities: &[],
        },
    )
    .unwrap()
}

fn assert_prepared_once(trace: &persistence::work::ingestion::Trace, publication: u64) {
    use persistence::work::ingestion::{Key, Kind};
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    let count = |kind| {
        events
            .iter()
            .filter(|event| event.kind == kind)
            .filter(|event| {
                matches!(
                    event.key,
                    Key::Publication { publication: seen, .. } if seen == publication
                )
            })
            .count()
    };
    assert_eq!(count(Kind::RingRead), 1);
    assert_eq!(count(Kind::FrameCapture), 1);
    assert_eq!(count(Kind::AppDerivation), 1);
    assert_eq!(count(Kind::RawCaptureAttempt), 2);
    assert_eq!(count(Kind::RawCapture), 1);
    assert_eq!(count(Kind::MachineReduction), 1);
}

fn publish_legacy_duplicates(fixture: &mut Fixture, count: usize) {
    let processes = vec![
        Process {
            pid: 7,
            usage: usage(0),
            task: MEASURED_TASK
        };
        count
    ];
    let cpus = [CPUTicks {
        user: 1,
        system: 0,
        nice: 0,
        idle: 10,
    }];
    let memory = Memory::default();
    fixture.writer.publish_observed(
        &Readings {
            run_tick: 0,
            cpus: &cpus,
            memory: &memory,
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
        &ObservationTimes {
            wall_ns: 1_000_000_000,
            continuous_ns: 1_000_000_000,
            awake_ns: 1_000_000_000,
        },
    );
}

#[test]
fn nan_fans_stay_missing_channels_without_refusing_native_raw_or_summary() {
    // Goal: unavailable fan channels keep the existing exception at the real native boundary.
    // Method: publish NaN fan speed beside valid power and inspect both owned facets.
    let mut fixture = Fixture::new();
    publish_smc(&mut fixture, 1, Some(12.0), true, f32::NAN);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();

    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        let machine = prepared.machine.as_ref().unwrap();
        assert_eq!(
            machine.complete().unwrap().summary(machine)["fan_rpm"],
            serde_json::json!([null])
        );
        assert!(prepared.raw().is_ok());
        true
    })
    .unwrap();
}

fn publish(fixture: &mut Fixture, step: u64, power: Option<f32>, clock: bool) {
    publish_smc(fixture, step, power, clock, 0.0);
}

fn publish_smc(fixture: &mut Fixture, step: u64, power: Option<f32>, clock: bool, fan_rpm: f32) {
    let identities = [publish_smc_identity(fixture)];
    let process = Process {
        pid: 7,
        usage: usage(step - 1),
        task: MEASURED_TASK,
    };
    let cpus = [CPUTicks {
        user: u32::try_from(step).unwrap(),
        idle: 10,
        system: 0,
        nice: 0,
    }];
    let memory = Memory::default();
    let fan = [fan_rpm];
    let readings = Readings {
        run_tick: step - 1,
        cpus: &cpus,
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: power.map(|power_watts| SmcReadings {
            power_watts,
            soc_celsius: 45.0,
            cpu_celsius: None,
            gpu_celsius: None,
            fan_rpm: &fan,
        }),
        processes: std::slice::from_ref(&process),
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: Dropped::default(),
    };
    let times = ObservationTimes {
        wall_ns: (1000 + step) * 1_000_000_000,
        continuous_ns: step * 1_000_000_000,
        awake_ns: step * 1_000_000_000,
    };
    fixture
        .writer
        .publish_with_identity(
            &readings,
            clock.then_some(&times),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
}

fn publish_smc_identity(fixture: &mut Fixture) -> IdentityObservation {
    let mut program = None;
    publish_program(
        fixture,
        &ProgramBytes {
            executable_name: b"current",
            app_path: Some(b"/Apps/Current.app"),
        },
        &mut program,
    );

    IdentityObservation {
        pid: 7,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: 7,
                pid_version: 1,
            },
            program: program.unwrap(),
        },
    }
}

#[cfg(test)]
#[path = "native_name_retry_tests.rs"]
mod name_retry_tests;
