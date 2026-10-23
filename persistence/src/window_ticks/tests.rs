use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::machine::Memory;
use collector::processes::{Process, Task, Unreadable};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, FoundEvent, Identity, IdentityObservation,
    IdentityPublication, IdentityUnavailable, Place, ProcessCreation, ProcessIdentity,
    ProgramBytes, ProgramReference, Readings, RingReader, RunIdentity, Shape, UsageAvailability,
    Writer,
};
use collector::tracker::{Event, Noticed};
use model::apps::{Creation, Measurement, ProgramUnavailable};
use model::programs::{Capacity, Programs};

use super::{WindowTick, WindowTicks};
use crate::{IdentitySource, Limits, NumberedEvent, Observation, Window, WindowIdentity};

const SHAPE: Shape = Shape {
    slot_count: 4,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 4,
    coalition_capacity: 1,
    event_capacity: 16,
};

#[test]
fn reads_a_window_without_identities_as_processes_in_pid_order_of_no_program() {
    // Goal: a source that names no programs gives the rule its processes in pid order.
    // Method: stage a tick of two processes out of pid order and an exit, then read it.
    let mut window = legacy_window(0);
    stage_legacy(
        &mut window,
        0,
        &[process(9, 90), process(3, 30)],
        &[exit(9, 4)],
    );

    let ticks = read(&window);

    let tick = &ticks[0];
    assert_eq!(tick.pid, [3, 9]);
    assert_eq!(tick.user_ns, [30, 90]);
    assert_eq!(tick.program, [Err(ProgramUnavailable::Unsupported); 2]);
    assert_eq!(tick.creation, [None; 2]);
    assert_eq!(tick.exits, [9]);
    assert_eq!(tick.threads, [Some(5), Some(5)]);
}

#[test]
fn reads_a_negative_thread_count_as_none() {
    // Goal: a count the collector read none of reaches the rule as no count.
    // Method: stage a process whose thread count is negative, then read it.
    let mut window = legacy_window(0);
    let mut unread = process(3, 30);
    unread.task.threads = -1;
    stage_legacy(&mut window, 0, &[unread], &[]);

    let ticks = read(&window);

    assert_eq!(ticks[0].threads, [None]);
}

#[test]
fn pairs_the_first_tick_of_a_window_with_the_last_tick_read_before_it() {
    // Goal: a window's first tick takes its rates from the window before it.
    // Method: read two windows of one reader and look at the first tick of the second.
    let mut first = legacy_window(0);
    stage_legacy(&mut first, 0, &[process(3, 30)], &[]);
    stage_legacy(&mut first, 1, &[process(3, 31)], &[]);
    let mut second = legacy_window(1);
    stage_legacy(&mut second, 2, &[process(3, 32)], &[]);
    let mut ticks = WindowTicks::default();
    let mut programs = programs();
    ticks.read(&first, &mut programs, |_| Ok(())).unwrap();

    let read = read_with(&mut ticks, &mut programs, &second).unwrap();

    assert_eq!(read[0].previous_tick, 1);
    assert_eq!(read[0].previous_publication, Some(2));
    assert_eq!(read[0].previous_user_ns, [31]);
}

#[test]
fn pairs_no_tick_with_the_first_after_a_refused_window() {
    // Goal: a refused window leaves no tick for the next to take rates from.
    // Method: read a window whose second tick holds two processes of one pid, then a window
    // after it.
    let mut refused = legacy_window(0);
    stage_legacy(&mut refused, 0, &[process(3, 30)], &[]);
    stage_legacy(&mut refused, 1, &[process(3, 31), process(3, 32)], &[]);
    let mut next = legacy_window(1);
    stage_legacy(&mut next, 2, &[process(3, 33)], &[]);
    let mut ticks = WindowTicks::default();
    let mut programs = programs();

    let error = ticks.read(&refused, &mut programs, |_| Ok(())).unwrap_err();
    let read = read_with(&mut ticks, &mut programs, &next).unwrap();

    assert!(matches!(
        error,
        crate::Error::Bound("window tick pid order")
    ));
    assert_eq!(read[0].previous_publication, None);
    assert_eq!(read[0].previous_user_ns, [0_u64; 0]);
}

