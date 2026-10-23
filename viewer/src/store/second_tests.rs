use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::system::{FactText, PerformanceLevel};

use super::{MachineSecond, MemorySecond, Reading, Traffic, readings};
use crate::recording::{
    CpuRow, FanRow, GpuStateRow, InterfaceRow, MachineMinute, MemoryRow, TickRow,
};

const SECOND_NS: u64 = 1_000_000_000;

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
    let second = Machine::at(2).second_after(&Machine::at(1));

    let expected = Traffic {
        received_bps: 0.0,
        sent_bps: 0.0,
    };
    assert_eq!(second.network, Some(expected));
}

#[test]
fn app_memory_is_anonymous_memory_that_macos_may_not_purge() {
    let machine = Machine::at(1).anonymous_and_purgeable(800, 300);

    let second = machine.first_second();

    let expected = MemorySecond {
        total_bytes: 4_096,
        app_bytes: Some(500),
        wired_bytes: 200,
        compressed_bytes: 100,
        pressure: Pressure::Warning,
    };
    assert_eq!(second.memory, Some(expected));
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
    let machine = Machine::at(1).smc(12.5, 45.0).thermal_pressure(2);

    let second = machine.first_second();

    assert_eq!(second.power_watts, Some(12.5));
    assert_eq!(second.soc_celsius, Some(45.0));
    assert_eq!(second.thermal_pressure, Some(2));
}

#[test]
fn fan_speed_is_the_mean_of_the_fans_the_tick_measured() {
    let machine = Machine::at(1).fans(&[1_000.0, 2_000.0, 3_000.0]);

    let second = machine.first_second();

    assert_eq!(second.fan_mean_rpm, Some(2_000.0));
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
    let before = Machine::at(1).disks(&[(1, 0, 0), (2, 0, 0)]);
    let after = Machine::at(2).disks(&[(1, 1_000, 300), (2, 500, 200)]);

    let second = after.second_after(&before);

    assert_eq!(second.disk_read_bps, Some(1_500.0));
    assert_eq!(second.disk_written_bps, Some(500.0));
}

#[test]
fn a_minutes_readings_pair_each_tick_with_its_own_gpu_states_and_fans() {
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
    let machine = Machine::at(1).without_clocks();

    let second = MachineSecond::of(&machine.reading(), None);

    assert_eq!(second, None);
}

mod when_the_previous_reading_is_not_the_one_just_before {
    use super::{Machine, SECOND_NS};

    #[test]
    fn the_first_second_of_a_run_has_no_rates() {
        let second = Machine::at(1).cpus(&[(1, 1)]).first_second();

        assert!(!second.follows);
        assert_eq!(second.cpu_percent, None);
        assert_eq!(second.network, None);
    }

    #[test]
    fn a_missed_publication_leaves_the_rates_unknown() {
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
        let before = Machine::at(1).cpus(&[(0, 0)]);
        let after = Machine::at(2).run_tick(5).cpus(&[(10, 10)]);

        let second = after.second_after(&before);

        assert!(!second.follows);
    }

    #[test]
    fn a_wall_clock_more_than_a_second_later_leaves_the_rates_unknown() {
        let before = Machine::at(1).cpus(&[(0, 0)]);
        let after = Machine::at(2).wall_ns(1_003 * SECOND_NS).cpus(&[(10, 10)]);

        let second = after.second_after(&before);

        assert!(!second.follows);
    }

    #[test]
    fn a_wall_clock_that_went_back_leaves_the_rates_unknown() {
        let before = Machine::at(2).cpus(&[(0, 0)]);
        let after = Machine::at(3).wall_ns(1_000 * SECOND_NS).cpus(&[(10, 10)]);

        let second = after.second_after(&before);

        assert!(!second.follows);
    }
}

mod when_the_readings_do_not_match {
    use super::{Machine, ScanState};

