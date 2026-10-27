use collector::battery::{BatteryReading, READ_INTERVAL_NS};
use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::system::{FactText, PerformanceLevel};

use super::{BatterySecond, MachineSecond, MemorySecond, Reading, Traffic, readings};
use crate::recording::{
    CpuRow, FanRow, GpuStateRow, InterfaceRow, MachineMinute, MemoryRow, TickRow,
};

const SECOND_NS: u64 = 1_000_000_000;

#[test]
fn retained_cpu_interval_survives_source_drop_and_keeps_rollback_predecessor_identity() {
    // Goal: wall-filtered queries retain the original predecessor and compatibility refusal.
    // Method: capture adjacent continuous clocks with wall time rolling back, then drop input.
    let before = Machine::at(1)
        .cpus(&[(0, 0), (0, 0)])
        .wall_ns(1000 * SECOND_NS);
    let current = Machine::at(2)
        .cpus(&[(40, 60), (50, 50)])
        .wall_ns(999 * SECOND_NS);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let second = MachineSecond::complete_of(&current.reading(), Some(&before.reading()), &budget)
        .unwrap()
        .unwrap();
    drop(before);
    drop(current);
    let interval = second.cpu_interval().unwrap().unwrap();

    assert_eq!(interval.values, [None, None]);
    assert_eq!(
        interval.unavailable_reason,
        Some("Requires consecutive compatible CPU readings")
    );
    let previous = interval.previous.unwrap();
    assert_eq!(previous.publication, 1);
    assert_eq!(previous.run_tick, 0);
    assert_eq!(previous.wall_ns, 1000 * SECOND_NS);
    assert_eq!(previous.continuous_ns, Some(SECOND_NS));
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_refused_summary_keeps_the_same_independent_cpu_interval_values() {
    // Goal: a bad power sensor cannot erase valid per-core rates or their source identity.
    // Method: compare available and NaN-summary captures of the same two-core interval.
    let before = Machine::at(1).cpus(&[(0, 0), (0, 0)]);
    let mut current = Machine::at(2).cpus(&[(40, 60), (50, 50)]);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));
    let complete = MachineSecond::complete_of(&current.reading(), Some(&before.reading()), &budget)
        .unwrap()
        .unwrap();
    current.tick.power_watts = Some(f32::NAN);

    let refused = MachineSecond::complete_of(&current.reading(), Some(&before.reading()), &budget)
        .unwrap()
        .unwrap();

    assert!(refused.complete().is_none());
    let available = complete.cpu_interval().unwrap().unwrap();
    let independent = refused.cpu_interval().unwrap().unwrap();
    assert_eq!(available.values, [Some(40.0), Some(50.0)]);
    assert_eq!(independent.values, available.values);
    assert_eq!(independent.previous, available.previous);
    assert_eq!(independent.unavailable_reason, None);
    drop(complete);
    drop(refused);
    assert_eq!(budget.used(), 0);
}

#[test]
fn malformed_cpu_domains_refuse_detail_without_erasing_usable_overview() {
    // Goal: strict CPU-detail admission stays independent of otherwise usable machine facts.
    // Method: retain original ID7 counters while recording the shared decoder's domain refusal.
    let mut before = Machine::at(1).cpus(&[(0, 0)]);
    let mut current = Machine::at(2).cpus(&[(40, 60)]);
    before.cpus[0].cpu = 7;
    current.cpus[0].cpu = 7;
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let second = MachineSecond::complete_of(&current.reading(), Some(&before.reading()), &budget)
        .unwrap()
        .unwrap();

    assert_eq!(second.cpu_percent, Some(40.0));
    assert_eq!(second.cpu_counters().unwrap()[0].cpu, 7);
    assert!(matches!(
        second.cpu_interval(),
        Err(persistence::Error::Bound("incomplete logical CPU domain"))
    ));
}

macro_rules! retained_cpu_gap {
    ($name:ident, $change:expr, $reason:expr) => {
        #[test]
        fn $name() {
            // Goal: canonical per-core values preserve every shared CPU eligibility refusal.
            // Method: change one original counter/clock/sequence fact before typed retention.
            let before = Machine::at(1).cpus(&[(10, 0)]);
            let mut current = Machine::at(2).cpus(&[(50, 60)]);
            $change(&mut current);
            let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

            let second =
                MachineSecond::complete_of(&current.reading(), Some(&before.reading()), &budget)
                    .unwrap()
                    .unwrap();
            let interval = second.cpu_interval().unwrap().unwrap();

            assert_eq!(interval.values, [None]);
            assert_eq!(interval.unavailable_reason, $reason);
        }
    };
}

retained_cpu_gap!(
    retained_cpu_interval_withholds_a_missing_publication,
    |current: &mut Machine| current.tick.publication = 3,
    Some("Requires consecutive compatible CPU readings")
);
retained_cpu_gap!(
    retained_cpu_interval_withholds_a_nonconsecutive_run_tick,
    |current: &mut Machine| current.tick.run_tick = 3,
    Some("Requires consecutive compatible CPU readings")
);
retained_cpu_gap!(
    retained_cpu_interval_withholds_a_counter_reset,
    |current: &mut Machine| current.cpus[0].ticks.user = 0,
    None
);

#[test]
fn retained_cpu_interval_never_invents_a_clock_for_a_clockless_predecessor() {
    // Goal: CPU detail keeps the legacy absence of a predecessor without a saved wall clock.
    // Method: retain a chartable current tick directly after an owned clockless tick.
    let mut before = Machine::at(1).cpus(&[(0, 0)]);
    before.tick.wall_ns = None;
    let current = Machine::at(2).cpus(&[(40, 60)]);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let second = MachineSecond::complete_of(&current.reading(), Some(&before.reading()), &budget)
        .unwrap()
        .unwrap();
    let interval = second.cpu_interval().unwrap().unwrap();

    assert!(interval.previous.is_none());
    assert_eq!(interval.values, [None]);
    assert_eq!(
        interval.unavailable_reason,
        Some("Previous CPU reading unavailable")
    );
}

