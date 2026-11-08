//! The reference input generator, run as a user runs it, and the readings it leaves read back from
//! the recording's saved minutes.

#[cfg(test)]
mod saved_windows;

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;

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

        /// The one row of `table` that `condition` picks at `second` of the input, as `columns`.
        fn saved_row(
            &self,
            second: u64,
            table: &str,
            columns: &str,
            condition: &str,
        ) -> polars::prelude::DataFrame {
            let tick = self
                .saved_tick(second)
                .unwrap_or_else(|| panic!("no saved minute holds second {second}"));
            let rows = query(
                &tick.data.join(format!("{table}.parquet")),
                &format!(
                    "SELECT {columns} FROM saved WHERE publication = {} AND {condition}",
                    tick.publication
                ),
            );

            assert_eq!(1, rows.height());
            rows
        }

        /// The saved tick of `second` of the input, where a saved minute holds one.
        fn saved_tick(&self, second: u64) -> Option<SavedTick> {
            let wall_ns = ORIGIN_WALL_NS + second * NANOSECONDS_PER_SECOND;
            let registry = Registry::read(&self.directory).unwrap();
            let mut found = None;
            for run in &registry.runs {
                for manifest in saved_windows::saved_manifests(&self.directory, &run.identity) {
                    let data = self.directory.join(&manifest.data_directory);
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
            }

            found
        }
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
