//! The reference input generator, run as a user runs it, and the readings it leaves read back from
//! the recording's saved minutes.

#[cfg(test)]
mod saved_windows;

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, OnceLock};

    use collector::disks::DiskSnapshot;
    use collector::machine::CPUTicks;
    use collector::recording::Registry;
    use collector::ring::{Identity, RunIdentity};
    use persistence::Manifest;
    use persistence::resources::ByteBudget;

    use super::saved_windows;

    /// A shortened input, whose saved minutes reach past the special seconds 300 to 306, since
    /// the minute an input ends in stays in its ring.
    const SECONDS: u64 = 370;
    const ORIGIN_WALL_NS: u64 = 1_800_000_000 * NANOSECONDS_PER_SECOND;
    const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
    const PROCESS_ZERO_PID: i32 = 41_000;
    const GIB: u64 = 1 << 30;
    /// The first period's incident seconds, one at each position of a ten-second bucket.
    const INCIDENT_SECONDS: [u64; 10] = [10, 21, 32, 43, 54, 65, 76, 87, 98, 109];

    #[test]
    fn saves_process_zero_cpu_time_above_two_to_the_fifty_third_exactly() {
        // Goal: the input's CPU counters stay exact above 2^53 through the ring and the seal.
        // Method: read process 0's CPU time at second 1 from its saved minute.
        let cpu_ns = input().process_counters(PROCESS_ZERO_PID, 1).cpu_ns;

        assert_eq!((1 << 53) + 1_000_000, cpu_ns);
    }

    #[test]
    fn saves_no_reading_at_the_missed_tick() {
        // Goal: second 301 is a missed tick, which leaves no readings, rather than readings of
        // zero. Method: look for the saved tick of each second from 300 to 302.
        let saved = [300, 301, 302].map(|second| input().saved_tick(second).is_some());

        assert_eq!([true, false, true], saved);
    }

    #[test]
    fn advances_no_process_counter_at_the_zero_second_or_the_missed_tick() {
        // Goal: neither second 300, which reads zero, nor the missed tick 301 adds to a
        // process's counters. Method: read process 0's counters at seconds 299, 300 and 302,
        // which follow 299, 299 and 300 of the contract's valid seconds, ten of them incidents.
        let counters =
            [299, 300, 302].map(|second| input().process_counters(PROCESS_ZERO_PID, second));

        assert_eq!(
            [(299, 10), (299, 10), (300, 10)]
                .map(|(valid, incidents)| process_zero_counters(valid, incidents)),
            counters
        );
    }

    #[test]
    fn grows_process_zero_cpu_and_energy_twentyfold_in_each_incident_second() {
        // Goal: process 0, the incident process, spends twenty times its CPU time and energy in
        // each incident second, and its disk counters keep their pace. Method: read its growth
        // over each of the first period's incident seconds and over the second after each.
        let growth = INCIDENT_SECONDS.map(|second| {
            [second, second + 1].map(|second| input().process_growth(PROCESS_ZERO_PID, second))
        });

        assert_eq!(
            [[
                ProcessCounters {
                    cpu_ns: 20_000_000,
                    energy_nj: 20_000,
                    disk_read_bytes: 4_096,
                    disk_written_bytes: 2_048,
                },
                ProcessCounters {
                    cpu_ns: 1_000_000,
                    energy_nj: 1_000,
                    disk_read_bytes: 4_096,
                    disk_written_bytes: 2_048,
                },
            ]; 10],
            growth
        );
    }

    #[test]
    fn reads_every_gauge_at_its_peak_in_each_incident_second() {
        // Goal: each incident second reads every gauge the contract names at its peak. Method:
        // read the gauges in each of the first period's incident seconds.
        let gauges = INCIDENT_SECONDS.map(|second| input().gauges(second));

        assert_eq!(
            vec![
                Gauges {
                    gpu_active_ns: 900_000_000,
                    smc: Smc {
                        power_watts: Some(40.0),
                        fan_rpm: vec![4_000.0, 4_100.0],
                        temperatures_celsius: [Some(80.0), Some(81.0), Some(82.0)],
                    },
                    memory_used_bytes: 12 * GIB,
                };
                10
            ],
            gauges.to_vec()
        );
    }

    #[test]
    fn idles_each_core_through_the_zero_second_and_the_missed_tick() {
        // Goal: a core works only in the contract's valid seconds, and idles a hundred ticks in
        // each other second. Method: read core 0's ticks at seconds 299, 300 and 302, which
        // follow 299, 299 and 300 valid seconds and none, one and two others.
        let ticks = [299, 300, 302].map(|second| input().cpu_ticks(0, second));

        assert_eq!(
            [
                core_zero_ticks(299, 0),
                core_zero_ticks(299, 1),
                core_zero_ticks(300, 2)
            ],
            ticks
        );
    }

    #[test]
    fn saves_the_contracts_hardware_in_a_second() {
        // Goal: a second holds every device and field the contract lists. Method: count what
        // second 1's saved tick holds of each.
        let hardware = input().hardware(1);

        assert_eq!(
            Hardware {
                cpus: 18,
                interfaces: 4,
                disks: 4,
                fans: 2,
                temperatures: 3,
                gpu_states: 2,
                core_channels: 18,
                battery: true,
                memory_pressure: Some(0),
                load: true,
                system: true,
            },
            hardware
        );
    }

    #[test]
    fn keeps_each_interface_and_disk_under_one_identity() {
        // Goal: the four interfaces and the four disks each keep a distinct identity of their own
        // through the whole input. Method: collect every identity the saved minutes give them.
        let identities = input().device_identities();

        assert_eq!(
            (4, 4),
            (identities.interfaces.len(), identities.disks.len())
        );
    }

    #[test]
    fn reads_each_gauge_as_exactly_zero_in_the_zero_second() {
        // Goal: second 300 reads each gauge the contract names as exactly zero, where the second
        // before it reads the gauge's baseline. The ring keeps a CPU or GPU temperature of 0 C for
        // a sensor the SMC does not list, as `collector::smc::usable_celsius` says, so those two
        // zeros read absent. Method: read the gauges at seconds 299 and 300.
        let gauges = [299, 300].map(|second| input().gauges(second));

        assert_eq!(
            [
                Gauges {
                    gpu_active_ns: 100_000_000,
                    smc: smc_baseline(),
                    memory_used_bytes: 8 * GIB,
                },
                Gauges {
                    gpu_active_ns: 0,
                    smc: Smc {
                        power_watts: Some(0.0),
                        fan_rpm: vec![0.0, 0.0],
                        temperatures_celsius: [Some(0.0), None, None],
                    },
                    memory_used_bytes: 0,
                },
            ],
            gauges
        );
    }

    #[test]
    fn reads_no_smc_gauge_in_the_second_the_smc_is_unavailable() {
        // Goal: second 302's unavailable source, the SMC, leaves its gauges absent rather than
        // zero, and only for that second. Method: read the SMC's gauges at seconds 302 and 303.
        let smc = [302, 303].map(|second| input().smc(second));

        assert_eq!(
            [
                Smc {
                    power_watts: None,
                    fan_rpm: Vec::new(),
                    temperatures_celsius: [None; 3],
                },
                smc_baseline(),
            ],
            smc
        );
    }

    #[test]
    fn passes_through_fair_and_serious_thermal_states_from_nominal_and_back() {
        // Goal: seconds 303 to 306 read the contract's thermal transitions, nominal, fair, serious
        // and nominal, which macOS publishes as the pressure levels nominal, moderate and heavy
        // and the charts name Nominal, Fair and Serious. Method: read each second's level.
        let levels = [303, 304, 305, 306].map(|second| input().thermal_pressure(second));

        assert_eq!([Some(0), Some(1), Some(2), Some(0)], levels);
    }

    #[test]
    fn rotates_two_creations_each_second_each_exit_before_its_successors_start() {
        // Goal: two processes a second take a new creation, from process 2 on, each publishing
        // its old creation's exit before its new creation's start, and the new creations count
        // from 257 in rotation order. Method: read the events seconds 1 and 2 publish.
        let events = [1, 2].map(|second| input().events(second));

        assert_eq!(
            [
                vec![
                    exit(41_002, 3),
                    start(41_002, 257),
                    exit(41_003, 4),
                    start(41_003, 258),
                ],
                vec![
                    exit(41_004, 5),
                    start(41_004, 259),
                    exit(41_005, 6),
                    start(41_005, 260),
                ],
            ],
            events
        );
    }

    #[test]
    fn carries_the_missed_ticks_rotations_in_the_tick_after_it() {
        // Goal: the processes that rotate in the missed second 301 still exit and start, and the
        // tick after it, 302, publishes their events before its own. Method: read the events of
        // second 302, where processes 94 and 95 rotated in 301 and 96 and 97 in 302, each pair
        // 127 seconds after its previous rotation, in 174 and 175.
        let events = input().events(302);

        assert_eq!(
            vec![
                exit(41_094, 603),
                start(41_094, 857),
                exit(41_095, 604),
                start(41_095, 858),
                exit(41_096, 605),
                start(41_096, 859),
                exit(41_097, 606),
                start(41_097, 860),
            ],
            events
        );
    }

    #[test]
    fn lists_a_rotated_process_under_its_new_creation() {
        // Goal: the process rows follow a rotation to the new creation. Method: read process 2's
        // creation at second 0 and at second 1, where it rotates.
        let creations = [0, 1].map(|second| input().process_creation(41_002, second));

        assert_eq!([3, 257], creations);
    }

    #[test]
    fn starts_each_new_creation_at_a_base_of_its_own() {
        // Goal: a new creation's counters start from its own base, never from its predecessor's,
        // its CPU time at 2^53 + 1,000 ns for each creation before it and the rest at zero, and
        // grow at its process's pace. Method: read process 2's counters at second 1, where its
        // creation 257 starts, and at second 2.
        let counters = [1, 2].map(|second| input().process_counters(41_002, second));

        assert_eq!(
            [
                ProcessCounters {
                    cpu_ns: (1 << 53) + 256_000,
                    energy_nj: 0,
                    disk_read_bytes: 0,
                    disk_written_bytes: 0,
                },
                ProcessCounters {
                    cpu_ns: (1 << 53) + 256_000 + 3_000_000,
                    energy_nj: 3_000,
                    disk_read_bytes: 12_288,
                    disk_written_bytes: 6_144,
                },
            ],
            counters
        );
    }

    #[test]
    fn ends_each_old_creation_at_its_counters_in_the_second_it_exits() {
        // Goal: an exit's endpoint is the old creation's counters in the second it exits, so its
        // App loses no growth to the rotation. Method: read the usage process 2's exit publishes
        // in second 1, one valid second after the creation's reading at second 0.
        let endpoint = input().exit_endpoint(41_002, 1);

        assert_eq!(
            ProcessCounters {
                cpu_ns: (1 << 53) + 2_000 + 3_000_000,
                energy_nj: 3_000,
                disk_read_bytes: 12_288,
                disk_written_bytes: 6_144,
            },
            endpoint
        );
    }

    #[test]
    fn keeps_the_last_processs_program_name_that_is_not_utf8_in_every_row_naming_it() {
        // Goal: one program name holds a byte no UTF-8 text holds, the contract's non-UTF-8
        // identity, and each saved row naming its process keeps the exact bytes. Method: read
        // process 255's name in its process row at second 0, and in its exit and start at
        // second 127, its first rotation.
        let input = input();
        let name = b"reference-process-255..........\xff".to_vec();

        assert_eq!(
            (vec![name.clone()], vec![name.clone(), name]),
            (
                input.program_names(0, "processes", "pid = 41255"),
                input.program_names(127, "events", "pid = 41255 ORDER BY event"),
            )
        );
    }

    #[test]
    fn names_the_recording_its_run_and_its_boot_with_fixed_identities() {
        // Goal: the recording, its runs and their boot carry the generator's fixed opaque
        // identities, never random ones or this Mac's boot session, so every generation names
        // them alike. Method: read the recording's identity and the run identities its registry
        // lists.
        let input = input();
        let recording = Identity([0x9e37_79b9_7f4a_7c15, 0xbf58_476d_1ce4_e5b9]);
        let boot = Identity([0x94d0_49bb_1331_11eb, 0x2545_f491_4f6c_dd1d]);
        let runs: Vec<RunIdentity> = Registry::read(&input.directory)
            .unwrap()
            .runs
            .iter()
            .map(|run| run.identity)
            .collect();

        assert_eq!(
            (
                recording,
                vec![
                    RunIdentity {
                        recording,
                        run: Identity([0xd6e8_feb8_6659_fd93, 0xa076_1d64_78bd_642f]),
                        boot,
                    },
                    RunIdentity {
                        recording,
                        run: Identity([0xe703_7ed1_a0b4_28db, 0x8ebc_6af0_9c88_c6e3]),
                        boot,
                    },
                ],
            ),
            (
                collector::recording::read_identity(&input.directory).unwrap(),
                runs
            )
        );
    }

    #[test]
    fn splits_the_input_into_two_runs_at_its_middle_minute() {
        // Goal: the input is two consecutive runs split at half its duration on a minute
        // boundary, second 180 of its 370, so each saved minute belongs to one run. Method: read
        // the first and last second each run's saved minutes hold, the second run's last saved
        // minute ending at 359 since the minute the input ends in stays in the ring.
        let seconds = input().saved_seconds_by_run();

        assert_eq!(vec![(0, 179), (180, 359)], seconds);
    }

    #[test]
    fn numbers_each_runs_ticks_from_its_own_start() {
        // Goal: each run counts its ticks from 0 at its own start, as a collector's warm-up tick
        // is its tick 0. Method: read the run tick of the first run's last second and of the
        // second run's first two.
        let input = input();
        let ticks = [179, 180, 181]
            .map(|second| cell::<u64>(&input.saved_rows(second, "ticks", "run_tick", "TRUE"), 0));

        assert_eq!([179, 0, 1], ticks);
    }

    #[test]
    fn ends_the_first_run_in_order_and_leaves_the_second_live() {
        // Goal: the first run ends as a collector that stops in order ends it, so the worker
        // saves and retires it, while the second stays live as a running collector's does.
        // Method: read whether the registry counts each run retired.
        let input = input();
        let registry = Registry::read(&input.directory).unwrap();
        let retired: Vec<bool> = registry
            .runs
            .iter()
            .map(|run| {
                Registry::completed(&input.directory, &run.identity, &run.ring_name).unwrap()
            })
            .collect();

        assert_eq!(vec![true, false], retired);
    }

    #[test]
    fn publishes_no_exit_or_start_in_the_second_runs_first_tick() {
        // Goal: the second run's first tick reports no exit or start, as a collector's first tick
        // knows no process before it, and lists the creations the pair rotating in that second
        // started. Method: read second 180's events, and process 106's creation at 179 and 180,
        // 361 from its rotation at 53 and 615 from its rotation at 180.
        let input = input();

        assert_eq!(
            (Vec::<SavedEvent>::new(), [361, 615]),
            (
                input.events(180),
                [179, 180].map(|second| input.process_creation(41_106, second))
            )
        );
    }

    /// The SMC's readings between the contract's special seconds.
    fn smc_baseline() -> Smc {
        Smc {
            power_watts: Some(5.0),
            fan_rpm: vec![1_200.0, 1_300.0],
            temperatures_celsius: [Some(35.0), Some(36.0), Some(37.0)],
        }
    }

    /// Process 0's counters after `valid` valid seconds, `incidents` of them incident seconds,
    /// as the contract gives them for process index 0.
    fn process_zero_counters(valid: u64, incidents: u64) -> ProcessCounters {
        ProcessCounters {
            cpu_ns: (1 << 53) + 1_000_000 * (valid + 19 * incidents),
            energy_nj: 1_000 * (valid + 19 * incidents),
            disk_read_bytes: 4_096 * valid,
            disk_written_bytes: 2_048 * valid,
        }
    }

    fn exit(pid: i32, creation: u64) -> SavedEvent {
        SavedEvent {
            kind: persistence::schema::event_kind::EXIT,
            pid,
            creation,
        }
    }

    fn start(pid: i32, creation: u64) -> SavedEvent {
        SavedEvent {
            kind: persistence::schema::event_kind::START,
            pid,
            creation,
        }
    }

    /// Core 0's ticks after `valid` valid seconds and `invalid` others.
    fn core_zero_ticks(valid: u32, invalid: u32) -> CPUTicks {
        CPUTicks {
            user: 10 * valid,
            system: 5 * valid,
            nice: 0,
            idle: 85 * valid + 100 * invalid,
        }
    }

    /// The input every test reads, generated once. Its rings are unlinked as soon as it is
    /// generated, since no test reads them and a ring outlives this process otherwise.
    fn input() -> &'static Input {
        static INPUT: OnceLock<Input> = OnceLock::new();
        INPUT.get_or_init(Input::generate)
    }

    struct Input {
        directory: PathBuf,
    }

    impl Input {
        fn generate() -> Self {
            let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reference_input");
            // A run that failed left its recording, which the generator refuses to add to.
            match std::fs::remove_dir_all(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{error}"),
            }

            let generated = generator()
                .arg("--directory")
                .arg(&directory)
                .args(["--seconds", &SECONDS.to_string()])
                .status()
                .unwrap();
            let unlinked = generator()
                .arg("--unlink")
                .arg(&directory)
                .status()
                .unwrap();

            assert!(generated.success());
            assert!(unlinked.success());
            Self { directory }
        }

        /// The counters a process's saved row holds at `second` of the input.
        fn process_counters(&self, pid: i32, second: u64) -> ProcessCounters {
            process_counters_of(&self.saved_row(
                second,
                "processes",
                PROCESS_COUNTERS,
                &format!("pid = {pid}"),
            ))
        }

        /// The creation of the process `pid` at `second` of the input.
        fn process_creation(&self, pid: i32, second: u64) -> u64 {
            cell(
                &self.saved_row(
                    second,
                    "processes",
                    "process_unique_id",
                    &format!("pid = {pid}"),
                ),
                0,
            )
        }

        /// The program names of the rows of `table` that `condition` picks at `second` of the
        /// input.
        fn program_names(&self, second: u64, table: &str, condition: &str) -> Vec<Vec<u8>> {
            let rows = self.saved_rows(second, table, "executable_name", condition);

            (0..rows.height())
                .map(|row| {
                    persistence::engine::Row::new(&rows, row)
                        .unwrap()
                        .get(0)
                        .unwrap()
                })
                .collect()
        }

        /// The exits and starts `second` of the input publishes, in the order it publishes them.
        fn events(&self, second: u64) -> Vec<SavedEvent> {
            let rows = self.saved_rows(
                second,
                "events",
                "kind, pid, process_unique_id",
                "TRUE ORDER BY event",
            );

            (0..rows.height())
                .map(|row| {
                    let row = persistence::engine::Row::new(&rows, row).unwrap();
                    SavedEvent {
                        kind: row.get(0).unwrap(),
                        pid: row.get(1).unwrap(),
                        creation: row.get(2).unwrap(),
                    }
                })
                .collect()
        }

        /// The counters the exit of the process `pid` that `second` of the input publishes end at.
        fn exit_endpoint(&self, pid: i32, second: u64) -> ProcessCounters {
            process_counters_of(&self.saved_row(
                second,
                "events",
                PROCESS_COUNTERS,
                &format!(
                    "pid = {pid} AND kind = {}",
                    persistence::schema::event_kind::EXIT
                ),
            ))
        }

        /// How far the counters of the process `pid` grew over `second` of the input.
        fn process_growth(&self, pid: i32, second: u64) -> ProcessCounters {
            let before = self.process_counters(pid, second - 1);
            let after = self.process_counters(pid, second);

            ProcessCounters {
                cpu_ns: after.cpu_ns - before.cpu_ns,
                energy_nj: after.energy_nj - before.energy_nj,
                disk_read_bytes: after.disk_read_bytes - before.disk_read_bytes,
                disk_written_bytes: after.disk_written_bytes - before.disk_written_bytes,
            }
        }

        /// The ticks a CPU's saved row holds at `second` of the input.
        fn cpu_ticks(&self, cpu: u32, second: u64) -> CPUTicks {
            let row = self.saved_row(
                second,
                "cpus",
                "user_ticks, system_ticks, nice_ticks, idle_ticks",
                &format!("cpu = {cpu}"),
            );

            CPUTicks {
                user: cell(&row, 0),
                system: cell(&row, 1),
                nice: cell(&row, 2),
                idle: cell(&row, 3),
            }
        }

        /// What a second's saved tick holds of each device and field the contract lists.
        fn hardware(&self, second: u64) -> Hardware {
            let count = |table: &str| {
                u32::try_from(self.saved_rows(second, table, "1", "TRUE").height()).unwrap()
            };
            let ticks = self.saved_row(second, "ticks", "*", "TRUE");
            let temperatures = ["soc_celsius", "cpu_celsius", "gpu_celsius"]
                .iter()
                .filter(|name| field::<f32>(&ticks, name).is_some())
                .count();
            let memory = self.saved_row(second, "memory", "pressure", "TRUE");

            Hardware {
                cpus: count("cpus"),
                interfaces: count("interfaces"),
                disks: u32::try_from(disks(&ticks, 0).count).unwrap(),
                fans: count("fans"),
                temperatures: u32::try_from(temperatures).unwrap(),
                gpu_states: count("gpu_states"),
                core_channels: u32::try_from(
                    self.saved_rows(second, "core_states", "DISTINCT core", "TRUE")
                        .height(),
                )
                .unwrap(),
                battery: field::<u64>(&ticks, "battery_charge_current").is_some(),
                memory_pressure: cell(&memory, 0),
                load: field::<f64>(&ticks, "load_average_1").is_some(),
                system: field::<u32>(&ticks, "machine_cpu_cores").is_some(),
            }
        }

        /// The thermal pressure level macOS published at `second`.
        fn thermal_pressure(&self, second: u64) -> Option<u8> {
            field(
                &self.saved_row(second, "ticks", "thermal_pressure", "TRUE"),
                "thermal_pressure",
            )
        }

        /// The SMC's readings, as `second` of the input reads them.
        fn smc(&self, second: u64) -> Smc {
            let ticks = self.saved_row(
                second,
                "ticks",
                "power_watts, soc_celsius, cpu_celsius, gpu_celsius",
                "TRUE",
            );
            let fans = self.saved_rows(second, "fans", "rpm", "TRUE ORDER BY fan");

            Smc {
                power_watts: cell(&ticks, 0),
                fan_rpm: (0..fans.height())
                    .map(|row| {
                        persistence::engine::Row::new(&fans, row)
                            .unwrap()
                            .get(0)
                            .unwrap()
                    })
                    .collect(),
                temperatures_celsius: [1, 2, 3].map(|column| {
                    persistence::engine::Row::new(&ticks, 0)
                        .unwrap()
                        .get(column)
                        .unwrap()
                }),
            }
        }

        /// The gauges the contract names, as `second` of the input reads them.
        fn gauges(&self, second: u64) -> Gauges {
            let gpu_active_ns = |second| -> u64 {
                cell(
                    &self.saved_rows(second, "gpu_states", "SUM(active_ns)", "TRUE"),
                    0,
                )
            };
            let memory = self.saved_row(
                second,
                "memory",
                "anonymous_bytes - purgeable_bytes + wired_bytes + compressed_bytes",
                "TRUE",
            );

            Gauges {
                gpu_active_ns: gpu_active_ns(second) - gpu_active_ns(second - 1),
                smc: self.smc(second),
                memory_used_bytes: cell(&memory, 0),
            }
        }

        /// Every identity the saved minutes give an interface or a disk.
        fn device_identities(&self) -> DeviceIdentities {
            let mut identities = DeviceIdentities::default();
            for minute in self.saved_minutes() {
                let data = self.directory.join(&minute.data_directory);
                let interfaces = query(
                    &data.join("interfaces.parquet"),
                    "SELECT DISTINCT interface, name FROM saved",
                );
                for index in 0..interfaces.height() {
                    let row = persistence::engine::Row::new(&interfaces, index).unwrap();
                    identities
                        .interfaces
                        .insert((row.get(0).unwrap(), row.get(1).unwrap()));
                }

                let ticks = query(&data.join("ticks.parquet"), "SELECT * FROM saved");
                for index in 0..ticks.height() {
                    let snapshot = disks(&ticks, index);
                    for disk in &snapshot.records[..snapshot.count] {
                        identities.disks.insert((disk.driver_id, disk.media_id));
                    }
                }
            }

            identities
        }

        /// The one row of `table` that `condition` picks at `second` of the input, as `columns`.
        fn saved_row(
            &self,
            second: u64,
            table: &str,
            columns: &str,
            condition: &str,
        ) -> polars::prelude::DataFrame {
            let rows = self.saved_rows(second, table, columns, condition);

            assert_eq!(1, rows.height());
            rows
        }

        /// The rows of `table` that `condition` picks at `second` of the input, as `columns`.
        fn saved_rows(
            &self,
            second: u64,
            table: &str,
            columns: &str,
            condition: &str,
        ) -> polars::prelude::DataFrame {
            let tick = self
                .saved_tick(second)
                .unwrap_or_else(|| panic!("no saved minute holds second {second}"));
            // A minute keeps its events in an indexed table of parts rather than in one file.
            let saved = if table == "events" {
                persistence::event_table::EventTable::open(&self.directory, &tick.minute)
                    .unwrap()
                    .into_lazy(&Arc::new(ByteBudget::new(EVENT_QUERY_BYTES)))
                    .unwrap()
            } else {
                let data = self.directory.join(&tick.minute.data_directory);
                persistence::engine::scan(&data.join(format!("{table}.parquet"))).unwrap()
            };

            query_frame(
                saved,
                &format!(
                    "SELECT {columns} FROM saved WHERE publication = {} AND {condition}",
                    tick.publication
                ),
            )
        }

        /// The saved tick of `second` of the input, where a saved minute holds one.
        fn saved_tick(&self, second: u64) -> Option<SavedTick> {
            let wall_ns = ORIGIN_WALL_NS + second * NANOSECONDS_PER_SECOND;
            let mut found = None;
            for minute in self.saved_minutes() {
                let publications = query(
                    &self
                        .directory
                        .join(&minute.data_directory)
                        .join("ticks.parquet"),
                    &format!("SELECT publication FROM saved WHERE wall_ns = {wall_ns}"),
                );
                if publications.height() == 1 {
                    // A second is saved once, since the input's runs share no second.
                    assert!(found.is_none());
                    found = Some(SavedTick {
                        minute,
                        publication: cell(&publications, 0),
                    });
                }
            }

            found
        }

        /// The first and last second each run's saved minutes hold, in the registry's order.
        fn saved_seconds_by_run(&self) -> Vec<(u64, u64)> {
            let registry = Registry::read(&self.directory).unwrap();

            registry
                .runs
                .iter()
                .map(|run| {
                    let walls: Vec<(u64, u64)> =
                        saved_windows::saved_manifests(&self.directory, &run.identity)
                            .iter()
                            .map(|minute| {
                                let walls = query(
                                    &self
                                        .directory
                                        .join(&minute.data_directory)
                                        .join("ticks.parquet"),
                                    "SELECT MIN(wall_ns), MAX(wall_ns) FROM saved",
                                );
                                (cell(&walls, 0), cell(&walls, 1))
                            })
                            .collect();
                    let second = |wall_ns: u64| (wall_ns - ORIGIN_WALL_NS) / NANOSECONDS_PER_SECOND;

                    (
                        second(walls.iter().map(|walls| walls.0).min().unwrap()),
                        second(walls.iter().map(|walls| walls.1).max().unwrap()),
                    )
                })
                .collect()
        }

        /// The manifest of each saved minute of every run.
        fn saved_minutes(&self) -> Vec<Manifest> {
            let registry = Registry::read(&self.directory).unwrap();

            registry
                .runs
                .iter()
                .flat_map(|run| saved_windows::saved_manifests(&self.directory, &run.identity))
                .collect()
        }
    }

    /// What a second holds of each device and field the contract lists: a count of each device,
    /// and whether each field is there.
    #[derive(Debug, PartialEq)]
    struct Hardware {
        cpus: u32,
        interfaces: u32,
        disks: u32,
        fans: u32,
        temperatures: u32,
        gpu_states: u32,
        core_channels: u32,
        battery: bool,
        memory_pressure: Option<u8>,
        load: bool,
        system: bool,
    }

    /// The gauges the contract names: the GPU's active time over the second before, the SMC's
    /// gauges, and the machine memory, as the memory chart adds it up.
    #[derive(Clone, Debug, PartialEq)]
    struct Gauges {
        gpu_active_ns: u64,
        smc: Smc,
        memory_used_bytes: u64,
    }

    /// The gauges the SMC reads: the power, the fans, and the chip's, the CPU's and the GPU's
    /// temperatures.
    #[derive(Clone, Debug, PartialEq)]
    struct Smc {
        power_watts: Option<f32>,
        fan_rpm: Vec<f32>,
        temperatures_celsius: [Option<f32>; 3],
    }

    /// Each interface's index and name, and each disk's driver and media identities.
    #[derive(Default)]
    struct DeviceIdentities {
        interfaces: BTreeSet<(u16, String)>,
        disks: BTreeSet<(u64, u64)>,
    }

    /// A process's counters, its CPU time the sum of its user and system time.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct ProcessCounters {
        cpu_ns: u64,
        energy_nj: u64,
        disk_read_bytes: u64,
        disk_written_bytes: u64,
    }

    /// An exit or a start an input's second publishes, with the creation it names.
    #[derive(Debug, PartialEq)]
    struct SavedEvent {
        kind: u8,
        pid: i32,
        creation: u64,
    }

    /// A tick a saved minute holds: the minute and the tick's publication in its tables.
    struct SavedTick {
        minute: Manifest,
        publication: u64,
    }

    /// The bytes a query of one saved minute's events may hold, ample for the input's few
    /// hundred events a minute.
    const EVENT_QUERY_BYTES: usize = 1 << 28;

    /// The columns of a process or event row that `process_counters_of` reads.
    const PROCESS_COUNTERS: &str =
        "user_ns + system_ns, energy_nj, disk_read_bytes, disk_written_bytes";

    /// The counters of the one row of `row`, which holds the columns `PROCESS_COUNTERS` names.
    fn process_counters_of(row: &polars::prelude::DataFrame) -> ProcessCounters {
        ProcessCounters {
            cpu_ns: cell(row, 0),
            energy_nj: cell(row, 1),
            disk_read_bytes: cell(row, 2),
            disk_written_bytes: cell(row, 3),
        }
    }

    fn generator() -> Command {
        Command::new(env!("CARGO_BIN_EXE_reference_input"))
    }

    fn query(path: &Path, sql: &str) -> polars::prelude::DataFrame {
        query_frame(persistence::engine::scan(path).unwrap(), sql)
    }

    /// The rows `sql` selects from `saved`, a saved table.
    fn query_frame(saved: polars::prelude::LazyFrame, sql: &str) -> polars::prelude::DataFrame {
        let engine = persistence::engine::Engine::new();
        engine.register_lazy("saved", saved).unwrap();

        engine.query(sql).unwrap()
    }

    /// The value of the column `name` in the first row of `frame`, `None` where the column is
    /// absent, as it is from a saved minute in which no tick read its family, or empty.
    fn field<T: persistence::engine::FromCell>(
        frame: &polars::prelude::DataFrame,
        name: &str,
    ) -> Option<T> {
        let column = frame.get_column_index(name)?;

        persistence::engine::Row::new(frame, 0)
            .unwrap()
            .get::<Option<T>>(column)
            .unwrap()
    }

    /// The disks the tick in row `row` of a saved ticks table read, none where it read none.
    fn disks(ticks: &polars::prelude::DataFrame, row: usize) -> DiskSnapshot {
        let mut snapshot = DiskSnapshot::default();
        let Some(column) = ticks.get_column_index("physical_disks") else {
            return snapshot;
        };
        let text = persistence::engine::Row::new(ticks, row)
            .unwrap()
            .get::<Option<String>>(column)
            .unwrap();
        if let Some(text) = text {
            persistence::disks::decode(&text, &mut snapshot).unwrap();
        }

        snapshot
    }

    fn cell<T: persistence::engine::FromCell>(
        frame: &polars::prelude::DataFrame,
        column: usize,
    ) -> T {
        persistence::engine::Row::new(frame, 0)
            .unwrap()
            .get(column)
            .unwrap()
    }
}