#[test]
fn complete_constructor_refuses_nonfinite_power_without_promoting_it_to_a_missing_gauge() {
    // Goal: complete Store capture keeps Raw's numeric refusal for a nonfinite SMC source.
    // Method: submit NaN power directly to the typed constructor, bypassing Raw capture.
    let machine = Machine::at(1).smc(f32::NAN, 45.0);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let second = MachineSecond::complete_of(&machine.reading(), None, &budget)
        .unwrap()
        .unwrap();

    assert_eq!(
        second.complete,
        super::CompleteMachineState::Refused("unsupported or nonfinite cell")
    );
    assert_eq!(second.power_watts, None);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_constructor_refuses_nonfinite_load_without_promoting_the_system_group() {
    // Goal: a nonfinite load average keeps the complete summary explicitly refused.
    // Method: submit a typed system row with one NaN average and otherwise valid fields.
    let mut machine = Machine::at(1);
    machine.tick.system = Some(collector::ring::SystemSnapshot {
        reading: collector::system::SystemReading {
            load_average: Some([f64::NAN, 1.0, 1.0]),
            ..collector::system::SystemReading::default()
        },
        ..collector::ring::SystemSnapshot::default()
    });
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let second = MachineSecond::complete_of(&machine.reading(), None, &budget)
        .unwrap()
        .unwrap();

    assert_eq!(
        second.complete,
        super::CompleteMachineState::Refused("system load average")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_constructor_keeps_nan_fans_as_individual_missing_channels() {
    // Goal: a failed fan reading stays a per-channel absence and leaves the other facts usable.
    // Method: retain NaN beside a stopped fan using the same complete numeric boundary.
    let machine = Machine::at(1).smc(12.0, 45.0).fans(&[f32::NAN, 0.0]);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let second = MachineSecond::complete_of(&machine.reading(), None, &budget)
        .unwrap()
        .unwrap();

    assert_eq!(
        second.complete().unwrap().summary(&second)["fan_rpm"],
        serde_json::json!([null, 0.0])
    );
    assert_eq!(second.power_watts, Some(12.0));
}

#[test]
fn complete_constructor_rejects_admission_without_accepting_an_overview_observation() {
    // Goal: a denied scratch reservation cannot degrade a new observation to Overview-only.
    // Method: admit the small retained owner but withhold the separately charged scratch.
    let machine = Machine::at(1).cpus(&[(1, 1)]);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(4096));

    let result = MachineSecond::complete_of(&machine.reading(), None, &budget);

    assert!(matches!(
        result,
        Err(super::MachineCaptureError::Admission(
            super::MachineAdmission::ScratchBytes
        ))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_constructor_marks_the_legacy_summary_refusal_beside_usable_overview_facts() {
    // Goal: a refused CPU detail domain retains explicit compatibility facts, without promotion.
    // Method: a matching CPU 7 remains 25% busy while full summary capture rejects its domain.
    let mut before = Machine::at(1).cpus(&[(0, 0)]);
    let mut current = Machine::at(2).cpus(&[(25, 75)]);
    before.cpus[0].cpu = 7;
    current.cpus[0].cpu = 7;
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let second = MachineSecond::complete_of(&current.reading(), Some(&before.reading()), &budget)
        .unwrap()
        .unwrap();

    assert_eq!(
        second.complete,
        super::CompleteMachineState::Refused("incomplete logical CPU domain")
    );
    assert_eq!(second.cpu_percent, Some(25.0));
    assert!(second.complete().is_none());
    assert_eq!(second.cpu_counters().unwrap()[0].cpu, 7);
    assert!(budget.used() > 0);
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn machine_pool_reclamation_releases_complete_owners_after_the_last_view() {
    // Goal: resetting and reclaiming pools neither leaks reservations nor invalidates a View.
    // Method: reset with a held View, then drop it and reclaim the free machine buffers.
    use collector::ring::{Identity, RunIdentity};
    let machine = Machine::at(1).cpus(&[(1, 1)]);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));
    let second = MachineSecond::complete_of(&machine.reading(), None, &budget)
        .unwrap()
        .unwrap();
    let charged = second.complete().unwrap().bytes();
    let mut held = crate::store::Held::new();
    let run = RunIdentity {
        recording: Identity([1, 2]),
        run: Identity([3, 4]),
        boot: Identity([5, 6]),
    };
    assert!(held.take(
        &run,
        &second,
        crate::store::apps::TickApps::Missing(&crate::store::Missing::Unread,)
    ));
    let mailbox = crate::store::Mailbox::new();
    held.publish(&mailbox);
    let view = mailbox.take().unwrap();
    drop(second);

    held.reset();
    assert_eq!(budget.used(), charged);
    assert_eq!(view.seconds[0].complete().unwrap().cpu_counters()[0].cpu, 0);
    drop(view);
    held.release_views();

    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_projection_keeps_exact_memory_and_independent_rate_presentation() {
    // Goal: a retained machine supplies the Memory page without reading its source again.
    // Method: compare exact large counters and multiplication-first rates with frozen values.
    let before = Machine::at(1).cpus(&[(0, 0)]);
    let mut current = Machine::at(2).cpus(&[(25, 75)]);
    current.tick.continuous_ns = Some(2_000_000_001);
    current.memory.memory.total_bytes = 9_007_199_254_740_993;
    current.memory.memory.pageins = 1;
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));
    let mut second = current.second_after(&before);

    second
        .retain_complete(&current.reading(), Some(&before.reading()), &budget)
        .unwrap();
    let summary = second.complete().unwrap().summary(&second);

    assert_eq!(summary["memory_total_bytes"], 9_007_199_254_740_993_u64);
    assert_eq!(summary["memory_pageins_per_second"], 0.999_999_999);
    assert_eq!(summary["cpu_percent"], 25.0);
    assert_eq!(summary["cpu_percent_one_core"], 25.0);
    assert_eq!(summary["network_rx_packets_per_second"], 0.0);
}

#[test]
fn complete_projection_matches_the_independent_frozen_desktop_summary() {
    // Goal: every existing summary key and absence survives typed retention.
    // Method: reproduce the native fixture's machine facts and compare its frozen old output.
    let mut before = Machine::at(1).cpus(&[(0, 0)]);
    let mut current = Machine::at(2).cpus(&[(1, 0)]);
    for machine in [&mut before, &mut current] {
        machine.tick.wall_ns = Some((1_799_999_999 + machine.tick.publication) * SECOND_NS);
        machine.tick.continuous_ns = Some((99 + machine.tick.publication) * SECOND_NS);
        machine.memory.memory = Memory {
            total_bytes: 8_589_934_592,
            free_bytes: 4_294_967_296,
            ..Memory::default()
        };
    }
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));
    let mut second = current.second_after(&before);
    second.segment = 1;
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("summary_fixture.json")).unwrap();

    second
        .retain_complete(&current.reading(), Some(&before.reading()), &budget)
        .unwrap();

    assert_eq!(second.complete().unwrap().summary(&second), expected);
}

