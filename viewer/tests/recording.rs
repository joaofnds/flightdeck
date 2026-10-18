#[cfg(test)]
#[path = "../../persistence/tests/encoder_fixture/mod.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::fixture;
    use collector::processes::Unreadable;
    use collector::ring::{Identity, RunIdentity};
    use collector::tracker::{Event, Noticed};
    use persistence::{
        Begin, Encoder, Limits, NumberedEvent, Observation, Store, Window, WindowIdentity,
    };
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use viewer::Session;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Recording(PathBuf);

    struct CapturedIdentity {
        version: u8,
        refused_pid: Option<i32>,
    }

    impl Default for CapturedIdentity {
        fn default() -> Self {
            Self {
                version: 1,
                refused_pid: None,
            }
        }
    }

    impl CapturedIdentity {
        fn write(&self, directory: &Path) {
            let config = duckdb::Config::default().threads(1).unwrap();
            let connection = duckdb::Connection::open_in_memory_with_flags(config).unwrap();
            let source =
                persistence::query::parquet_literal(&directory.join("processes.parquet")).unwrap();
            let target =
                persistence::query::parquet_literal(&directory.join("captured.parquet")).unwrap();
            let version = self.version;
            connection
                .execute_batch(&format!(
                    r"CREATE TABLE captured AS SELECT *,
                {version}::UTINYINT identity_version,0::UTINYINT identity_availability,
                0::UTINYINT usage_availability,(CASE WHEN pid=0 THEN
                1000000000+awake_ns_at_start ELSE 10000+pid END)::UBIGINT process_unique_id,
                4::INTEGER pid_version,'worker\xFF'::BLOB executable_name,
                '/Applications/Engine.app'::BLOB app_path FROM read_parquet('{source}')"
                ))
                .unwrap();
            if let Some(pid) = self.refused_pid {
                let fields = persistence::schema::Table::Processes
                    .columns()
                    .skip(2)
                    .map(|field| format!("{field}=NULL"))
                    .collect::<Vec<_>>()
                    .join(",");
                connection
                    .execute_batch(&format!(
                        "UPDATE captured SET usage_availability=1,{fields} WHERE pid={pid}"
                    ))
                    .unwrap();
            }
            connection
                .execute_batch(&format!(
                    "COPY captured TO '{target}' (FORMAT PARQUET,COMPRESSION ZSTD)"
                ))
                .unwrap();
            std::fs::rename(
                directory.join("captured.parquet"),
                directory.join("processes.parquet"),
            )
            .unwrap();
        }
    }

    impl Recording {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            Self(
                std::env::temp_dir()
                    .join(format!("flightdeck-viewer-{}-{serial}", std::process::id())),
            )
        }

        fn write(&self) {
            self.write_processes(40);
        }

        fn write_processes(&self, processes: u64) {
            self.write_processes_with(processes, |_| {});
        }

        fn write_processes_with(&self, processes: u64, extend: impl FnOnce(&Path)) {
            let store = Store::open(&self.0).unwrap();
            let mut shape = fixture::profile(false);
            shape.process_capacity = processes;
            let identity = WindowIdentity {
                run: RunIdentity {
                    recording: store.recording(),
                    run: Identity([3, 4]),
                    boot: Identity([5, 6]),
                },
                window: 0,
            };
            let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap());
            Self::write_ticks(&mut window, &shape);
            let Begin::Pending(pending) = store.begin(&identity).unwrap() else {
                panic!("new fixture")
            };
            let tables = Encoder.encode(&window, pending.path()).unwrap();
            extend(pending.path());
            let manifest = persistence::Manifest {
                identity,
                publication_first: 1,
                publication_end: 5,
                events_first: 0,
                events_end: 20,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
                losses: persistence::Losses::default(),
                data_directory: String::new(),
                tables,
            };
            store.commit(pending, manifest).unwrap();
        }

        fn write_ticks(window: &mut Window, shape: &collector::ring::Shape) {
            let events = [
                Event::Exit {
                    pid: 42,
                    last: Ok(fixture::usage(100)),
                    noticed: Noticed::OnExit,
                },
                Event::Exit {
                    pid: 43,
                    last: Err(Unreadable::Gone),
                    noticed: Noticed::AlreadyExited,
                },
                Event::Exit {
                    pid: 44,
                    last: Err(Unreadable::Refused),
                    noticed: Noticed::OnExit,
                },
                Event::Start {
                    pid: 45,
                    coalition: Some(99),
                },
                Event::Exec { pid: 45 },
            ];
            for (index, second) in [Some(1000), Some(1001), None, Some(1003)]
                .into_iter()
                .enumerate()
            {
                let number = u64::try_from(index).unwrap();
                let mut tick = fixture::tick(shape);
                tick.run_tick = number;
                tick.events = number * 5..number * 5 + 5;
                tick.processes[0].usage.user_ns = 9_007_199_254_740_993;
                tick.processes[0].usage.awake_ns_at_start = 1 + number * 1_000_000;
                tick.power_watts = 0.0;
                tick.gpu_energy_nj = 10_000_000_000 + number * 4_000_000_000;
                for interface in &mut tick.interfaces {
                    interface.received_bytes += number * 2048;
                    interface.sent_bytes += number * 1024;
                }
                tick.smc_read = index != 3;
                for cpu in &mut tick.cpus {
                    cpu.idle = 100 + u32::try_from(index).unwrap() * 60;
                    cpu.user = 100 + u32::try_from(index).unwrap() * 40;
                }
                let numbered: Vec<_> = events
                    .iter()
                    .enumerate()
                    .map(|(offset, event)| NumberedEvent {
                        number: number * 5 + u64::try_from(offset).unwrap(),
                        tick: number,
                        event,
                    })
                    .collect();
                window
                    .push_tick(
                        &Observation {
                            publication: number + 1,
                            wall_ns: second.map(|s| s * 1_000_000_000),
                            continuous_ns: Some((number + 1) * 2_000_000_000),
                            awake_ns: Some((number + 1) * 1_000_000_000),
                        },
                        &tick,
                        &numbered,
                    )
                    .unwrap();
            }
        }

        fn standard_run(&self, run: [u64; 2]) {
            self.standard_run_with(run, |_| {});
        }

        fn standard_run_with(&self, run: [u64; 2], extend: impl FnOnce(&Path)) {
            let store = Store::open(&self.0).unwrap();
            let mut window = fixture::window(false);
            window.identity.run.recording = store.recording();
            window.identity.run.run = Identity(run);
            let identity = window.identity;
            let Begin::Pending(pending) = store.begin(&identity).unwrap() else {
                panic!("new run");
            };
            let tables = Encoder.encode(&window, pending.path()).unwrap();
            extend(pending.path());
            let manifest = persistence::Manifest {
                identity,
                publication_first: 1,
                publication_end: 62,
                events_first: 0,
                events_end: 3904,
                wall_ns: Some(persistence::TimeBounds {
                    min: 1_000_000_000_000,
                    max: 1_060_000_000_000,
                }),
                continuous_ns: None,
                awake_ns: None,
                losses: persistence::Losses::default(),
                data_directory: String::new(),
                tables,
            };
            store.commit(pending, manifest).unwrap();
        }
    }

    #[test]
    fn keeps_readable_identity_when_saved_process_usage_was_refused() {
        // Goal: a readable process remains identifiable without fabricated numeric readings.
        // Method: inspect a committed identity-only row through the production viewer page.
        let recording = Recording::new();
        let identity = CapturedIdentity {
            refused_pid: Some(39),
            ..CapturedIdentity::default()
        };
        recording.write_processes_with(40, |path| identity.write(path));
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1001, 0).unwrap();

        let page = session.page("processes", 0).unwrap();

        assert!(
            page["rows"][39].as_array().unwrap()[2..21]
                .iter()
                .all(serde_json::Value::is_null)
        );
        assert_eq!(
            &page["rows"][39].as_array().unwrap()[21..],
            serde_json::json!([
                "1",
                "0",
                "1",
                "10039",
                "4",
                "worker\\xff",
                "/Applications/Engine.app"
            ])
            .as_array()
            .unwrap()
        );
    }

    #[test]
    fn excludes_identity_only_presence_from_measured_usage_queries() {
        // Goal: the numeric historical query excludes presence without a measured payload.
        // Method: query a committed process with readable identity but refused usage.
        let recording = Recording::new();
        let identity = CapturedIdentity {
            refused_pid: Some(39),
            ..CapturedIdentity::default()
        };
        recording.standard_run_with([3, 4], |path| identity.write(path));

        let result = persistence::query::processes_at_second(&recording.0, 1001).unwrap();

        assert_eq!(result.processes.len(), 103);
        assert!(!result.processes.iter().any(|(pid, _)| *pid == 39));
        assert!(result.processes.contains(&(0, 2)));
        assert!(result.processes.contains(&(30_000, 2)));
    }

    #[test]
    fn keeps_captured_identity_with_the_process_beyond_the_first_page() {
        // Goal: byte identity and numeric readings stay attached to the same paged process.
        // Method: open a captured 600-process fixture and inspect its final saved process.
        let recording = Recording::new();
        recording.write_processes_with(600, |path| CapturedIdentity::default().write(path));
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1001, 0).unwrap();

        let page = session.page("processes", 512).unwrap();

        assert_eq!(page["total"], 600);
        assert_eq!(page["rows"].as_array().unwrap().len(), 88);
        assert_eq!(page["rows"][87][3], "59902");
        assert_eq!(
            &page["rows"][87].as_array().unwrap()[21..],
            serde_json::json!([
                "1",
                "0",
                "0",
                "10599",
                "4",
                "worker\\xff",
                "/Applications/Engine.app"
            ])
            .as_array()
            .unwrap()
        );
    }

    #[test]
    fn refuses_unknown_captured_identity_before_returning_a_saved_page() {
        // Goal: the production viewer cannot bypass unsupported identity-version rejection.
        // Method: open a real committed window with otherwise valid version-nine metadata.
        let recording = Recording::new();
        let identity = CapturedIdentity {
            version: 9,
            ..CapturedIdentity::default()
        };
        recording.write_processes_with(40, |path| identity.write(path));
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1001, 0).unwrap();

        let result = session.page("processes", 0);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn normalizes_legacy_recordings_to_explicit_unsupported_identity() {
        // Goal: the actual saved viewer binds a stable identity schema for old recordings.
        // Method: select a real legacy window and inspect its process page metadata/payload.
        let recording = Recording::new();
        recording.write();
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1001, 0).unwrap();

        let page = session.page("processes", 0).unwrap();

        assert_eq!(
            &page["columns"].as_array().unwrap()[21..],
            serde_json::json!([
                "identity_version",
                "identity_availability",
                "usage_availability",
                "process_unique_id",
                "pid_version",
                "executable_name",
                "app_path"
            ])
            .as_array()
            .unwrap()
        );
        assert_eq!(
            &page["rows"][0].as_array().unwrap()[21..],
            serde_json::json!([null, "3", "0", null, null, null, null])
                .as_array()
                .unwrap()
        );
    }

    #[test]
    fn inspects_legacy_and_captured_processes_in_one_recording() {
        // Goal: mixed format history keeps metadata availability with its selected run.
        // Method: reopen one recording with legacy and augmented runs at the same second.
        let recording = Recording::new();
        recording.standard_run([3, 4]);
        recording.standard_run_with([7, 8], |path| CapturedIdentity::default().write(path));
        let mut session = Session::open(&recording.0).unwrap();

        let legacy = session.select(1001, 0).unwrap();
        let legacy_page = session.page("processes", 0).unwrap();
        let captured = session.select(1001, 1).unwrap();
        let captured_page = session.page("processes", 0).unwrap();

        assert_ne!(legacy["selected"]["run"], captured["selected"]["run"]);
        assert_eq!(legacy_page["rows"][1][3], "102");
        assert_eq!(captured_page["rows"][1][3], "102");
        assert_eq!(legacy_page["rows"][1][22], "3");
        assert!(legacy_page["rows"][1][26].is_null());
        assert_eq!(captured_page["rows"][1][22], "0");
        assert_eq!(captured_page["rows"][1][26], "worker\\xff");
    }

    #[test]
    fn preserves_distinct_runs_at_the_same_second_and_refuses_a_stale_page() {
        // Goal: wall-clock collisions never merge publications from different collector runs.
        // Method: commit identical source clocks in two runs, inspect both, then refuse an index.
        let recording = Recording::new();
        recording.standard_run([3, 4]);
        recording.standard_run([7, 8]);
        let mut session = Session::open(&recording.0).unwrap();

        let first = session.select(1001, 0).unwrap();
        let second = session.select(1001, 1).unwrap();
        let page = session.page("processes", 0).unwrap();

        assert_eq!(first["alternatives"].as_array().unwrap().len(), 2);
        assert_eq!(second["trends"].as_array().unwrap().len(), 4);
        assert_ne!(first["selected"]["run"], second["selected"]["run"]);
        assert_eq!(page["observation"]["run"], second["selected"]["run"]);
        assert_eq!(page["total"], 40);
        assert!(session.select(1001, 2).is_err());
        assert!(session.page("processes", 0).is_err());
    }

    #[test]
    fn refuses_a_corrupt_committed_table_without_repairing_it() {
        // Goal: corrupt history is a read failure, never a measured empty selection.
        // Method: damage the committed ticks file in place and observe a nonmutating refusal.
        let recording = Recording::new();
        recording.write();
        let ticks = recording
            .0
            .join(
                persistence::layout::WindowKey {
                    run: Identity([3, 4]),
                    window: 0,
                }
                .ready_relative(None),
            )
            .join("ticks.parquet");
        std::fs::write(&ticks, b"damaged").unwrap();

        assert!(Session::open(&recording.0).is_err());
        assert_eq!(std::fs::read(&ticks).unwrap(), b"damaged");
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            if self.0.exists() {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    #[test]
    fn opens_overview_and_inspects_the_same_saved_second() {
        // Goal: native presentation receives the exact saved tick across overview and inspection.
        // Method: real private Parquet files with a missing clock, exits and an exact wide integer.
        let recording = Recording::new();
        recording.write();
        let mut session = Session::open(&recording.0).unwrap();

        let overview = session.select(1001, 0).unwrap();
        let processes = session.page("processes", 0).unwrap();
        let events = session.page("events", 0).unwrap();

        assert_eq!(session.catalog()["last_second"], 1003);
        assert_eq!(session.catalog()["unknown_clock_ticks"], 1);
        assert_eq!(overview["selected"]["publication"], 2);
        assert_eq!(overview["summary"]["cpu_percent"], 40.0);
        assert_eq!(overview["summary"]["power_watts"], 0.0);
        assert_eq!(
            processes["rows"][0],
            serde_json::json!([
                "2",
                "0",
                "1000001",
                "9007199254740993",
                "3",
                "4",
                "5",
                "6",
                "7",
                "8",
                "9",
                "10",
                "11",
                "12",
                "13",
                "14",
                "-1",
                "2",
                "3",
                "2147483647",
                "-2147483648",
                null,
                "3",
                "0",
                null,
                null,
                null,
                null
            ])
        );
        assert_eq!(events["rows"][1][6], "2");
        assert_eq!(events["rows"][1][9], serde_json::Value::Null);
        let gap = session.select(1002, 0).unwrap();
        assert!(gap["selected"].is_null());
        assert!(session.page("processes", 0).is_err());
        let unavailable = session.select(1003, 0).unwrap();
        assert!(unavailable["summary"]["power_watts"].is_null());
        assert!(unavailable["summary"]["cpu_percent"].is_null());
    }

    #[test]
    fn counter_rates_use_the_measured_continuous_interval() {
        // Goal: displayed traffic and GPU power use real elapsed time, not a guessed second.
        // Method: source counters advance over two continuous seconds but one wall second.
        let recording = Recording::new();
        recording.write();
        let mut session = Session::open(&recording.0).unwrap();

        let overview = session.select(1001, 0).unwrap();

        assert_eq!(overview["summary"]["network_rx_bps"], 2048.0);
        assert_eq!(overview["summary"]["network_tx_bps"], 1024.0);
        assert_eq!(overview["summary"]["gpu_watts"], 2.0);
    }

    #[test]
    fn retains_event_only_usage_and_unknown_creation_evidence_with_its_tick() {
        // Goal: exits between sweeps and unavailable lifecycle evidence never become PID joins.
        // Method: inspect measured, gone, refused, start and exec rows at publication two.
        let recording = Recording::new();
        recording.write();
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1001, 0).unwrap();
        let events = session.page("events", 0).unwrap();

        assert_eq!(events["total"], 5);
        assert_eq!(
            events["rows"][0],
            serde_json::json!([
                "2", "5", "1", "42", "3", null, "0", "0", "101", "102", "103", "104", "105", "106",
                "107", "108", "109", "110", "111", "112", "113", "114", null, "3", "0", null, null,
                null, null
            ])
        );
        assert_eq!(events["rows"][1][6], "2");
        assert_eq!(events["rows"][2][6], "1");
        assert_eq!(events["rows"][3][4], "1");
        assert_eq!(events["rows"][3][5], "99");
        assert_eq!(events["rows"][4][4], "2");
        for row in &events["rows"].as_array().unwrap()[1..] {
            assert!(row[8].is_null());
            assert!(row[9].is_null());
        }
    }

    #[test]
    fn machine_pages_keep_core_memory_swap_and_interface_source_values() {
        // Goal: machine inspection preserves independent values at the process publication.
        // Method: compare exact source fields from the private real-file fixture.
        let recording = Recording::new();
        recording.write();
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1001, 0).unwrap();

        let cpus = session.page("cpus", 0).unwrap();
        let memory = session.page("memory", 0).unwrap();
        let interfaces = session.page("interfaces", 0).unwrap();

        assert_eq!(
            cpus["rows"],
            serde_json::json!([
                ["2", "0", "140", "2", "3", "160"],
                ["2", "1", "140", "2", "3", "160"]
            ])
        );
        assert_eq!(memory["rows"][0][1], "1");
        assert_eq!(memory["rows"][0][12], "12");
        assert_eq!(memory["rows"][0][19], "1");
        assert_eq!(
            interfaces["rows"],
            serde_json::json!([
                [
                    "2", "0", "fixture0", "2049", "1026", "3", "4", "5", "6", "7"
                ],
                [
                    "2", "1", "fixture0", "2050", "1027", "4", "5", "6", "7", "8"
                ]
            ])
        );
    }
    #[test]
    fn a_reused_pid_retains_each_observations_creation_identity() {
        // Goal: historical PID reuse never assigns an earlier creation to a later observation.
        // Method: one PID has distinct awake start times in consecutive stored publications.
        let recording = Recording::new();
        recording.write();
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1000, 0).unwrap();
        let before = session.page("processes", 0).unwrap();
        session.select(1001, 0).unwrap();
        let after = session.page("processes", 0).unwrap();

        assert_eq!(before["rows"][0][1], "0");
        assert_eq!(after["rows"][0][1], "0");
        assert_eq!(before["rows"][0][2], "1");
        assert_eq!(after["rows"][0][2], "1000001");
    }

    #[test]
    fn process_pages_cover_every_saved_row_without_dropping_overflow() {
        // Goal: process inspection reaches every row beyond its bounded first page.
        // Method: encode 600 processes and compare both page boundaries and explicit end refusal.
        let recording = Recording::new();
        recording.write_processes(600);
        let mut session = Session::open(&recording.0).unwrap();
        session.select(1001, 0).unwrap();

        let first = session.page("processes", 0).unwrap();
        let final_page = session.page("processes", 512).unwrap();

        assert_eq!(first["total"], 600);
        assert_eq!(first["rows"].as_array().unwrap().len(), 512);
        assert_eq!(final_page["rows"].as_array().unwrap().len(), 88);
        assert_eq!(first["rows"][511][1], "511");
        assert_eq!(final_page["rows"][0][1], "512");
        assert_eq!(final_page["rows"][87][1], "599");
        assert!(session.page("processes", 1024).is_err());
    }

    #[test]
    fn contextual_derivation_can_read_the_exact_predecessor_outside_the_range() {
        // Goal: the first contextual rate uses its real predecessor, never a guessed zero.
        // Method: missing second 1900 puts 1001 first in its 900-second context, excluding 1000.
        let recording = Recording::new();
        recording.write();
        let mut session = Session::open(&recording.0).unwrap();

        let selection = session.select(1900, 0).unwrap();

        assert!(selection["selected"].is_null());
        assert_eq!(selection["trends"][0]["wall_ns"], 1_001_000_000_000_u64);
        assert_eq!(selection["trends"][0]["cpu_percent"], 40.0);
        assert_eq!(selection["trends"][0]["gpu_watts"], 2.0);
    }

    #[test]
    fn canceled_work_cannot_read_a_page_and_a_new_epoch_can_select_again() {
        // Goal: superseding a native request invalidates its entire remaining read path.
        // Method: cancel an actual disk session, refuse its page, then start a fresh epoch.
        let recording = Recording::new();
        recording.write();
        let control = std::sync::Arc::new(viewer::Cancellation::default());
        let mut session =
            Session::open_with(&recording.0, std::sync::Arc::clone(&control), 0).unwrap();
        session.select(1001, 0).unwrap();

        control.cancel().unwrap();

        assert!(session.page("processes", 0).is_err());
        assert!(session.begin(0).is_err());
        session.begin(control.epoch()).unwrap();
        let selection = session.select(1000, 0).unwrap();
        assert_eq!(selection["selected"]["publication"], 1);
    }
}