#[test]
fn reads_each_identity_with_its_creation_program_and_usage() {
    // Goal: the rule reads the creation, program and usage the collector published.
    // Method: publish a refused and a measured process and a start through a real ring.
    let mut ring = Ring::new();
    let helper = ring.intern(b"helper", Some(b"/Apps/Outer.app"));
    let tool = ring.intern(b"tool", None);
    let identities = [
        identity(1, UsageAvailability::Refused, helper),
        identity(7, UsageAvailability::Measured, tool),
    ];
    let start = IdentityObservation {
        usage: UsageAvailability::NotApplicable,
        ..identities[1]
    };
    let starts = [Event::Start {
        pid: 7,
        coalition: None,
    }];
    ring.publish(0, &[process(7, 70)], &identities, &starts, &[start]);
    let window = ring.stage(0);

    let mut ticks = WindowTicks::default();
    let mut programs = programs();
    let read = read_with(&mut ticks, &mut programs, &window).unwrap();

    let tick = &read[0];
    assert_eq!(tick.pid, [1, 7]);
    assert_eq!(tick.creation, [Some(creation(1)), Some(creation(7))]);
    assert_eq!(
        tick.measurement,
        [Measurement::Refused, Measurement::Measured]
    );
    assert_eq!(tick.user_ns, [0, 70]);
    assert_eq!(tick.starts, tick.program[1..]);
    let names: Vec<_> = tick
        .program
        .iter()
        .map(|entry| {
            let program = programs.program(*entry.as_ref().unwrap());
            (
                program.executable_name.to_vec(),
                program.app_path.map(<[u8]>::to_vec),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            (b"helper".to_vec(), Some(b"/Apps/Outer.app".to_vec())),
            (b"tool".to_vec(), None),
        ]
    );
}

#[test]
fn reads_why_the_collector_read_no_program() {
    // Goal: a process whose program the collector could not read reaches the rule with why.
    // Method: publish a process whose identity is unavailable through a real ring.
    let mut ring = Ring::new();
    let unread = IdentityObservation {
        pid: 4,
        usage: UsageAvailability::Gone,
        identity: ProcessIdentity::Unavailable {
            creation: None,
            reason: IdentityUnavailable::Exhausted,
        },
    };
    ring.publish(0, &[], &[unread], &[], &[]);
    let window = ring.stage(0);

    let mut ticks = WindowTicks::default();
    let read = read_with(&mut ticks, &mut programs(), &window).unwrap();

    assert_eq!(read[0].program, [Err(ProgramUnavailable::Exhausted)]);
    assert_eq!(read[0].measurement, [Measurement::Gone]);
    assert_eq!(read[0].creation, [None]);
    assert_eq!((read[0].usage_refused, read[0].usage_gone), (0, 1));
}

/// What a test reads of one tick and the tick before it.
#[derive(Debug)]
struct Read {
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<Result<u32, ProgramUnavailable>>,
    measurement: Vec<Measurement>,
    user_ns: Vec<u64>,
    threads: Vec<Option<u64>>,
    exits: Vec<i32>,
    starts: Vec<Result<u32, ProgramUnavailable>>,
    usage_refused: u32,
    usage_gone: u32,
    previous_tick: u64,
    previous_publication: Option<u64>,
    previous_user_ns: Vec<u64>,
}

impl Read {
    fn new(tick: &WindowTick<'_>) -> Self {
        let selected = &tick.selected;
        Self {
            pid: selected.processes.pid.to_vec(),
            creation: selected.processes.creation.to_vec(),
            program: selected.processes.program.to_vec(),
            measurement: selected.processes.usage.measurement.to_vec(),
            user_ns: selected.processes.usage.user_ns.to_vec(),
            threads: selected.processes.threads.to_vec(),
            exits: selected.exits.pid.to_vec(),
            starts: selected.starts.program.to_vec(),
            usage_refused: tick.usage_refused,
            usage_gone: tick.usage_gone,
            previous_tick: tick.previous.stamp.tick,
            previous_publication: tick
                .previous_observation
                .map(|observation| observation.publication),
            previous_user_ns: tick.previous.processes.usage.user_ns.to_vec(),
        }
    }
}

fn read(window: &Window) -> Vec<Read> {
    read_with(&mut WindowTicks::default(), &mut programs(), window).unwrap()
}

fn read_with(
    ticks: &mut WindowTicks,
    programs: &mut Programs,
    window: &Window,
) -> crate::Result<Vec<Read>> {
    let mut read = Vec::new();
    ticks.read(window, programs, |tick| {
        read.push(Read::new(tick));
        Ok(())
    })?;
    Ok(read)
}

fn programs() -> Programs {
    Programs::new(Capacity {
        entries: 16,
        program_bytes: 1024,
    })
}

fn run() -> RunIdentity {
    RunIdentity {
        recording: Identity([1, 2]),
        run: Identity([3, 4]),
        boot: Identity([5, 6]),
    }
}

fn legacy_window(number: u64) -> Window {
    Window::new(
        WindowIdentity {
            run: run(),
            window: number,
        },
        Limits::from_shape(&SHAPE).unwrap(),
    )
}

/// Stages tick `run_tick` of `processes` and `events` in `window`, as a source of no identities
/// publishes it.
fn stage_legacy(window: &mut Window, run_tick: u64, processes: &[Process], events: &[Event]) {
    let mut tick = crate::tick_buffer(&SHAPE).unwrap();
    tick.run_tick = run_tick;
    tick.processes = processes.to_vec();
    let first = run_tick * 10;
    tick.events = first..first + u64::try_from(events.len()).unwrap();
    let numbered: Vec<_> = events
        .iter()
        .zip(first..)
        .map(|(event, number)| NumberedEvent {
            number,
            tick: run_tick,
            event,
        })
        .collect();
    window
        .push_tick(&observation(run_tick), &tick, &numbered)
        .unwrap();
}

fn observation(run_tick: u64) -> Observation {
    Observation {
        publication: run_tick + 1,
        wall_ns: Some(1_000_000_000_000 + run_tick * 1_000_000_000),
        continuous_ns: Some(run_tick * 1_000_000_000),
        awake_ns: Some(run_tick * 1_000_000_000),
    }
}

fn process(pid: i32, user_ns: u64) -> Process {
    Process {
        pid,
        usage: usage(user_ns),
        task: Task {
            threads: 5,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    }
}

fn usage(user_ns: u64) -> collector::processes::Usage {
    collector::processes::Usage {
        awake_ns_at_start: 1,
        user_ns,
        system_ns: 2,
        energy_nj: 3,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 4,
        interrupt_wakeups: 0,
        pageins: 5,
        resident_bytes: 6,
        footprint_bytes: 7,
        disk_read_bytes: 8,
        disk_written_bytes: 9,
    }
}

fn exit(pid: i32, user_ns: u64) -> Event {
    Event::Exit {
        pid,
        last: if user_ns == 0 {
            Err(Unreadable::Gone)
        } else {
            Ok(usage(user_ns))
        },
        noticed: Noticed::OnExit,
    }
}

fn creation(pid: i32) -> Creation {
    Creation {
        unique_id: 100 + u64::try_from(pid).unwrap(),
        pid_version: -pid,
    }
}

fn identity(pid: i32, usage: UsageAvailability, program: ProgramReference) -> IdentityObservation {
    IdentityObservation {
        pid,
        usage,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: 100 + u64::try_from(pid).unwrap(),
                pid_version: -pid,
            },
            program,
        },
    }
}