#[test]
fn complete_projection_refusal_preserves_the_store_reading_with_noncontiguous_cpu_ids() {
    // Goal: detail's contiguous CPU domain never becomes a new Store acceptance rule.
    // Method: keep IDs 1 and 3 usable for Overview while the legacy full summary refuses them.
    let mut before = Machine::at(1).cpus(&[(0, 0), (0, 0)]);
    let mut current = Machine::at(2).cpus(&[(25, 75), (75, 25)]);
    before.cpus[0].cpu = 1;
    before.cpus[1].cpu = 3;
    current.cpus[0].cpu = 1;
    current.cpus[1].cpu = 3;
    let mut second = current.second_after(&before);
    let original = second.clone();
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));

    let refusal = second.retain_complete(&current.reading(), Some(&before.reading()), &budget);

    assert!(matches!(
        refusal,
        Err(super::MachineCaptureError::Projection(
            persistence::Error::Bound("incomplete logical CPU domain")
        ))
    ));
    assert_eq!(second, original);
    assert_eq!(second.cpu_percent, Some(50.0));
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_projection_shares_one_charge_until_the_last_retainer_leaves() {
    // Goal: a cloned second shares exact capacities and cannot relabel another observation.
    // Method: retain one CPU, clone it, then alter its public identity and release both owners.
    let machine = Machine::at(1).cpus(&[(1, 1)]);
    let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));
    let mut second = machine.first_second();
    second
        .retain_complete(&machine.reading(), None, &budget)
        .unwrap();
    let charged = second.complete().unwrap().bytes();
    let retained = second.clone();

    second.publication += 1;
    assert!(second.complete().is_none());
    drop(second);

    assert_eq!(budget.used(), charged);
    assert_eq!(retained.complete().unwrap().cpu_counters()[0].cpu, 0);
    assert_eq!(
        retained.complete().unwrap().cpu_counters()[0].ticks,
        machine.cpus[0].ticks
    );
    eprintln!(
        "MachineSecond={} CompleteMachine charge={} CPU row={}",
        size_of::<MachineSecond>(),
        charged,
        size_of_val(&retained.complete().unwrap().cpu_counters()[0])
    );
    drop(retained);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cpu_percent_is_the_busy_share_of_every_cpus_ticks_since_the_previous_second() {
    // Goal: the CPU chart draws the share of all CPUs' ticks that were busy over the second.
    // Method: two CPUs, one busy for 30 of 100 ticks and one for 50 of 100, read a second apart.
    let before = Machine::at(1).cpus(&[(0, 0), (0, 0)]);
    let after = Machine::at(2).cpus(&[(30, 70), (50, 50)]);

    let second = after.second_after(&before);

    assert_eq!(second.cpu_percent, Some(40.0));
}

#[test]
fn all_cpu_adds_up_each_cpus_busy_share_since_the_previous_second() {
    // Goal: CPU by app draws the whole Mac's CPU in its Apps' unit, where one busy CPU is 100%,
    // as the worker's All CPU reference adds it up. Method: two CPUs, one busy for 30 of 100
    // ticks and one for 50 of 100, read a second apart.
    let before = Machine::at(1).cpus(&[(0, 0), (0, 0)]);
    let after = Machine::at(2).cpus(&[(30, 70), (50, 50)]);

    let second = after.second_after(&before);

    assert_eq!(second.cpu_percent_one_core, Some(80.0));
}

#[test]
fn network_rates_are_bytes_per_second_of_the_continuous_clock() {
    // Goal: traffic is a rate over the time the Mac was running, summed over its interfaces.
    // Method: two interfaces move 1,500 bytes in and 600 out over half a second.
    let before = Machine::at(1).interfaces(&[(1, 0, 0), (2, 0, 0)]);
    let after = Machine::at(2)
        .half_a_second_later_than(&before)
        .interfaces(&[(1, 1_000, 100), (2, 500, 500)]);

    let second = after.second_after(&before);

    let expected = Traffic {
        received_bps: 3_000.0,
        sent_bps: 1_200.0,
    };
    assert_eq!(second.network, Some(expected));
}

#[test]
fn a_mac_with_no_interface_moves_no_traffic() {
    // Goal: a Mac with no interface draws no traffic rather than a gap. Method: two seconds a
    // second apart, neither with an interface.
    let second = Machine::at(2).second_after(&Machine::at(1));

    let expected = Traffic {
        received_bps: 0.0,
        sent_bps: 0.0,
    };
    assert_eq!(second.network, Some(expected));
}

#[test]
fn app_memory_is_anonymous_memory_that_macos_may_not_purge() {
    // Goal: the memory chart counts app memory as Activity Monitor does. Method: 800 bytes of
    // anonymous memory, 300 of them purgeable.
    let machine = Machine::at(1).anonymous_and_purgeable(800, 300);

    let second = machine.first_second();

    let expected = MemorySecond {
        total_bytes: 4_096,
        app_bytes: Some(500),
        wired_bytes: 200,
        compressed_bytes: 100,
        pressure: Pressure::Warning,
        swap_used_bytes: 0,
        swap_total_bytes: 0,
    };
    assert_eq!(second.memory, Some(expected));
}

