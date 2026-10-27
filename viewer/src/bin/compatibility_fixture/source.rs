use std::path::{Path, PathBuf};

use collector::machine::{CPUTicks, Memory};
use collector::processes::{Process, Task, Usage};
use collector::recording::{self, RegisteredRun, Registry, private};
use collector::ring::{
    DictionaryCapacity, Dropped, Identity, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference,
    Readings, RingReader, RunIdentity, Shape, UsageAvailability, Writer,
};
use collector::tracker::{Event, Noticed};
use persistence::{Error, Result};

use super::SourceLayout;

pub const SECOND_FIRST: u64 = 1_800_000_000;
pub const CONTINUOUS_FIRST_NS: u64 = 100_000_000_000;
pub const AWAKE_FIRST_NS: u64 = 80_000_000_000;
pub const EXECUTABLE: &[u8] = b"worker\xff\n\\";
pub const APP: &[u8] = b"/Applications/Fixture\xff\n\\.app";
pub const EVENT_EXECUTABLE: &[u8] = b"event\xfe\t\\";

pub struct Source {
    pub identity: RunIdentity,
    pub population: bool,
    pub layout: SourceLayout,
    pub lock: PathBuf,
    pub ring: String,
    pub second_first: u64,
    pub mapped_bytes: u64,
}

struct Programs {
    process: ProgramReference,
    event: ProgramReference,
}

const USAGE: Usage = Usage {
    awake_ns_at_start: 70_000_000_000,
    user_ns: 9_007_199_254_740_993,
    system_ns: 9_007_199_254_741_003,
    energy_nj: 9_007_199_254_741_093,
    instructions: 9_007_199_254_741_993,
    super_instructions: 9_007_199_254_742_993,
    cycles: 18_446_744_073_709_551_605,
    idle_wakeups: 11,
    interrupt_wakeups: 12,
    pageins: 13,
    resident_bytes: 104_857_600,
    footprint_bytes: 125_829_120,
    disk_read_bytes: 9_007_199_254_743_993,
    disk_written_bytes: 9_007_199_254_744_993,
};

pub const PROCESS: Process = Process {
    pid: 41001,
    usage: USAGE,
    task: Task {
        threads: 4,
        faults: i32::MAX,
        context_switches: i32::MIN,
        mach_syscalls: 17,
        unix_syscalls: 18,
    },
};

pub fn create(
    root: &Path,
    recording: Identity,
    layout: SourceLayout,
    index: u64,
    output: &mut Option<Source>,
) -> Result<()> {
    create_custom(root, recording, layout, index, output, |_| {})
}

pub fn create_custom(
    root: &Path,
    recording: Identity,
    layout: SourceLayout,
    index: u64,
    output: &mut Option<Source>,
    mut customize: impl FnMut(&mut Readings<'_>),
) -> Result<()> {
    assert!(output.is_none());
    assert!(index < 2);
    let name = match layout {
        SourceLayout::Legacy => "legacy",
        SourceLayout::Identity => "identity",
    };
    let place = Place {
        lock_path: root.join(format!("{name}.lock")),
        name_prefix: format!("/fdcompat{}r{index}", std::process::id()),
    };
    let identity = recording::new_run_identity(recording)?;
    let mut writer = match layout {
        SourceLayout::Legacy => {
            Writer::create_recording(&place, SHAPE, &identity, CONTINUOUS_FIRST_NS)?
        }
        SourceLayout::Identity => Writer::create_with_identity(
            &place,
            SHAPE,
            DictionaryCapacity {
                entries: 2,
                arena_bytes: 256,
            },
            &identity,
            CONTINUOUS_FIRST_NS,
        )?,
    };
    create_register(root, &identity, writer.name())?;
    let mut programs = None;
    if layout == SourceLayout::Identity {
        create_programs(&mut writer, &mut programs)?;
    }

    let second_first = SECOND_FIRST + index * 120;
    for tick in 0..2 {
        create_publish(
            &mut writer,
            programs.as_ref(),
            second_first,
            tick,
            &mut customize,
        )?;
    }

    writer.finish();

    create_output(writer, place, identity, second_first, layout, output)
}

fn create_output(
    writer: Writer,
    place: Place,
    identity: RunIdentity,
    second_first: u64,
    layout: SourceLayout,
    output: &mut Option<Source>,
) -> Result<()> {
    let reader = RingReader::open(&place.lock_path)?;
    let mapped_bytes = u64::try_from(reader.mapped_size_bytes())
        .map_err(|_| Error::Bound("fixture mapping bytes"))?;
    if mapped_bytes > 65_536 {
        return Err(Error::Bound("fixture ring mapping bytes"));
    }

    let ring = writer.name().to_owned();
    drop(reader);
    drop(writer);

    *output = Some(Source {
        identity,
        population: false,
        layout,
        lock: place.lock_path,
        ring,
        second_first,
        mapped_bytes,
    });

    Ok(())
}

pub const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 3,
    coalition_capacity: 1,
    event_capacity: 2,
};

