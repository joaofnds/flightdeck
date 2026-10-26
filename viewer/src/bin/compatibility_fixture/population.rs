//! A finite, completed native source for comparing readers against identical process volume.
//! The generator exits before either reader starts; no external publication can advance it.
use collector::machine::{CPUTicks, Memory};
use collector::processes::{Process, Task, Usage};
use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, SmcReadings, Writer};
use collector::tracker::Event;
use persistence::{Error, Result};

use super::source::{AWAKE_FIRST_NS, CONTINUOUS_FIRST_NS, SECOND_FIRST, Source};

pub const PUBLICATIONS: u64 = 120;
pub const PROCESSES: u64 = 1050;
pub const EVENTS: usize = 64;
pub const MAPPED_BYTES_MAX: u64 = 64 * 1024 * 1024;

pub fn create(root: &std::path::Path, recording: collector::ring::Identity) -> Result<Source> {
    let identity = collector::recording::new_run_identity(recording)?;
    let place = Place {
        lock_path: root.join("population.lock"),
        name_prefix: format!("/fdpop{}", std::process::id()),
    };
    let shape = Shape {
        slot_count: PUBLICATIONS,
        cpu_capacity: 18,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 2,
        fan_capacity: 2,
        process_capacity: PROCESSES,
        coalition_capacity: 1,
        event_capacity: PUBLICATIONS * u64::try_from(EVENTS).map_err(|_| Error::Bound("events"))?,
    };
    let mut writer = Writer::create_recording(&place, shape, &identity, CONTINUOUS_FIRST_NS)?;
    super::source::create_register(root, &identity, writer.name())?;
    for tick in 0..PUBLICATIONS {
        publish(&mut writer, tick)?;
    }
    writer.finish();
    let reader = collector::ring::RingReader::open_generation(writer.name())?;
    let mapped_bytes = collector::u64_from_usize(reader.mapped_size_bytes());
    if mapped_bytes > MAPPED_BYTES_MAX {
        return Err(Error::Bound("population ring bytes"));
    }
    Ok(Source {
        identity,
        layout: super::SourceLayout::Legacy,
        lock: place.lock_path,
        ring: writer.name().to_owned(),
        second_first: SECOND_FIRST,
        mapped_bytes,
        population: true,
    })
}

fn publish(writer: &mut Writer, tick: u64) -> Result<()> {
    let tick32 = u32::try_from(tick).map_err(|_| Error::Bound("population tick"))?;
    let cpus = [CPUTicks {
        user: tick32 * 20,
        system: tick32 * 10,
        nice: 0,
        idle: tick32 * 70,
    }; 18];
    let memory = Memory {
        total_bytes: 48 * 1024 * 1024 * 1024,
        active_bytes: 16 * 1024 * 1024 * 1024,
        pageins: tick,
        pageouts: tick * 2,
        ..Memory::default()
    };
    let processes = processes(tick)?;
    let events: Vec<_> = (1..=EVENTS)
        .map(|pid| {
            i32::try_from(pid)
                .map(|pid| Event::Exec { pid })
                .map_err(|_| Error::Bound("population event pid"))
        })
        .collect::<Result<_>>()?;
    let mut bytes = [0; collector::interfaces::IFNAMSIZ];
    bytes[..3].copy_from_slice(b"en0");
    let interfaces = [collector::interfaces::Interface {
        index: 7,
        name: collector::interfaces::Name::from_bytes(bytes, 3)
            .ok_or(Error::Bound("population interface"))?,
        received_bytes: tick * 1_048_576,
        sent_bytes: tick * 524_288,
        received_packets: tick * 100,
        sent_packets: tick * 50,
        receive_errors: 0,
        send_errors: 0,
        receive_drops: 0,
    }];
    writer.publish_observed(
        &Readings {
            run_tick: tick,
            cpus: &cpus,
            memory: &memory,
            interfaces: &interfaces,
            ioreport: None,
            smc: Some(SmcReadings {
                power_watts: 12.3,
                soc_celsius: 42.1,
                cpu_celsius: None,
                gpu_celsius: None,
                fan_rpm: &[1234.1, 0.0],
            }),
            processes: &processes,
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &events,
            dropped: Dropped::default(),
        },
        &ObservationTimes {
            wall_ns: (SECOND_FIRST + tick) * 1_000_000_000,
            continuous_ns: CONTINUOUS_FIRST_NS + tick * 1_000_000_000,
            awake_ns: AWAKE_FIRST_NS + tick * 1_000_000_000,
        },
    );
    Ok(())
}

fn processes(tick: u64) -> Result<Vec<Process>> {
    let mut processes = Vec::with_capacity(
        usize::try_from(PROCESSES).map_err(|_| Error::Bound("population process count"))?,
    );
    for pid in 1..=PROCESSES {
        processes.push(Process {
            pid: i32::try_from(pid).map_err(|_| Error::Bound("population pid"))?,
            usage: Usage {
                awake_ns_at_start: 70_000_000_000,
                user_ns: 9_007_199_254_740_993 + tick * 1_000_000,
                system_ns: tick * 1_000,
                energy_nj: tick * 1_000_000,
                instructions: tick * 500_000,
                super_instructions: tick * 300_000,
                cycles: tick * 1_000_000,
                idle_wakeups: tick,
                interrupt_wakeups: tick,
                pageins: tick,
                resident_bytes: 1024 * 1024,
                footprint_bytes: 2 * 1024 * 1024,
                disk_read_bytes: tick * 4096,
                disk_written_bytes: tick * 2048,
            },
            task: Task {
                threads: 3,
                faults: 4,
                context_switches: 5,
                mach_syscalls: 6,
                unix_syscalls: 7,
            },
        });
    }
    Ok(processes)
}
