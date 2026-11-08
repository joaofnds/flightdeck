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
    use std::sync::OnceLock;

    use collector::disks::DiskSnapshot;
    use collector::machine::CPUTicks;
    use collector::recording::Registry;

    use super::saved_windows;

    /// A shortened input, whose saved minutes reach past the special seconds 300 to 306, since
    /// the minute an input ends in stays in its ring.
    const SECONDS: u64 = 370;
    const ORIGIN_WALL_NS: u64 = 1_800_000_000 * NANOSECONDS_PER_SECOND;
    const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
    const PROCESS_ZERO_PID: i32 = 41_000;

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
        // which follow 299, 299 and 300 of the contract's valid seconds.
        let counters =
            [299, 300, 302].map(|second| input().process_counters(PROCESS_ZERO_PID, second));

        assert_eq!([299, 299, 300].map(process_zero_counters), counters);
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

    /// Process 0's counters after `valid` valid seconds, as the contract gives them for process
    /// index 0.
    fn process_zero_counters(valid: u64) -> ProcessCounters {
        ProcessCounters {
            cpu_ns: (1 << 53) + 1_000_000 * valid,
            energy_nj: 1_000 * valid,
            disk_read_bytes: 4_096 * valid,
            disk_written_bytes: 2_048 * valid,
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
            let row = self.saved_row(
                second,
                "processes",
                "user_ns + system_ns, energy_nj, disk_read_bytes, disk_written_bytes",
                &format!("pid = {pid}"),
            );

            ProcessCounters {
                cpu_ns: cell(&row, 0),
                energy_nj: cell(&row, 1),
                disk_read_bytes: cell(&row, 2),
                disk_written_bytes: cell(&row, 3),
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

        /// Every identity the saved minutes give an interface or a disk.
        fn device_identities(&self) -> DeviceIdentities {
            let mut identities = DeviceIdentities::default();
            for data in self.saved_minutes() {
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

            query(
                &tick.data.join(format!("{table}.parquet")),
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
            for data in self.saved_minutes() {
                let publications = query(
                    &data.join("ticks.parquet"),
                    &format!("SELECT publication FROM saved WHERE wall_ns = {wall_ns}"),
                );
                if publications.height() == 1 {
                    // A second is saved once, since the input's runs share no second.
                    assert!(found.is_none());
                    found = Some(SavedTick {
                        data,
                        publication: cell(&publications, 0),
                    });
                }
            }

            found
        }

        /// The tables of each saved minute of every run.
        fn saved_minutes(&self) -> Vec<PathBuf> {
            let registry = Registry::read(&self.directory).unwrap();
            let mut minutes = Vec::new();
            for run in &registry.runs {
                for manifest in saved_windows::saved_manifests(&self.directory, &run.identity) {
                    minutes.push(self.directory.join(&manifest.data_directory));
                }
            }

            minutes
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

    /// Each interface's index and name, and each disk's driver and media identities.
    #[derive(Default)]
    struct DeviceIdentities {
        interfaces: BTreeSet<(u16, String)>,
        disks: BTreeSet<(u64, u64)>,
    }

    /// A process's counters, its CPU time the sum of its user and system time.
    #[derive(Debug, PartialEq)]
    struct ProcessCounters {
        cpu_ns: u64,
        energy_nj: u64,
        disk_read_bytes: u64,
        disk_written_bytes: u64,
    }

    /// A tick a saved minute holds: the minute's tables and the tick's publication in them.
    struct SavedTick {
        data: PathBuf,
        publication: u64,
    }

    fn generator() -> Command {
        Command::new(env!("CARGO_BIN_EXE_reference_input"))
    }

    fn query(path: &Path, sql: &str) -> polars::prelude::DataFrame {
        let engine = persistence::engine::Engine::new();
        engine
            .register_lazy("saved", persistence::engine::scan(path).unwrap())
            .unwrap();

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
