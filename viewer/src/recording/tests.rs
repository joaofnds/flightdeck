use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::ioreport::States;
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::ring::{
    Dropped, Found, IOReportReadings, ObservationTimes, Readings, RingReader, SmcReadings, Tick,
};

use super::{MachineMinute, SealedMinute, read_machine};
use crate::live::identity_fixture::Fixture;

#[test]
fn a_sealed_minute_holds_the_machine_readings_the_ring_held() {
    // Goal: the recording and the ring hand the store the same machine readings, so one rate
    // rule serves both.
    // Method: publish three ticks with distinct readings, the second without IOReport and the
    // third without the SMC, take the ring's ticks as the store would, seal them as one
    // minute, and decode it.
    let mut fixture = Fixture::new_mode(false, 2, None);
    for step in 1..=3 {
        publish_machine(&mut fixture, step);
    }
    let mut expected = MachineMinute::default();
    for (publication, tick) in ring_ticks(&fixture) {
        expected.push_tick(publication, &tick);
    }
    fixture.seal_minute(0);
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut decoded = MachineMinute::default();

    read_machine(&minute, &mut decoded).unwrap();

    let states = (expected.gpu_states.len(), expected.core_states.len());
    assert_eq!(
        (expected.ticks.len(), states, expected.fans.len()),
        (3, (2, 2), 2)
    );
    assert_eq!(decoded, expected);
}

/// Publishes the `step`th tick of run minute 0, holding `machine(step)`, with no `IOReport`
/// readings at step 2 and no SMC readings at step 3.
fn publish_machine(fixture: &mut Fixture, step: u64) {
    let (cpus, memory, interfaces) = machine(step);
    let sources = sources(step);
    let readings = Readings {
        run_tick: step - 1,
        cpus: &cpus,
        memory: &memory,
        interfaces: &interfaces,
        ioreport: (step != 2).then_some(IOReportReadings {
            gpu_energy_nj: sources.gpu_energy_nj,
            gpu_active_ns: &sources.gpu_active_ns,
            cores: &sources.cores,
        }),
        smc: (step != 3).then_some(SmcReadings {
            power_watts: sources.power_watts,
            soc_celsius: sources.soc_celsius,
            cpu_celsius: Some(sources.cpu_celsius),
            gpu_celsius: None,
            fan_rpm: &sources.fan_rpm,
        }),
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: Dropped::default(),
    };
    let times = ObservationTimes {
        wall_ns: (1_000 + step) * 1_000_000_000,
        continuous_ns: step * 1_000_000_000,
        awake_ns: step * 900_000_000,
    };
    fixture.writer.publish_observed(&readings, &times);
}

/// Every tick the ring holds, oldest first, each with its publication.
fn ring_ticks(fixture: &Fixture) -> Vec<(u64, Tick)> {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = reader.shape();
    let mut ticks = Vec::with_capacity(8);
    for _index in 0..8 {
        let mut tick = persistence::tick_buffer(&shape).unwrap();
        match reader.read_into(&mut tick).unwrap() {
            Found::Nothing => break,
            Found::Missed { .. } => panic!("fixture source overwritten"),
            Found::Publication { publication, .. } => ticks.push((publication, tick)),
        }
    }
    ticks
}

/// The marker of the sealed window of run minute `minute`.
fn marker(fixture: &Fixture, minute: u64) -> std::path::PathBuf {
    let store = persistence::Store::open(&fixture.root).unwrap();
    store.marker_path(&persistence::WindowIdentity {
        run: fixture.writer.identity().unwrap(),
        window: minute,
    })
}

/// The machine readings of the `step`th tick, each counter distinct from every other tick's.
fn machine(step: u64) -> ([CPUTicks; 1], Memory, [Interface; 1]) {
    let small = u32::try_from(step).unwrap();
    let cpus = [CPUTicks {
        user: 10 * small,
        system: 10 * small + 1,
        nice: 10 * small + 2,
        idle: 10 * small + 3,
    }];
    let memory = Memory {
        total_bytes: 1_000 * step,
        free_bytes: 1_000 * step + 1,
        active_bytes: 1_000 * step + 2,
        inactive_bytes: 1_000 * step + 3,
        speculative_bytes: 1_000 * step + 4,
        wired_bytes: 1_000 * step + 5,
        compressed_bytes: 1_000 * step + 6,
        anonymous_bytes: 1_000 * step + 7,
        file_backed_bytes: 1_000 * step + 8,
        purgeable_bytes: 1_000 * step + 9,
        swap_total_bytes: 1_000 * step + 10,
        swap_used_bytes: 1_000 * step + 11,
        swapped_in_bytes: 1_000 * step + 12,
        swapped_out_bytes: 1_000 * step + 13,
        pageins: 1_000 * step + 14,
        pageouts: 1_000 * step + 15,
        compressions: 1_000 * step + 16,
        decompressions: 1_000 * step + 17,
        pressure: [Pressure::Normal, Pressure::Warning, Pressure::Critical]
            [usize::try_from(step % 3).unwrap()],
    };
    let mut name = [0; IFNAMSIZ];
    name[..4].copy_from_slice(b"utun");
    let interfaces = [Interface {
        index: 4,
        name: Name::from_bytes(name, 4).unwrap(),
        received_bytes: 100 * step,
        sent_bytes: 100 * step + 1,
        received_packets: 100 * step + 2,
        sent_packets: 100 * step + 3,
        receive_errors: 100 * step + 4,
        send_errors: 100 * step + 5,
        receive_drops: 100 * step + 6,
    }];
    (cpus, memory, interfaces)
}

/// The `IOReport` and SMC readings of the `step`th tick, each distinct from every other tick's.
struct Sources {
    gpu_energy_nj: u64,
    gpu_active_ns: [u64; 1],
    cores: [States; 1],
    power_watts: f32,
    soc_celsius: f32,
    cpu_celsius: f32,
    fan_rpm: [f32; 1],
}

fn sources(step: u64) -> Sources {
    let small = f32::from(u8::try_from(step).unwrap());
    Sources {
        gpu_energy_nj: 10_000 * step,
        gpu_active_ns: [10_000 * step + 1],
        cores: [States::with_active_ns(&[10_000 * step + 2])],
        power_watts: 10.0 + small,
        soc_celsius: 40.0 + small,
        cpu_celsius: 50.0 + small,
        fan_rpm: [1_000.0 + small],
    }
}