#[test]
fn a_second_holds_the_swap_macos_reported() {
    // Goal: the Overview's utilization of swap reads the swap in use and the swap macOS set
    // aside, as the worker's summary does. Method: 300 of 1,024 bytes of swap in use.
    let machine = Machine::at(1).swap(300, 1_024);

    let second = machine.first_second();

    let memory = second.memory.unwrap();
    assert_eq!(
        (memory.swap_used_bytes, memory.swap_total_bytes),
        (300, 1_024)
    );
}

#[test]
fn a_minutes_readings_pair_each_tick_with_its_own_rows() {
    // Goal: the store reads a minute's rows as one reading a publication, as the ring gives
    // them.
    // Method: two publications with two CPUs and one interface each, the second without a
    // memory row.
    let mut minute = MachineMinute::default();
    Machine::at(1)
        .cpus(&[(1, 1), (2, 2)])
        .interfaces(&[(1, 10, 10)])
        .push_into(&mut minute);
    Machine::at(2)
        .cpus(&[(3, 3), (4, 4)])
        .interfaces(&[(1, 20, 20)])
        .push_into(&mut minute);
    minute.memory.pop();

    let shapes: Vec<Shape> = readings(&minute)
        .map(|reading| Shape::of(&reading))
        .collect();

    let expected = vec![
        Shape {
            publication: 1,
            cpus: vec![1, 2],
            memory: true,
            interfaces: vec![10],
            gpu_states: vec![],
            fans: vec![],
        },
        Shape {
            publication: 2,
            cpus: vec![3, 4],
            memory: false,
            interfaces: vec![20],
            gpu_states: vec![],
            fans: vec![],
        },
    ];
    assert_eq!(shapes, expected);
}

#[test]
fn power_heat_and_thermal_pressure_are_the_ticks_own_readings() {
    // Goal: the power, heat and thermal pressure charts draw what the tick read, with no rate
    // between seconds. Method: a tick that read 12.5 W, 45 °C and thermal pressure 2.
    let machine = Machine::at(1).smc(12.5, 45.0).thermal_pressure(2);

    let second = machine.first_second();

    assert_eq!(second.power_watts, Some(12.5));
    assert_eq!(second.soc_celsius, Some(45.0));
    assert_eq!(second.thermal_pressure, Some(2));
}

#[test]
fn die_temperatures_are_the_ticks_own_readings() {
    // Goal: Overview's CPU and GPU readouts show each die's heat at the second. Method: a tick
    // that read both dies.
    let machine = Machine::at(1).die_temperatures(61.5, 52.25);

    let second = machine.first_second();

    assert_eq!(second.cpu_celsius, Some(61.5));
    assert_eq!(second.gpu_celsius, Some(52.25));
}

#[test]
fn performance_levels_are_the_ticks_own() {
    // Goal: the CPU chart names each level as the second's tick named it. Method: a tick
    // naming six super and twelve performance cores.
    let machine = Machine::at(1).levels(6, 12);

    let second = machine.first_second();

    assert_eq!(second.performance_levels, machine.tick.performance_levels);
}

#[test]
fn fan_speed_is_the_mean_of_the_fans_the_tick_measured() {
    // Goal: the fan chart draws one speed for the Mac however many fans it has. Method: three fans
    // at 1,000, 2,000 and 3,000 rpm.
    let machine = Machine::at(1).fans(&[1_000.0, 2_000.0, 3_000.0]);

    let second = machine.first_second();

    assert_eq!(second.fan_mean_rpm, Some(2_000.0));
}

#[test]
fn each_fan_speed_is_the_ticks_reading_of_that_fan() {
    // Goal: the hardware figure spins each of its two fans at the speed the tick read for it.
    // Method: two fans at 1,200 and 1,500 rpm.
    let machine = Machine::at(1).fans(&[1_200.0, 1_500.0]);

    let second = machine.first_second();

    assert_eq!(second.fan_rpm, [Some(1_200.0), Some(1_500.0)]);
}

#[test]
fn a_fan_at_rest_reads_zero_rpm() {
    // Goal: a fan macOS has stopped shows 0 rpm rather than no reading, as an idle Mac's fans
    // often are. Method: fan 0 at rest and fan 1 at 1,500 rpm.
    let machine = Machine::at(1).fans(&[0.0, 1_500.0]);

    let second = machine.first_second();

    assert_eq!(second.fan_rpm, [Some(0.0), Some(1_500.0)]);
}

#[test]
fn a_fan_is_read_by_its_own_number() {
    // Goal: each fan of the figure shows its own fan's speed, whatever order the rows come in.
    // Method: the rows of fans 1 and 0, in that order.
    let mut machine = Machine::at(1).fans(&[1_500.0, 1_200.0]);
    machine.fans[0].fan = 1;
    machine.fans[1].fan = 0;

    let second = machine.first_second();

    assert_eq!(second.fan_rpm, [Some(1_200.0), Some(1_500.0)]);
}

#[test]
fn battery_charge_and_power_are_the_ticks_battery_reading() {
    // Goal: the hardware figure shows the battery's charge and power at the second. Method: a
    // reading taken at the tick, 40 of 80 units charged, giving 1,500 mA at 12,000 mV.
    let reading = BatteryReading {
        charge_current: Some(40),
        charge_maximum: Some(80),
        current_ma: Some(-1_500),
        voltage_mv: Some(12_000),
        ..BatteryReading::default()
    };
    let machine = Machine::at(1).battery_read_before(reading, 0);

    let second = machine.first_second();

    let expected = BatterySecond {
        charge_percent: Some(50.0),
        power_watts: Some(-18.0),
    };
    assert_eq!(second.battery, Some(expected));
}

#[test]
fn gpu_activity_is_the_share_of_the_second_the_gpu_spent_in_its_states() {
    // Goal: the GPU chart draws how busy the GPU was over the second. Method: two states that
    // were active for 200 ms and 300 ms of a second.
    let before = Machine::at(1).gpu_states(&[100_000_000, 0]);
    let after = Machine::at(2).gpu_states(&[300_000_000, 300_000_000]);

    let second = after.second_after(&before);

    assert_eq!(second.gpu_active_percent, Some(50.0));
}