    #[test]
    fn a_cpu_counter_that_went_back_leaves_the_cpu_percent_unknown() {
        let before = Machine::at(1).cpus(&[(10, 10)]);
        let after = Machine::at(2).cpus(&[(5, 20)]);

        let second = after.second_after(&before);

        assert!(second.follows);
        assert_eq!(second.cpu_percent, None);
    }

    #[test]
    fn a_cpu_that_appeared_leaves_the_cpu_percent_unknown() {
        let before = Machine::at(1).cpus(&[(0, 0)]);
        let after = Machine::at(2).cpus(&[(10, 10), (10, 10)]);

        let second = after.second_after(&before);

        assert_eq!(second.cpu_percent, None);
    }

    #[test]
    fn an_interface_that_appeared_leaves_the_traffic_unknown() {
        let before = Machine::at(1).interfaces(&[(1, 0, 0)]);
        let after = Machine::at(2).interfaces(&[(1, 10, 10), (2, 10, 10)]);

        let second = after.second_after(&before);

        assert_eq!(second.network, None);
    }

    #[test]
    fn an_interface_replaced_by_another_leaves_the_traffic_unknown() {
        let before = Machine::at(1).interfaces(&[(1, 0, 0)]);
        let after = Machine::at(2).interfaces(&[(2, 10, 10)]);

        let second = after.second_after(&before);

        assert_eq!(second.network, None);
    }

    #[test]
    fn an_index_taken_by_another_interface_leaves_the_traffic_unknown() {
        let before = Machine::at(1).interfaces(&[(1, 0, 0)]);
        let after = Machine::at(2)
            .interfaces(&[(1, 10, 10)])
            .interfaces_named("utun1");

        let second = after.second_after(&before);

        assert_eq!(second.network, None);
    }

    #[test]
    fn a_gpu_state_that_went_away_leaves_the_gpu_activity_unknown() {
        let before = Machine::at(1).gpu_states(&[0, 0, 0]);
        let after = Machine::at(2).gpu_states(&[10, 10]);

        let second = after.second_after(&before);

        assert_eq!(second.gpu_active_percent, None);
    }

    #[test]
    fn levels_that_do_not_cover_the_cpus_leave_each_levels_activity_unknown() {
        let before = Machine::at(1).levels(1, 1).cpus(&[(0, 0), (0, 0), (0, 0)]);
        let after = Machine::at(2).levels(1, 1).cpus(&[(1, 1), (1, 1), (1, 1)]);

        let second = after.second_after(&before);

        assert_eq!(second.cpu_level_percent, [None, None]);
    }

    #[test]
    fn a_disk_scan_that_did_not_complete_leaves_the_disk_throughput_unknown() {
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
        let before = Machine::at(1).disks(&[(1, 0, 0)]);
        let after = Machine::at(2)
            .disks(&[(1, 1_000, 1_000)])
            .half_a_second_later_than(&before);

        let second = after.second_after(&before);

        assert_eq!(second.disk_read_bps, None);
    }

    #[test]
    fn a_fan_reading_that_is_no_speed_is_left_out_of_the_mean() {
        let machine = Machine::at(1).fans(&[1_000.0, f32::NAN, -5.0, 3_000.0]);

        let second = machine.first_second();

        assert_eq!(second.fan_mean_rpm, Some(2_000.0));
    }

    #[test]
    fn a_tick_that_measured_no_fan_has_no_fan_speed() {
        let second = Machine::at(1).first_second();

        assert_eq!(second.fan_mean_rpm, None);
    }

    #[test]
    fn purgeable_memory_above_anonymous_memory_leaves_app_memory_unknown() {
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

    fn thermal_pressure(mut self, level: u8) -> Self {
        self.tick.thermal_pressure = Some(level);
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
            fans: &self.fans,
        }
    }

    fn first_second(&self) -> MachineSecond {
        MachineSecond::of(&self.reading(), None).unwrap()
    }

    fn second_after(&self, before: &Self) -> MachineSecond {
        MachineSecond::of(&self.reading(), Some(&before.reading())).unwrap()
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