pub(super) fn create_register(root: &Path, identity: &RunIdentity, ring: &str) -> Result<()> {
    let lock = private::FileLock::wait_short(&root.join("registration.lock"))?;
    let mut registry = Registry::read(root)?;
    registry.add(
        root,
        RegisteredRun {
            identity: *identity,
            ring_name: ring.to_owned(),
            collector_pid: std::process::id(),
        },
    )?;
    registry.write(root)?;
    drop(lock);

    Ok(())
}

fn create_programs(writer: &mut Writer, output: &mut Option<Programs>) -> Result<()> {
    assert!(output.is_none());
    let mut process = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: EXECUTABLE,
                app_path: Some(APP),
            },
            &mut process,
        )
        .map_err(|_| Error::Bound("fixture process dictionary"))?;
    let mut event = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: EVENT_EXECUTABLE,
                app_path: None,
            },
            &mut event,
        )
        .map_err(|_| Error::Bound("fixture event dictionary"))?;

    *output = Some(Programs {
        process: process.ok_or(Error::Bound("fixture process key absent"))?,
        event: event.ok_or(Error::Bound("fixture event key absent"))?,
    });

    Ok(())
}

fn create_publish(
    writer: &mut Writer,
    programs: Option<&Programs>,
    second_first: u64,
    tick: u64,
    customize: &mut impl FnMut(&mut Readings<'_>),
) -> Result<()> {
    assert!(tick < 2);
    let memory = Memory {
        total_bytes: 8_589_934_592,
        free_bytes: 4_294_967_296,
        ..Memory::default()
    };
    let cpus = [CPUTicks {
        user: 10 + u32::try_from(tick).map_err(|_| Error::Bound("tick"))?,
        system: 20,
        nice: 30,
        idle: 40,
    }];
    let mut usage = USAGE;
    usage.user_ns += tick * 200_000_000;
    usage.system_ns += tick * 100_000_000;
    usage.energy_nj += tick * 1_000_000_000;
    usage.cycles += tick;
    let processes = [Process { usage, ..PROCESS }];
    let events = [if tick == 0 {
        Event::Start {
            pid: 42001,
            coalition: Some(17),
        }
    } else {
        Event::Exit {
            pid: 42001,
            last: Ok(usage),
            noticed: Noticed::OnExit,
        }
    }];
    let times = ObservationTimes {
        wall_ns: (second_first + tick) * 1_000_000_000,
        continuous_ns: CONTINUOUS_FIRST_NS + tick * 1_000_000_000,
        awake_ns: AWAKE_FIRST_NS + tick * 1_000_000_000,
    };
    let mut readings = Readings {
        run_tick: tick,
        cpus: &cpus,
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &processes,
        processes_refused: 1,
        processes_gone: 1,
        coalitions: &[],
        events: &events,
        dropped: Dropped::default(),
    };
    customize(&mut readings);
    if let Some(programs) = programs {
        create_publish_identity(writer, programs, &readings, &times)?;
    } else {
        writer.publish_observed(&readings, &times);
    }

    Ok(())
}

fn create_publish_identity(
    writer: &mut Writer,
    programs: &Programs,
    readings: &Readings<'_>,
    times: &ObservationTimes,
) -> Result<()> {
    let mut identities = [IdentityObservation::EMPTY; 3];
    create_identity(
        41001,
        UsageAvailability::Measured,
        &programs.process,
        u64::MAX,
        &mut identities[0],
    );
    create_identity(
        41002,
        UsageAvailability::Refused,
        &programs.process,
        u64::MAX - 1,
        &mut identities[1],
    );
    create_identity(
        41003,
        UsageAvailability::Gone,
        &programs.process,
        u64::MAX - 2,
        &mut identities[2],
    );
    let mut event = IdentityObservation::EMPTY;
    create_identity(
        42001,
        if readings.run_tick == 0 {
            UsageAvailability::NotApplicable
        } else {
            UsageAvailability::Measured
        },
        &programs.event,
        1_311_768_467_463_790_320,
        &mut event,
    );
    writer.publish_with_identity(
        readings,
        Some(times),
        &IdentityPublication {
            processes: &identities,
            events: &[event],
        },
    )?;

    Ok(())
}

fn create_identity(
    pid: i32,
    usage: UsageAvailability,
    program: &ProgramReference,
    unique_id: u64,
    output: &mut IdentityObservation,
) {
    *output = IdentityObservation {
        pid,
        usage,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id,
                pid_version: if pid == 42001 { i32::MAX } else { i32::MIN },
            },
            program: *program,
        },
    };
}