/// A ring of the collector's that publishes identities, and its reader.
struct Ring {
    directory: PathBuf,
    writer: Writer,
    reader: Option<RingReader>,
}

static NEXT: AtomicU64 = AtomicU64::new(0);

impl Ring {
    fn new() -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("window-ticks-{}-{number}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let place = Place {
            lock_path: directory.join("lock"),
            name_prefix: "fdwinticks".into(),
        };
        let capacity = DictionaryCapacity {
            entries: 4,
            arena_bytes: 32768,
        };
        let writer = Writer::create_with_identity(&place, SHAPE, capacity, &run(), 0).unwrap();
        let reader = RingReader::open(&place.lock_path).unwrap();
        Self {
            directory,
            writer,
            reader: Some(reader),
        }
    }

    fn intern(&mut self, name: &[u8], app: Option<&[u8]>) -> ProgramReference {
        let mut program = None;
        let bytes = ProgramBytes {
            executable_name: name,
            app_path: app,
        };
        self.writer.intern_program(&bytes, &mut program).unwrap();
        program.unwrap()
    }

    fn publish(
        &mut self,
        run_tick: u64,
        processes: &[Process],
        identities: &[IdentityObservation],
        events: &[Event],
        event_identities: &[IdentityObservation],
    ) {
        let count = |usage| {
            let rows = identities.iter().filter(|row| row.usage == usage).count();
            u32::try_from(rows).unwrap()
        };
        let readings = Readings {
            run_tick,
            cpus: &[],
            memory: &Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes,
            processes_refused: count(UsageAvailability::Refused),
            processes_gone: count(UsageAvailability::Gone),
            coalitions: &[],
            events,
            dropped: Dropped::default(),
        };
        let publication = IdentityPublication {
            processes: identities,
            events: event_identities,
        };
        self.writer
            .publish_with_identity(&readings, None, &publication)
            .unwrap();
    }

    /// A window of the tick published last, read as the persister stages it.
    fn stage(&mut self, number: u64) -> Window {
        let reader = self.reader.as_mut().unwrap();
        let mut window = Window::new(
            WindowIdentity {
                run: run(),
                window: number,
            },
            Limits::from_shape(&SHAPE).unwrap(),
        );
        let mut tick = crate::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(4);
        let found = reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        assert!(matches!(found, Found::Publication { .. }));
        let mut events = Vec::new();
        let mut event_identities = Vec::new();
        for _ in 0..4 {
            let mut identity = IdentityObservation::EMPTY;
            match reader.read_event_identity(&mut identity).unwrap() {
                FoundEvent::Event {
                    number,
                    tick,
                    event,
                } => {
                    events.push((number, tick, event));
                    event_identities.push(identity);
                }
                FoundEvent::Nothing => break,
                FoundEvent::Missed { .. } => panic!("the ring lost an event"),
            }
        }
        let numbered: Vec<_> = events
            .iter()
            .map(|(number, tick, event)| NumberedEvent {
                number: *number,
                tick: *tick,
                event,
            })
            .collect();
        let source = IdentitySource {
            reader,
            processes: &identities,
            events: &event_identities,
        };
        window
            .push_tick_with_identity(&observation(tick.run_tick), &tick, &numbered, &source)
            .unwrap();
        window
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        self.reader = None;
        let name = self.writer.name().to_owned();
        collector::ring::unlink_generation(&name).unwrap();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}