#[test]
fn each_performance_level_reads_the_mean_busy_share_of_its_own_cpus() {
    // Goal: the CPU chart draws each level's activity apart, the slower level's CPUs taking
    // the lowest indices as macOS numbers them. Method: one fast CPU at 100% and two slower
    // CPUs at 0% and 50%.
    let before = Machine::at(1).levels(1, 2).cpus(&[(0, 0), (0, 0), (0, 0)]);
    let after = Machine::at(2)
        .levels(1, 2)
        .cpus(&[(0, 100), (50, 50), (100, 0)]);

    let second = after.second_after(&before);

    assert_eq!(second.cpu_level_percent, [Some(100.0), Some(25.0)]);
}

#[test]
fn disk_throughput_is_bytes_per_second_summed_over_the_disks() {
    // Goal: the disk chart draws the Mac's throughput over all its disks. Method: two disks that
    // read 1,000 and 500 bytes and wrote 300 and 200 over a second.
    let before = Machine::at(1).disks(&[(1, 0, 0), (2, 0, 0)]);
    let after = Machine::at(2).disks(&[(1, 1_000, 300), (2, 500, 200)]);

    let second = after.second_after(&before);

    assert_eq!(second.disk_read_bps, Some(1_500.0));
    assert_eq!(second.disk_written_bps, Some(500.0));
}

#[test]
fn a_minutes_readings_pair_each_tick_with_its_own_gpu_states_and_fans() {
    // Goal: each second reads only its own tick's GPU states and fans. Method: two publications
    // with two GPU states each, the first with a fan and the second with none.
    let mut minute = MachineMinute::default();
    Machine::at(1)
        .gpu_states(&[10, 11])
        .fans(&[100.0])
        .push_into(&mut minute);
    Machine::at(2).gpu_states(&[20, 21]).push_into(&mut minute);

    let shapes: Vec<Shape> = readings(&minute)
        .map(|reading| Shape::of(&reading))
        .collect();

    assert_eq!(shapes[0].gpu_states, [10, 11]);
    assert_eq!(shapes[0].fans, [100.0]);
    assert_eq!(shapes[1].gpu_states, [20, 21]);
    assert!(shapes[1].fans.is_empty());
}

#[test]
fn a_tick_without_a_wall_clock_has_no_second() {
    // Goal: a tick the store cannot place on the clock draws nothing. Method: a tick with no
    // clocks.
    let machine = Machine::at(1).without_clocks();

    let second = MachineSecond::of(&machine.reading(), None);

    assert_eq!(second, None);
}

mod when_the_previous_reading_is_not_the_one_just_before {
    use super::{Machine, SECOND_NS};

    #[test]
    fn the_first_second_of_a_run_has_no_rates() {
        // Goal: a run's first second draws no rate, since no earlier counter starts one. Method:
        // one CPU's ticks with no previous reading.
        let second = Machine::at(1).cpus(&[(1, 1)]).first_second();

        assert!(!second.follows);
        assert_eq!(second.cpu_percent, None);
        assert_eq!(second.network, None);
    }

    #[test]
    fn a_missed_publication_leaves_the_rates_unknown() {
        // Goal: a rate never spans a publication the store did not read. Method: publications 1 and
        // 3, whose run ticks and wall clocks follow each other.
        let before = Machine::at(1).cpus(&[(0, 0)]);
        let after = Machine::at(3)
            .run_tick(1)
            .wall_ns(1_002 * SECOND_NS)
            .cpus(&[(10, 10)]);

        let second = after.second_after(&before);

        assert!(!second.follows);
        assert_eq!(second.cpu_percent, None);
    }

    #[test]
    fn a_missed_tick_leaves_the_rates_unknown() {
        // Goal: a rate never spans a tick the collector missed. Method: consecutive publications at
        // run ticks 0 and 5.
        let before = Machine::at(1).cpus(&[(0, 0)]);
        let after = Machine::at(2).run_tick(5).cpus(&[(10, 10)]);

        let second = after.second_after(&before);

        assert!(!second.follows);
    }

    #[test]
    fn a_wall_clock_more_than_a_second_later_leaves_the_rates_unknown() {
        // Goal: a rate never spans more than a second of the wall clock. Method: consecutive
        // publications two seconds apart.
        let before = Machine::at(1).cpus(&[(0, 0)]);
        let after = Machine::at(2).wall_ns(1_003 * SECOND_NS).cpus(&[(10, 10)]);

        let second = after.second_after(&before);

        assert!(!second.follows);
    }

    #[test]
    fn a_wall_clock_that_went_back_leaves_the_rates_unknown() {
        // Goal: a wall clock set back never makes a rate. Method: a publication two seconds before
        // the previous one by the wall clock.
        let before = Machine::at(2).cpus(&[(0, 0)]);
        let after = Machine::at(3).wall_ns(1_000 * SECOND_NS).cpus(&[(10, 10)]);

        let second = after.second_after(&before);

        assert!(!second.follows);
    }
}

mod when_the_readings_do_not_match {
    use super::{BatteryReading, Machine, READ_INTERVAL_NS, ScanState};

    #[test]
    fn a_power_or_heat_reading_that_is_no_number_is_unknown() {
        // Goal: a chart draws a gap where the sensor read no number, as the worker's readings
        // draw one, and never fails to draw. Method: a tick whose power and every temperature
        // read NaN or an infinity.
        let machine = Machine::at(1)
            .smc(f32::NAN, f32::INFINITY)
            .die_temperatures(f32::NEG_INFINITY, f32::NAN);

        let second = machine.first_second();

        assert_eq!(second.power_watts, None);
        assert_eq!(second.soc_celsius, None);
        assert_eq!(second.cpu_celsius, None);
        assert_eq!(second.gpu_celsius, None);
    }

    #[test]
    fn a_cpu_counter_that_went_back_leaves_the_cpu_percent_unknown() {
        // Goal: a counter that went back draws no CPU share rather than a wrong one. Method: a CPU
        // whose busy ticks fall from 10 to 5.
        let before = Machine::at(1).cpus(&[(10, 10)]);
        let after = Machine::at(2).cpus(&[(5, 20)]);

        let second = after.second_after(&before);

        assert!(second.follows);
        assert_eq!(second.cpu_percent, None);
    }

