use collector::coalitions::Coalition;
use collector::interfaces::{Interface, Name};
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::processes::{Process, Task, Usage};
use collector::ring::{Identity, RunIdentity, Shape, Tick, TickBuffers};
use collector::tracker::{Event, Noticed};
use persistence::{Limits, NumberedEvent, Observation, Window, WindowIdentity};

#[must_use]
pub fn profile(full: bool) -> Shape {
    if full {
        Shape {
            slot_count: 120,
            cpu_capacity: 18,
            interface_capacity: 1638,
            core_capacity: 18,
            state_capacity: 64,
            fan_capacity: 10,
            process_capacity: 12_000,
            coalition_capacity: 28_384,
            event_capacity: 120 * 16_384,
        }
    } else {
        Shape {
            slot_count: 120,
            cpu_capacity: 2,
            interface_capacity: 2,
            core_capacity: 2,
            state_capacity: 3,
            fan_capacity: 2,
            process_capacity: 40,
            coalition_capacity: 8,
            event_capacity: 120 * 64,
        }
    }
}

pub fn tick(shape: &Shape) -> Tick {
    let count = |value| usize::try_from(value).unwrap();
    let mut tick = Tick::from_buffers(TickBuffers {
        cpus: vec![
            CPUTicks {
                user: 1,
                system: 2,
                nice: 3,
                idle: u32::MAX
            };
            count(shape.cpu_capacity)
        ],
        interfaces: (0..shape.interface_capacity).map(interface).collect(),
        gpu_active_ns: vec![41; count(shape.state_capacity)],
        core_active_ns: vec![42; count(shape.core_capacity * shape.state_capacity)],
        core_state_counts: vec![count(shape.state_capacity); count(shape.core_capacity)],
        state_capacity: count(shape.state_capacity),
        fan_rpm: vec![1234.5; count(shape.fan_capacity)],
        processes: (0..shape.process_capacity).map(process).collect(),
        coalitions: (0..shape.coalition_capacity).map(coalition).collect(),
    });
    tick.memory = memory();
    tick.ioreport_read = true;
    tick.smc_read = true;
    tick.gpu_energy_nj = 123;
    tick.power_watts = 42.25;
    tick.soc_celsius = 51.5;
    tick.processes_refused = 9;
    tick.processes_gone = 10;
    tick.dropped = collector::ring::Dropped {
        starts: 11,
        execs: 12,
        exits: 13,
    };
    tick
}

#[must_use]
pub fn window(full: bool) -> Window {
    let shape = profile(full);
    let mut tick = tick(&shape);
    let event_count = if full { 16_384_u64 } else { 64_u64 };
    let events: Vec<Event> = (0..event_count)
        .map(|index| Event::Exit {
            pid: i32::try_from(index + 30_000).unwrap(),
            last: Ok(usage(index)),
            noticed: Noticed::OnExit,
        })
        .collect();
    let identity = WindowIdentity {
        run: RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        },
        window: 0,
    };
    let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap());
    for index in 0..61 {
        tick.run_tick = index;
        tick.events = index * event_count..(index + 1) * event_count;
        let numbered: Vec<_> = events
            .iter()
            .enumerate()
            .map(|(offset, event)| NumberedEvent {
                number: index * event_count + u64::try_from(offset).unwrap(),
                tick: index,
                event,
            })
            .collect();
        let observation = Observation {
            publication: index + 1,
            wall_ns: Some(1_000_000_000_000 + index * 1_000_000_000),
            continuous_ns: Some(index * 1_000_000_000),
            awake_ns: Some(index * 1_000_000_000),
        };
        window.push_tick(&observation, &tick, &numbered).unwrap();
    }
    window
}

#[must_use]
pub fn usage(base: u64) -> Usage {
    Usage {
        awake_ns_at_start: base + 1,
        user_ns: base + 2,
        system_ns: base + 3,
        energy_nj: base + 4,
        instructions: base + 5,
        super_instructions: base + 6,
        cycles: base + 7,
        idle_wakeups: base + 8,
        interrupt_wakeups: base + 9,
        pageins: base + 10,
        resident_bytes: base + 11,
        footprint_bytes: base + 12,
        disk_read_bytes: base + 13,
        disk_written_bytes: base + 14,
    }
}

fn process(index: u64) -> Process {
    Process {
        pid: i32::try_from(index).unwrap(),
        usage: usage(index * 100),
        task: Task {
            threads: -1,
            faults: 2,
            context_switches: 3,
            mach_syscalls: i32::MAX,
            unix_syscalls: i32::MIN,
        },
    }
}

fn coalition(index: u64) -> Coalition {
    Coalition {
        id: index,
        tasks_started: index + 1,
        tasks_exited: index + 2,
        cpu_ns: index + 3,
        interrupt_wakeups: index + 4,
        idle_wakeups: index + 5,
        disk_read_bytes: index + 6,
        disk_written_bytes: index + 7,
        gpu_ns: index + 8,
        energy_nj: index + 9,
    }
}

fn interface(index: u64) -> Interface {
    Interface {
        index: u16::try_from(index).unwrap(),
        name: Name::from_bytes(*b"fixture0\0\0\0\0\0\0\0\0", 8).unwrap(),
        received_bytes: index + 1,
        sent_bytes: index + 2,
        received_packets: index + 3,
        sent_packets: index + 4,
        receive_errors: index + 5,
        send_errors: index + 6,
        receive_drops: index + 7,
    }
}

fn memory() -> Memory {
    Memory {
        total_bytes: 1,
        free_bytes: 2,
        active_bytes: 3,
        inactive_bytes: 4,
        speculative_bytes: 5,
        wired_bytes: 6,
        compressed_bytes: 7,
        anonymous_bytes: 8,
        file_backed_bytes: 9,
        purgeable_bytes: 10,
        swap_total_bytes: 11,
        swap_used_bytes: 12,
        swapped_in_bytes: 13,
        swapped_out_bytes: 14,
        pageins: 15,
        pageouts: 16,
        compressions: 17,
        decompressions: 18,
        pressure: Pressure::Warning,
    }
}
