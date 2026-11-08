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
        let cpu_ns = input().cpu_ns(PROCESS_ZERO_PID, 1);

        assert_eq!((1 << 53) + 1_000_000, cpu_ns);
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

        /// The CPU time a process's saved row holds at `second` of the input.
        fn cpu_ns(&self, pid: i32, second: u64) -> u64 {
            let wall_ns = ORIGIN_WALL_NS + second * NANOSECONDS_PER_SECOND;
            let registry = Registry::read(&self.directory).unwrap();
            for run in &registry.runs {
                for manifest in saved_windows::saved_manifests(&self.directory, &run.identity) {
                    let data = self.directory.join(&manifest.data_directory);
                    let publications = query(
                        &data.join("ticks.parquet"),
                        &format!("SELECT publication FROM saved WHERE wall_ns = {wall_ns}"),
                    );
                    if publications.height() == 1 {
                        let publication: u64 = cell(&publications, 0);
                        let processes = query(
                            &data.join("processes.parquet"),
                            &format!(
                                "SELECT user_ns + system_ns FROM saved \
                                WHERE publication = {publication} AND pid = {pid}"
                            ),
                        );
                        assert_eq!(1, processes.height());
                        return cell(&processes, 0);
                    }
                }
            }

            panic!("no saved minute holds second {second}");
        }
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