    #[test]
    fn a_cpu_counter_that_went_back_leaves_all_cpu_unknown() {
        // Goal: All CPU reads no sum rather than one that leaves a CPU out. Method: two CPUs, the
        // second's busy ticks falling from 10 to 5.
        let before = Machine::at(1).cpus(&[(10, 10), (10, 10)]);
        let after = Machine::at(2).cpus(&[(20, 20), (5, 20)]);

        let second = after.second_after(&before);

        assert!(second.follows);
        assert_eq!(second.cpu_percent_one_core, None);
    }

    #[test]
    fn a_cpu_that_appeared_leaves_the_cpu_percent_unknown() {
        // Goal: the CPU share compares the same CPUs in both seconds. Method: one CPU, then two.
        let before = Machine::at(1).cpus(&[(0, 0)]);
        let after = Machine::at(2).cpus(&[(10, 10), (10, 10)]);

        let second = after.second_after(&before);

        assert_eq!(second.cpu_percent, None);
    }

    #[test]
    fn an_interface_that_appeared_leaves_the_traffic_unknown() {
        // Goal: traffic compares the same interfaces in both seconds. Method: one interface, then
        // two.
        let before = Machine::at(1).interfaces(&[(1, 0, 0)]);
        let after = Machine::at(2).interfaces(&[(1, 10, 10), (2, 10, 10)]);

        let second = after.second_after(&before);

        assert_eq!(second.network, None);
    }

    #[test]
    fn an_interface_replaced_by_another_leaves_the_traffic_unknown() {
        // Goal: traffic never subtracts one interface's counters from another's. Method: interface
        // 1, then interface 2 alone.
        let before = Machine::at(1).interfaces(&[(1, 0, 0)]);
        let after = Machine::at(2).interfaces(&[(2, 10, 10)]);

        let second = after.second_after(&before);

        assert_eq!(second.network, None);
    }

    #[test]
    fn an_index_taken_by_another_interface_leaves_the_traffic_unknown() {
        // Goal: traffic compares an interface only with itself, known by its index and its name.
        // Method: index 1 named en1, then named utun1.
        let before = Machine::at(1).interfaces(&[(1, 0, 0)]);
        let after = Machine::at(2)
            .interfaces(&[(1, 10, 10)])
            .interfaces_named("utun1");

        let second = after.second_after(&before);

        assert_eq!(second.network, None);
    }

    #[test]
    fn a_gpu_state_that_went_away_leaves_the_gpu_activity_unknown() {
        // Goal: GPU activity compares the same states in both seconds. Method: three states, then
        // two.
        let before = Machine::at(1).gpu_states(&[0, 0, 0]);
        let after = Machine::at(2).gpu_states(&[10, 10]);

        let second = after.second_after(&before);

        assert_eq!(second.gpu_active_percent, None);
    }

    #[test]
    fn levels_that_do_not_cover_the_cpus_leave_each_levels_activity_unknown() {
        // Goal: a level's activity is drawn only where the levels name every CPU. Method: two
        // levels of one CPU each over three CPUs.
        let before = Machine::at(1).levels(1, 1).cpus(&[(0, 0), (0, 0), (0, 0)]);
        let after = Machine::at(2).levels(1, 1).cpus(&[(1, 1), (1, 1), (1, 1)]);

        let second = after.second_after(&before);

        assert_eq!(second.cpu_level_percent, [None, None]);
    }

    #[test]
    fn a_disk_scan_that_did_not_complete_leaves_the_disk_throughput_unknown() {
        // Goal: a partial scan draws no throughput rather than one missing some disks. Method: a
        // disk read over a second whose scan was partial.
        let before = Machine::at(1).disks(&[(1, 0, 0)]);
        let after = Machine::at(2)
            .disks(&[(1, 1_000, 1_000)])
            .disk_scan(ScanState::Partial);

        let second = after.second_after(&before);

        assert_eq!(second.disk_read_bps, None);
        assert_eq!(second.disk_written_bps, None);
    }

    #[test]
    fn a_disk_scan_taken_apart_from_its_tick_leaves_the_disk_throughput_unknown() {
        // Goal: throughput divides by the time between two ticks, so it reads only a scan taken at
        // its tick. Method: a scan stamped a second after the previous tick, in a tick half a
        // second after it.
        let before = Machine::at(1).disks(&[(1, 0, 0)]);
        let after = Machine::at(2)
            .disks(&[(1, 1_000, 1_000)])
            .half_a_second_later_than(&before);

        let second = after.second_after(&before);

        assert_eq!(second.disk_read_bps, None);
    }

    #[test]
    fn a_fan_reading_that_is_no_speed_is_left_out_of_the_mean() {
        // Goal: a fan reading that is no speed leaves the mean of the other fans as it is. Method:
        // fans at 1,000 and 3,000 rpm beside a NaN and a negative reading.
        let machine = Machine::at(1).fans(&[1_000.0, f32::NAN, -5.0, 3_000.0]);

        let second = machine.first_second();

        assert_eq!(second.fan_mean_rpm, Some(2_000.0));
    }

    #[test]
    fn a_fan_reading_that_is_no_speed_leaves_that_fan_without_one() {
        // Goal: a fan whose reading is no speed shows none, and the other fan its own. Method:
        // fan 0 read NaN and fan 1 1,500 rpm.
        let machine = Machine::at(1).fans(&[f32::NAN, 1_500.0]);

        let second = machine.first_second();

        assert_eq!(second.fan_rpm, [None, Some(1_500.0)]);
    }

    #[test]
    fn a_battery_reading_older_than_its_read_interval_shows_no_battery() {
        // Goal: the figure never shows a battery reading the collector has since replaced, as
        // the worker's summary of the second does not. Method: a reading one nanosecond older
        // than the interval the collector reads the battery at.
        let reading = BatteryReading {
            charge_current: Some(40),
            charge_maximum: Some(80),
            ..BatteryReading::default()
        };
        let machine = Machine::at(100).battery_read_before(reading, READ_INTERVAL_NS + 1);

        let second = machine.first_second();

        assert_eq!(second.battery, None);
    }

    #[test]
    fn a_tick_that_measured_no_fan_has_no_fan_speed() {
        // Goal: a tick that read no fan draws a gap rather than 0 rpm. Method: a tick with no fan.
        let second = Machine::at(1).first_second();

        assert_eq!(second.fan_mean_rpm, None);
    }

    #[test]
    fn purgeable_memory_above_anonymous_memory_leaves_app_memory_unknown() {
        // Goal: app memory is never a count wrapped below zero. Method: 300 bytes of anonymous
        // memory with 800 purgeable.
        let machine = Machine::at(1).anonymous_and_purgeable(300, 800);

        let second = machine.first_second();

        assert_eq!(second.memory.unwrap().app_bytes, None);
    }
}

/// One publication's machine readings, built for a test.
struct Machine {
    tick: TickRow,
    cpus: Vec<CpuRow>,
    memory: MemoryRow,
    interfaces: Vec<InterfaceRow>,
    gpu_states: Vec<GpuStateRow>,
    fans: Vec<FanRow>,
}

impl Machine {
    /// Publication `publication`, its run tick one less, `publication` seconds into the run.
    fn at(publication: u64) -> Self {
        let memory = Memory {
            total_bytes: 4_096,
            anonymous_bytes: 600,
            purgeable_bytes: 100,
            wired_bytes: 200,
            compressed_bytes: 100,
            pressure: Pressure::Warning,
            ..Memory::default()
        };
        Self {
            tick: TickRow {
                publication,
                run_tick: publication - 1,
                wall_ns: Some((1_000 + publication) * SECOND_NS),
                continuous_ns: Some(publication * SECOND_NS),
                awake_ns: Some(publication * SECOND_NS),
                ioreport_read: false,
                smc_read: false,
                gpu_energy_nj: None,
                power_watts: None,
                soc_celsius: None,
                cpu_celsius: None,
                gpu_celsius: None,
                thermal_pressure: None,
                performance_levels: None,
                system: None,
                saved_computer: None,
                battery: None,
                disks: None,
            },
            cpus: Vec::new(),
            memory: MemoryRow {
                publication,
                memory,
            },
            interfaces: Vec::new(),
            gpu_states: Vec::new(),
            fans: Vec::new(),
        }
    }

    fn run_tick(mut self, run_tick: u64) -> Self {
        self.tick.run_tick = run_tick;
        self
    }

    fn wall_ns(mut self, wall_ns: u64) -> Self {
        self.tick.wall_ns = Some(wall_ns);
        self
    }

    fn without_clocks(mut self) -> Self {
        self.tick.wall_ns = None;
        self.tick.continuous_ns = None;
        self.tick.awake_ns = None;
        self
    }

    fn half_a_second_later_than(mut self, before: &Self) -> Self {
        let continuous_ns = before.tick.continuous_ns.unwrap() + SECOND_NS / 2;
        self.tick.continuous_ns = Some(continuous_ns);
        self
    }

    /// One CPU for each `(busy, idle)`, its busy ticks split over user, system and nice.
    fn cpus(mut self, cpus: &[(u32, u32)]) -> Self {
        self.cpus = (0_u32..)
            .zip(cpus)
            .map(|(cpu, &(busy, idle))| CpuRow {
                publication: self.tick.publication,
                cpu,
                ticks: CPUTicks {
                    user: busy / 2,
                    system: busy - busy / 2 - busy / 5,
                    nice: busy / 5,
                    idle,
                },
            })
            .collect();
        self
    }

    /// One interface for each `(index, received, sent)` count of bytes.
    fn interfaces(mut self, interfaces: &[(u16, u64, u64)]) -> Self {
        self.interfaces = interfaces
            .iter()
            .map(|&(index, received_bytes, sent_bytes)| InterfaceRow {
                publication: self.tick.publication,
                interface: Interface {
                    index,
                    name: interface_name(&format!("en{index}")),
                    received_bytes,
                    sent_bytes,
                    received_packets: 0,
                    sent_packets: 0,
                    receive_errors: 0,
                    send_errors: 0,
                    receive_drops: 0,
                },
            })
            .collect();
        self
    }

    /// Gives every interface the name `name`.
    fn interfaces_named(mut self, name: &str) -> Self {
        for row in &mut self.interfaces {
            row.interface.name = interface_name(name);
        }
        self
    }

    fn swap(mut self, used_bytes: u64, total_bytes: u64) -> Self {
        self.memory.memory.swap_used_bytes = used_bytes;
        self.memory.memory.swap_total_bytes = total_bytes;
        self
    }

    fn anonymous_and_purgeable(mut self, anonymous_bytes: u64, purgeable_bytes: u64) -> Self {
        self.memory.memory.anonymous_bytes = anonymous_bytes;
        self.memory.memory.purgeable_bytes = purgeable_bytes;
        self
    }

    fn smc(mut self, power_watts: f32, soc_celsius: f32) -> Self {
        self.tick.smc_read = true;
        self.tick.power_watts = Some(power_watts);
        self.tick.soc_celsius = Some(soc_celsius);
        self
    }

    fn die_temperatures(mut self, cpu_celsius: f32, gpu_celsius: f32) -> Self {
        self.tick.smc_read = true;
        self.tick.cpu_celsius = Some(cpu_celsius);
        self.tick.gpu_celsius = Some(gpu_celsius);
        self
    }

    fn thermal_pressure(mut self, level: u8) -> Self {
        self.tick.thermal_pressure = Some(level);
        self
    }

    /// The battery reading `reading`, taken `age_ns` of the continuous clock before the tick.
    fn battery_read_before(mut self, reading: BatteryReading, age_ns: u64) -> Self {
        let source_continuous_ns = self.tick.continuous_ns.unwrap() - age_ns;

        self.tick.battery = Some(BatteryReading {
            source_continuous_ns,
            ..reading
        });
        self
    }

    /// One fan for each speed, read from the SMC.
    fn fans(mut self, rpm: &[f32]) -> Self {
        self.tick.smc_read = true;
        self.fans = (0_u32..)
            .zip(rpm)
            .map(|(fan, &rpm)| FanRow {
                publication: self.tick.publication,
                fan,
                rpm,
            })
            .collect();
        self
    }

    /// One GPU state for each count of nanoseconds it was active, read from `IOReport`.
    fn gpu_states(mut self, active_ns: &[u64]) -> Self {
        self.tick.ioreport_read = true;
        self.gpu_states = (0_u32..)
            .zip(active_ns)
            .map(|(state, &active_ns)| GpuStateRow {
                publication: self.tick.publication,
                state,
                active_ns,
            })
            .collect();
        self
    }

    /// Two performance levels of `fast` and `slow` logical CPUs.
    fn levels(mut self, fast: u32, slow: u32) -> Self {
        let level = |name: &str, logical_cpus| PerformanceLevel {
            name: FactText::new(name.as_bytes()).unwrap(),
            logical_cpus,
        };
        self.tick.performance_levels = Some([level("Super", fast), level("Performance", slow)]);
        self.tick.system = Some(collector::ring::SystemSnapshot {
            machine: collector::system::MachineFacts {
                levels: self.tick.performance_levels,
                ..collector::system::MachineFacts::default()
            },
            ..collector::ring::SystemSnapshot::default()
        });
        self
    }

    /// A complete scan of one disk for each `(driver, read, written)` count of bytes, taken
    /// at the tick.
    fn disks(mut self, disks: &[(u64, u64, u64)]) -> Self {
        let mut snapshot = DiskSnapshot {
            source_continuous_ns: self.tick.continuous_ns.unwrap(),
            state: ScanState::Complete,
            count: disks.len(),
            ..DiskSnapshot::default()
        };
        for (record, &(driver_id, read_bytes, write_bytes)) in
            snapshot.records.iter_mut().zip(disks)
        {
            *record = DiskReading {
                driver_id,
                read_bytes: Some(read_bytes),
                write_bytes: Some(write_bytes),
                ..DiskReading::default()
            };
        }
        self.tick.disks = Some(snapshot);
        self
    }

    fn disk_scan(mut self, state: ScanState) -> Self {
        let Some(snapshot) = &mut self.tick.disks else {
            unreachable!("a disk scan's state is set after its disks");
        };
        snapshot.state = state;
        self
    }

    fn reading(&self) -> Reading<'_> {
        Reading {
            tick: &self.tick,
            cpus: &self.cpus,
            memory: Some(&self.memory),
            interfaces: &self.interfaces,
            gpu_states: &self.gpu_states,
            core_states: &[],
            fans: &self.fans,
        }
    }

    fn first_second(&self) -> MachineSecond {
        self.complete_second(None)
    }

    fn second_after(&self, before: &Self) -> MachineSecond {
        self.complete_second(Some(&before.reading()))
    }

    fn complete_second(&self, before: Option<&Reading<'_>>) -> MachineSecond {
        let budget = std::sync::Arc::new(crate::byte_budget::ByteBudget::new(16 * 1024 * 1024));
        let expected = MachineSecond::of(&self.reading(), before).unwrap();
        let mut second = MachineSecond::complete_of(&self.reading(), before, &budget)
            .unwrap()
            .unwrap();
        second.complete = super::CompleteMachineState::Unread;
        second.cpu_refused = None;
        assert_eq!(second, expected);
        second
    }

    fn push_into(self, minute: &mut MachineMinute) {
        minute.ticks.push(self.tick);
        minute.cpus.extend(self.cpus);
        minute.memory.push(self.memory);
        minute.interfaces.extend(self.interfaces);
        minute.gpu_states.extend(self.gpu_states);
        minute.fans.extend(self.fans);
    }
}

fn interface_name(text: &str) -> Name {
    let mut bytes = [0_u8; IFNAMSIZ];
    bytes[..text.len()].copy_from_slice(text.as_bytes());
    Name::from_bytes(bytes, text.len()).unwrap()
}

/// What a reading holds, by the values that tell its rows apart.
#[derive(Debug, PartialEq)]
struct Shape {
    publication: u64,
    cpus: Vec<u32>,
    memory: bool,
    interfaces: Vec<u64>,
    gpu_states: Vec<u64>,
    fans: Vec<f32>,
}

impl Shape {
    fn of(reading: &Reading<'_>) -> Self {
        Self {
            publication: reading.tick.publication,
            cpus: reading.cpus.iter().map(|row| row.ticks.idle).collect(),
            memory: reading.memory.is_some(),
            interfaces: reading
                .interfaces
                .iter()
                .map(|row| row.interface.received_bytes)
                .collect(),
            gpu_states: reading.gpu_states.iter().map(|row| row.active_ns).collect(),
            fans: reading.fans.iter().map(|row| row.rpm).collect(),
        }
    }
}

#[test]
fn retains_cpu_activity_for_matching_noncontiguous_cpu_identities() {
    // Goal: sharing heatmap arithmetic cannot impose its complete-domain policy on Store rates.
    // Method: retain CPUs 0 and 7 with 30% and 50% activity, as the original Store paired them.
    let mut before = Machine::at(1).cpus(&[(0, 0), (0, 0)]);
    let mut after = Machine::at(2).cpus(&[(30, 70), (50, 50)]);
    before.cpus[1].cpu = 7;
    after.cpus[1].cpu = 7;

    let second = after.second_after(&before);

    assert_eq!(second.cpu_percent, Some(40.0));
    assert_eq!(second.cpu_percent_one_core, Some(80.0));
}

#[test]
fn pairs_aggregate_cpu_activity_by_identity_after_rows_change_order() {
    // Goal: aggregate Store CPU activity preserves its identity-based pairing policy.
    // Method: reverse only current rows; the position-based per-core sum remains unavailable.
    let before = Machine::at(1).cpus(&[(0, 0), (0, 0)]);
    let mut after = Machine::at(2).cpus(&[(30, 70), (50, 50)]);
    after.cpus.reverse();

    let second = after.second_after(&before);

    assert_eq!(second.cpu_percent, Some(40.0));
    assert_eq!(second.cpu_percent_one_core, None);
}
