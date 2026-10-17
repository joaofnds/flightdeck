#[cfg(test)]
#[path = "../src/live_fixture.rs"]
mod identity_fixture;
#[cfg(test)]
mod tests {
    use collector::machine::Memory;
    use collector::recording::{Registration, private};
    use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, Writer};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use viewer::LiveSession;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Recording {
        root: PathBuf,
        writer: Writer,
        elapsed: u64,
    }

    impl Recording {
        fn new() -> Self {
            Self::with_process_capacity(1)
        }

        fn identity_source() -> Self {
            let mut recording = Self::new();
            let place = Place {
                lock_path: recording.root.join("identity.lock"),
                name_prefix: format!("/fdlid{}", std::process::id()),
            };
            let writer = Writer::create_with_identity(
                &place,
                collector::ring::RingReader::open_generation(recording.writer.name())
                    .unwrap()
                    .shape(),
                collector::ring::DictionaryCapacity {
                    entries: 1,
                    arena_bytes: 32,
                },
                &recording.writer.identity().unwrap(),
                1,
            )
            .unwrap();
            collector::ring::unlink_generation(recording.writer.name()).unwrap();
            recording.writer = writer;
            let registry = collector::recording::Registry {
                runs: vec![collector::recording::RegisteredRun {
                    identity: recording.writer.identity().unwrap(),
                    ring_name: recording.writer.name().to_owned(),
                    collector_pid: 0,
                }],
                evictions: 0,
            };
            registry.write(&recording.root).unwrap();
            recording
        }

        fn with_process_capacity(process_capacity: u64) -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("flightdeck-live-{}-{serial}", std::process::id()));
            private::create_private_dir(&root).unwrap();
            let place = Place {
                lock_path: root.join("ring.lock"),
                name_prefix: format!("/fdlive{}x{serial}", std::process::id()),
            };
            let shape = Shape {
                slot_count: if process_capacity == 1 { 8 } else { 120 },
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 2,
                fan_capacity: 1,
                process_capacity,
                coalition_capacity: 1,
                event_capacity: 8,
            };
            let (writer, registered) = Registration::start(&root, &place, shape, 0)
                .unwrap()
                .into_parts();
            registered.unwrap();
            Self {
                root,
                writer,
                elapsed: 0,
            }
        }

        fn seal(&self) {
            let mut reader =
                collector::ring::RingReader::open_generation(self.writer.name()).unwrap();
            let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
            let identity = persistence::WindowIdentity {
                run: self.writer.identity().unwrap(),
                window: 0,
            };
            let mut window = persistence::Window::new(
                identity,
                persistence::Limits::from_shape(&reader.shape()).unwrap(),
            );
            for _attempt in 0..8 {
                match reader.read_into(&mut tick).unwrap() {
                    collector::ring::Found::Nothing => break,
                    collector::ring::Found::Missed { .. } => panic!("fixture source overwritten"),
                    collector::ring::Found::Publication { publication, .. } => {
                        let times = tick.times.unwrap();
                        if times.continuous_ns >= 60_000_000_000 {
                            break;
                        }
                        window
                            .push_tick(
                                &persistence::Observation {
                                    publication,
                                    wall_ns: Some(times.wall_ns),
                                    continuous_ns: Some(times.continuous_ns),
                                    awake_ns: Some(times.awake_ns),
                                },
                                &tick,
                                &[],
                            )
                            .unwrap();
                    }
                }
            }
            let store = persistence::Store::open(&self.root).unwrap();
            let persistence::Begin::Pending(pending) = store.begin(&identity).unwrap() else {
                panic!("fixture window already committed");
            };
            let tables = persistence::Encoder
                .encode(&window, pending.path())
                .unwrap();
            let publications = window.publication_range();
            let events = window.event_range();
            store
                .commit(
                    pending,
                    persistence::Manifest {
                        identity,
                        publication_first: publications.start,
                        publication_end: publications.end,
                        events_first: events.start,
                        events_end: events.end,
                        wall_ns: None,
                        continuous_ns: None,
                        awake_ns: None,
                        losses: persistence::Losses::default(),
                        data_directory: String::new(),
                        tables,
                    },
                )
                .unwrap();
        }

        fn publish_gpu(&mut self, second: u64) {
            let memory = Memory {
                total_bytes: 9_007_199_254_740_993,
                ..Memory::default()
            };
            let cpus = [collector::machine::CPUTicks {
                user: 1,
                system: 2,
                nice: 3,
                idle: 4,
            }];
            let active_ns = [
                1_000_000_000 + self.elapsed * 500_000_000,
                2_000_000_000 + self.elapsed * 300_000_000,
            ];
            self.writer.publish_observed(
                &Readings {
                    run_tick: self.elapsed,
                    cpus: &cpus,
                    memory: &memory,
                    interfaces: &[],
                    ioreport: Some(collector::ring::IOReportReadings {
                        gpu_energy_nj: 1_000_000_000 + self.elapsed * 4_000_000_000,
                        gpu_active_ns: &active_ns,
                        cores: &[],
                    }),
                    smc: None,
                    processes: &[],
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                &ObservationTimes {
                    wall_ns: second * 1_000_000_000,
                    continuous_ns: (self.elapsed * 2 + 1) * 1_000_000_000,
                    awake_ns: self.elapsed * 1_000_000_000,
                },
            );
            self.elapsed += 1;
        }

        fn publish(&mut self, second: u64) {
            self.publish_events(second, &[]);
        }

        fn publish_events(&mut self, second: u64, events: &[collector::tracker::Event]) {
            self.publish_processes(second, events, &[]);
        }

        fn publish_processes(
            &mut self,
            second: u64,
            events: &[collector::tracker::Event],
            processes: &[collector::processes::Process],
        ) {
            let memory = Memory {
                total_bytes: 9_007_199_254_740_993 + self.elapsed,
                ..Memory::default()
            };
            let cpus = [collector::machine::CPUTicks {
                user: 1,
                system: 2,
                nice: 3,
                idle: 4,
            }];
            self.writer.publish_observed(
                &Readings {
                    run_tick: self.elapsed,
                    cpus: &cpus,
                    memory: &memory,
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes,
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events,
                    dropped: Dropped::default(),
                },
                &ObservationTimes {
                    wall_ns: second * 1_000_000_000,
                    continuous_ns: self.elapsed * 1_000_000_000,
                    awake_ns: self.elapsed * 1_000_000_000,
                },
            );
            self.elapsed += 1;
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            let name = self.writer.name().to_owned();
            collector::ring::unlink_generation(&name).unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn admits_an_empty_identity_source_for_preserving_projection() {
        // Goal: empty v4 rings are admitted by the identity-preserving live projector.
        // Method: open a registered identity source before it publishes any observations.
        let recording = Recording::identity_source();

        let result = LiveSession::open(&recording.root);

        assert!(result.is_ok());
    }

    #[test]
    fn reads_unsealed_seconds_and_preserves_exact_cells() {
        // Goal: the ring's unsealed observation has the historical table contract.
        // Method: publish into an owned recording and inspect it through the public adapter.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();

        let selected = session.select(1000, 0).unwrap();
        let page = session.page("memory", 0).unwrap();

        assert_eq!(selected["selected"]["publication"], 1);
        assert_eq!(
            selected["summary"]["memory_total_bytes"],
            9_007_199_254_740_993_u64
        );
        assert_eq!(page["rows"][0][1], "9007199254740993");
        assert_eq!(session.catalog()["last_second"], 1000);
    }

    #[test]
    fn polls_new_ticks_and_retains_an_inspectable_gap() {
        // Goal: polling adds new observations without inventing a missing second.
        // Method: publish separated clocks and inspect their missing middle second.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();
        assert!(!session.poll().unwrap());
        recording.publish(1002);

        assert!(session.poll().unwrap());
        let gap = session.select(1001, 0).unwrap();
        let latest = session.select(1002, 0).unwrap();

        assert!(gap["selected"].is_null());
        assert_eq!(latest["selected"]["publication"], 2);
        assert_eq!(latest["trends"].as_array().unwrap().len(), 2);
        assert_eq!(latest["summary"]["segment"], 2);
    }
    #[test]
    fn keeps_repeated_wall_seconds_as_distinct_observations() {
        // Goal: a repeated wall second preserves both publications and their identities.
        // Method: publish the same fake wall clock twice and inspect both alternatives.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();

        let first = session.select(1000, 0).unwrap();
        let second = session.select(1000, 1).unwrap();

        assert_eq!(first["alternatives"].as_array().unwrap().len(), 2);
        assert_eq!(first["selected"]["publication"], 1);
        assert_eq!(second["selected"]["publication"], 2);
        assert_ne!(first["summary"]["segment"], second["summary"]["segment"]);
    }

    #[test]
    fn reports_overwritten_publications_and_completion() {
        // Goal: an overwritten source and a stopped collector remain visible as such.
        // Method: overfill the real eight-slot ring after opening, then mark it finished.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();
        for second in 1001..1010 {
            recording.publish(second);
        }
        recording.writer.finish();

        assert!(session.poll().unwrap());
        let latest = session.select(1009, 0).unwrap();

        assert_eq!(latest["selected"]["reader_losses"]["publications"], 1);
        assert!(latest["selected"]["window_losses"].is_null());
        assert_eq!(
            session.catalog()["reader_losses"][0]["losses"]["publications"],
            1
        );
        assert_eq!(session.catalog()["collector_completed"], true);
        assert!(!session.poll().unwrap());
    }
    #[test]
    fn merges_the_committed_boundary_with_the_unsealed_tail_once() {
        // Goal: one publication retains every memory cell when its source becomes history.
        // Method: inspect before sealing, seal while open, evict its raw tick, then reread it.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1001);
        let mut session = LiveSession::open(&recording.root).unwrap();
        session.select(1000, 0).unwrap();
        let unsealed_page = session.page("memory", 0).unwrap();
        session.select(1001, 0).unwrap();
        let neighbor_page = session.page("memory", 0).unwrap();
        assert_ne!(unsealed_page["rows"], neighbor_page["rows"]);
        recording.seal();
        session.poll().unwrap();
        recording.publish(2001);
        session.poll().unwrap();
        assert_eq!(session.catalog()["tail_ticks"], 1);

        let selected = session.select(1000, 0).unwrap();
        let sealed_page = session.page("memory", 0).unwrap();

        assert_eq!(selected["alternatives"].as_array().unwrap().len(), 1);
        assert_eq!(unsealed_page["columns"], sealed_page["columns"]);
        assert_eq!(unsealed_page["rows"], sealed_page["rows"]);
        assert_eq!(unsealed_page["total"], sealed_page["total"]);
        assert_eq!(unsealed_page["offset"], sealed_page["offset"]);
        assert_eq!(unsealed_page["name"], sealed_page["name"]);
        assert_eq!(page_identity(&unsealed_page), page_identity(&sealed_page));
        assert!(unsealed_page["observation"]["window_losses"].is_null());
        assert!(sealed_page["observation"]["window_losses"].is_object());
        assert_eq!(session.catalog()["windows"], 1);
    }

    #[test]
    fn retains_typed_event_availability_and_exact_coalition_identity() {
        // Goal: live events use the persistence codes and preserve absent exit usage.
        // Method: publish a start and refused exit into the real event log and page them.
        let mut recording = Recording::new();
        recording.publish_events(
            1000,
            &[
                collector::tracker::Event::Start {
                    pid: 42,
                    coalition: Some(9_007_199_254_740_993),
                },
                collector::tracker::Event::Exit {
                    pid: 42,
                    last: Err(collector::processes::Unreadable::Refused),
                    noticed: collector::tracker::Noticed::AlreadyExited,
                },
            ],
        );
        let mut session = LiveSession::open(&recording.root).unwrap();
        session.select(1000, 0).unwrap();

        let page = session.page("events", 0).unwrap();

        assert_eq!(page["rows"][0][5], "9007199254740993");
        assert_eq!(page["rows"][1][4], "3");
        assert_eq!(page["rows"][1][6], "1");
        assert_eq!(page["rows"][1][7], "1");
        assert!(page["rows"][1][8].is_null());
    }
    #[test]
    fn identifies_the_latest_publication_after_a_wall_clock_rollback() {
        // Goal: following chooses the new publication even if its wall clock moved backward.
        // Method: publish two fake wall clocks in reverse order and inspect the live catalog.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();
        recording.publish(900);

        assert!(session.poll().unwrap());
        let catalog = session.catalog();

        assert_eq!(catalog["last_second"], 1000);
        assert_eq!(
            catalog["latest_observation"]["wall_ns"],
            900_000_000_000_u64
        );
        assert_eq!(catalog["latest_observation"]["publication"], 2);
    }

    #[test]
    fn retains_a_pinned_historical_page_after_a_catalog_refresh() {
        // Goal: refreshing committed history preserves an inspected historical observation.
        // Method: seal one tick, pin it, then advance beyond its raw cache and refresh history.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.seal();
        let mut session = LiveSession::open(&recording.root).unwrap();
        session.select(1000, 0).unwrap();
        for second in 3000..3060 {
            recording.publish(second);
            session.poll().unwrap();
        }

        let page = session.page("memory", 0).unwrap();

        assert_eq!(page["observation"]["publication"], 1);
        assert_eq!(page["rows"][0][1], "9007199254740993");
    }
    #[test]
    fn discovers_a_delayed_seal_without_waiting_for_another_tick() {
        // Goal: the persister's delayed minute commitment appears on the next live poll.
        // Method: cross a real source window, poll before sealing, then seal and poll again.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();
        recording.elapsed = 120;
        recording.publish(1120);
        session.poll().unwrap();
        assert_eq!(session.catalog()["windows"], 0);
        recording.seal();

        assert!(!session.poll().unwrap());

        assert_eq!(session.catalog()["windows"], 1);
    }
    #[test]
    fn joins_consecutive_points_after_an_earlier_historical_gap() {
        // Goal: independent history and tail segment counters cannot split a valid interval.
        // Method: preserve an early committed gap, overwrite its first ring tick, and merge.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1002);
        recording.seal();
        for second in 1003..=1009 {
            recording.publish(second);
        }
        let mut session = LiveSession::open(&recording.root).unwrap();

        let response = session.select(1003, 0).unwrap();
        let points = response["trends"].as_array().unwrap();

        assert_ne!(points[0]["segment"], points[1]["segment"]);
        assert_eq!(points[1]["segment"], points[2]["segment"]);
        assert_eq!(response["summary"]["segment"], points[2]["segment"]);
    }
    #[test]
    fn rehydrates_an_evicted_unsealed_tick_from_its_retained_ring_publication() {
        // Goal: raw cache pressure cannot hide an unsealed tick still held by the ring.
        // Method: fill 120 slots with 12,000 processes, exceed 128 MiB, then inspect tick 1.
        let mut recording = Recording::with_process_capacity(12_000);
        let processes: Vec<_> = (1..=12_000).map(capacity_process).collect();
        recording.publish_processes(1000, &[], &processes);
        let mut session = LiveSession::open(&recording.root).unwrap();
        let before = session.select(1000, 0).unwrap();
        let first_page = session.page("processes", 0).unwrap();
        let last_page = session.page("processes", 11_776).unwrap();
        let mut publish_and_poll_ns_max = 0;
        for second in 1001..=1030 {
            let start = std::time::Instant::now();
            recording.publish_processes(second, &[], &processes);
            assert!(session.poll().unwrap());
            publish_and_poll_ns_max = publish_and_poll_ns_max.max(start.elapsed().as_nanos());
        }
        assert!(session.catalog()["tail_ticks"].as_u64().unwrap() < 31);
        let start = std::time::Instant::now();
        let selected = session.select(1000, 0).unwrap();
        let rehydration_ns = start.elapsed().as_nanos();
        assert_eq!(selected["selected"], before["selected"]);
        assert_eq!(session.page("processes", 0).unwrap(), first_page);
        assert_eq!(session.page("processes", 11_776).unwrap(), last_page);
        assert_eq!(session.catalog()["latest_observation"]["publication"], 31);
        recording.publish_processes(1031, &[], &processes);
        assert!(session.poll().unwrap());
        assert_eq!(session.page("processes", 0).unwrap(), first_page);
        assert_eq!(session.catalog()["latest_observation"]["publication"], 32);
        assert!(session.catalog()["tail_cache_bytes"].as_u64().unwrap() <= 128 * 1024 * 1024);
        assert!(!session.poll().unwrap());
        println!(
            "{}",
            serde_json::json!({"processes_per_tick": 12000, "publications": 32,
            "rehydration_ns": rehydration_ns, "publish_and_poll_ns_max": publish_and_poll_ns_max,
            "retained_cache_bytes": session.catalog()["tail_cache_bytes"],
            "cache_bytes_max": session.catalog()["tail_cache_bytes_max"]})
        );
    }

    fn capacity_process(pid: i32) -> collector::processes::Process {
        collector::processes::Process {
            pid,
            usage: collector::processes::Usage {
                awake_ns_at_start: u64::try_from(pid).unwrap(),
                user_ns: 0,
                system_ns: 0,
                energy_nj: 0,
                instructions: 0,
                super_instructions: 0,
                cycles: 0,
                idle_wakeups: 0,
                interrupt_wakeups: 0,
                pageins: 0,
                resident_bytes: 0,
                footprint_bytes: 0,
                disk_read_bytes: 0,
                disk_written_bytes: 0,
            },
            task: collector::processes::Task {
                threads: 1,
                faults: 0,
                context_switches: 0,
                mach_syscalls: 0,
                unix_syscalls: 0,
            },
        }
    }

    #[test]
    fn preserves_the_scalar_point_shape_across_a_real_ring_seal() {
        // Goal: public live point fields remain exactly equal after committing the same ticks.
        // Method: publish positive GPU counters through the real ring, then seal and reopen
        // history.
        let mut recording = Recording::new();
        recording.publish_gpu(1000);
        recording.publish_gpu(1001);
        let mut live = LiveSession::open(&recording.root).unwrap();
        let before = live.select(1001, 0).unwrap();
        assert_eq!(before["summary"]["gpu_active_percent"], 40.0);
        assert_eq!(before["summary"]["gpu_watts"], 2.0);
        assert_eq!(
            live.page("gpu_states", 0).unwrap()["rows"],
            serde_json::json!([["2", "0", "1500000000"], ["2", "1", "2300000000"]])
        );
        recording.seal();
        let mut saved = viewer::Session::open(&recording.root).unwrap();

        let after = saved.select(1001, 0).unwrap();

        assert_eq!(before["summary"], after["summary"]);
        assert_eq!(before["trends"], after["trends"]);
        assert!(before["summary"].get("observation").is_none());
        assert_eq!(live.catalog()["latest_observation"]["publication"], 2);
        assert_eq!(
            live.page("memory", 0).unwrap()["observation"]["publication"],
            2
        );
    }

    fn page_identity(page: &serde_json::Value) -> [&serde_json::Value; 5] {
        let observation = &page["observation"];
        [
            &observation["run"],
            &observation["boot"],
            &observation["publication"],
            &observation["run_tick"],
            &observation["wall_ns"],
        ]
    }

    fn assert_reader_losses_absent(catalog: &serde_json::Value, sources: usize) {
        let losses = catalog["reader_losses"].as_array().unwrap();
        assert_eq!(losses.len(), sources);
        assert!(sources <= 2);
        for source in losses.iter().take(2) {
            assert_eq!(
                source["losses"],
                serde_json::json!({"publications": 0, "events": 0, "unknown": false})
            );
        }
    }
    #[test]
    fn preserves_captured_process_and_independent_event_metadata_while_polling() {
        // Goal: live pages preserve captured identity separately from unavailable usage.
        // Method: poll two real v4 publications and inspect their normalized original/raw cells.
        let mut fixture = super::identity_fixture::Fixture::new();
        fixture.publish(1000);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.select(1000, 0).unwrap();
        let first = session.page("processes", 0).unwrap();
        let events = session.page("events", 0).unwrap();

        fixture.publish(1001);
        assert!(session.poll().unwrap());
        session.select(1001, 0).unwrap();
        let next = session.page("processes", 0).unwrap();

        assert_eq!(first["rows"].as_array().unwrap().len(), 2);
        assert_eq!(first["rows"][0][1], "1");
        assert!(
            first["rows"][0].as_array().unwrap()[2..21]
                .iter()
                .all(serde_json::Value::is_null)
        );
        assert_eq!(first["rows"][0][21], "1");
        assert_eq!(first["rows"][0][22], "0");
        assert_eq!(first["rows"][0][23], "1");
        assert_eq!(first["rows"][0][24], "1");
        assert_eq!(first["rows"][0][25], "-1");
        assert_eq!(first["rows"][0][26], r"refused\xff");
        assert_eq!(first["rows"][0][27], r"/Apps/Outer\xfe.app");
        assert_eq!(first["rows"][1][3], "9007199254740993");
        assert_eq!(next["rows"][1][3], "9007199254740994");
        assert_eq!(events["rows"][0][25], "700");
        assert_eq!(events["rows"][0][26], "-70");
        assert_eq!(events["rows"][0][27], r"departed\xfd");
        assert_eq!(events["rows"][0][28], "/Old.app");
        session.select(1000, 0).unwrap();
        assert_eq!(session.page("processes", 0).unwrap(), first);
    }

    #[test]
    fn preserves_identity_pages_once_across_live_and_sealed_overlap() {
        // Goal: sealing and raw-row eviction do not duplicate or alter identity observations.
        // Method: inspect live pages, seal, evict old rows by wall time, then select history.
        let mut fixture = super::identity_fixture::Fixture::new();
        fixture.publish(1000);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.select(1000, 0).unwrap();
        let processes = session.page("processes", 0).unwrap();
        let events = session.page("events", 0).unwrap();
        fixture.seal();
        session.poll().unwrap();
        fixture.publish(2001);
        session.poll().unwrap();

        let selected = session.select(1000, 0).unwrap();
        let saved_processes = session.page("processes", 0).unwrap();
        let saved_events = session.page("events", 0).unwrap();

        assert_eq!(selected["alternatives"].as_array().unwrap().len(), 1);
        assert_eq!(processes["columns"], saved_processes["columns"]);
        assert_eq!(processes["rows"], saved_processes["rows"]);
        assert_eq!(events["columns"], saved_events["columns"]);
        assert_eq!(events["rows"], saved_events["rows"]);
        assert_eq!(session.catalog()["windows"], 1);
        assert_eq!(session.catalog()["tail_ticks"], 1);
    }

    #[test]
    fn keeps_legacy_identity_absent_and_measured_usage_exact() {
        // Goal: legacy live rows retain unsupported identity without changing measured values.
        // Method: query the same process fixture through an ordinary registered v3 writer.
        let mut fixture = super::identity_fixture::Fixture::new_mode(false, 2, None);
        fixture.publish(1000);
        let mut session = LiveSession::open(&fixture.root).unwrap();

        session.select(1000, 0).unwrap();
        let page = session.page("processes", 0).unwrap();

        assert_eq!(page["rows"].as_array().unwrap().len(), 1);
        assert_eq!(page["rows"][0][3], "9007199254740993");
        assert!(page["rows"][0][21].is_null());
        assert_eq!(page["rows"][0][22], "3");
        assert_eq!(page["rows"][0][23], "0");
        assert!(page["rows"][0][26].is_null());
        assert!(page["rows"][0][27].is_null());
    }

    #[test]
    fn rehydrates_captured_identity_after_real_eviction_without_disturbing_polling() {
        // Goal: inspecting evicted v4 rows preserves metadata and subsequent polling.
        // Method: exceed the raw cache while all publications remain in a 64-slot ring.
        let mut fixture = super::identity_fixture::Fixture::new_mode(true, 12_000, None);
        fixture.publish(1000);
        let control = std::sync::Arc::new(viewer::Cancellation::default());
        let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
        let before = session.select(1000, 0).unwrap();
        let first = session.page("processes", 0).unwrap();
        let last = session.page("processes", 11_776).unwrap();
        let events = session.page("events", 0).unwrap();
        assert_eq!(first["rows"][0][26], r"refused\xff");
        assert!(first["rows"][0][3].is_null());
        assert_eq!(last["rows"].as_array().unwrap().last().unwrap()[1], "12000");
        assert_eq!(events["rows"][0][25], "700");
        assert_eq!(events["rows"][0][27], r"departed\xfd");
        for second in 1001..=1031 {
            fixture.publish(second);
            assert!(session.poll().unwrap());
        }
        let catalog = session.catalog();
        assert!(catalog["tail_ticks"].as_u64().unwrap() < 32);
        assert_eq!(catalog["first_second"], 1000);
        assert_eq!(catalog["latest_observation"]["publication"], 32);
        assert_reader_losses_absent(&catalog, 1);

        control.cancel().unwrap();
        assert!(session.select(1000, 0).is_err());
        session.begin(control.epoch()).unwrap();
        let selected = session.select(1000, 0).unwrap();

        assert_eq!(selected["selected"], before["selected"]);
        assert_eq!(session.page("processes", 0).unwrap(), first);
        assert_eq!(session.page("processes", 11_776).unwrap(), last);
        assert_eq!(session.page("events", 0).unwrap(), events);
        assert_eq!(
            session.catalog()["latest_observation"],
            catalog["latest_observation"]
        );
        assert_eq!(session.catalog()["reader_losses"], catalog["reader_losses"]);
        assert!(!session.poll().unwrap());
        fixture.publish(1032);
        assert!(session.poll().unwrap());
        assert_eq!(session.catalog()["latest_observation"]["publication"], 33);
        assert_eq!(session.catalog()["reader_losses"], catalog["reader_losses"]);
        assert_eq!(session.page("processes", 0).unwrap(), first);
        assert_eq!(session.page("events", 0).unwrap(), events);
        session.select(1032, 0).unwrap();
        assert_eq!(
            session.page("processes", 0).unwrap()["rows"][6][3],
            "9007199254741025"
        );
        assert!(session.catalog()["tail_cache_bytes"].as_u64().unwrap() <= 128 * 1024 * 1024);
        println!(
            "{}",
            serde_json::json!({"identity_observations_per_tick":12000, "publications":33,
                "retained_tail_ticks_before_rehydration":catalog["tail_ticks"],
                "retained_cache_bytes_before_rehydration":catalog["tail_cache_bytes"]})
        );
    }

    #[test]
    fn alternates_legacy_and_captured_sources_within_one_recording() {
        // Goal: source discovery and polling preserve each source's optional identity schema.
        // Method: alternate registered v4/v3 publications through one public live session.
        let mut captured = super::identity_fixture::Fixture::new();
        let mut legacy = super::identity_fixture::Fixture::new_mode(false, 2, Some(&captured));
        let mut session = LiveSession::open(&captured.root).unwrap();

        captured.publish(1000);
        assert!(session.poll().unwrap());
        session.select(1000, 0).unwrap();
        let processes = session.page("processes", 0).unwrap();
        let events = session.page("events", 0).unwrap();
        legacy.publish(1001);
        assert!(session.poll().unwrap());
        session.select(1001, 0).unwrap();
        let original = session.page("processes", 0).unwrap();
        let original_events = session.page("events", 0).unwrap();
        captured.publish(1002);
        assert!(session.poll().unwrap());
        session.select(1002, 0).unwrap();
        let next = session.page("processes", 0).unwrap();
        legacy.publish(1003);
        assert!(session.poll().unwrap());
        session.select(1003, 0).unwrap();
        let latest = session.page("processes", 0).unwrap();

        assert_eq!(processes["rows"][0][26], r"refused\xff");
        assert!(processes["rows"][0][3].is_null());
        assert_eq!(processes["rows"][1][3], "9007199254740993");
        assert_eq!(events["rows"][0][25], "700");
        assert_eq!(events["rows"][0][27], r"departed\xfd");
        assert!(original["rows"][0][21].is_null());
        assert_eq!(original["rows"][0][22], "3");
        assert!(original["rows"][0][26].is_null());
        assert!(original_events["rows"][0][22].is_null());
        assert!(original_events["rows"][0][27].is_null());
        assert_eq!(
            next["rows"][0].as_array().unwrap()[1..],
            processes["rows"][0].as_array().unwrap()[1..]
        );
        assert_eq!(next["rows"][1][3], "9007199254740994");
        assert_eq!(latest["rows"][0][3], "9007199254740994");
        assert!(latest["rows"][0][21].is_null());
        assert_ne!(page_identity(&processes)[0], page_identity(&original)[0]);
        assert_reader_losses_absent(&session.catalog(), 2);
        assert!(!session.poll().unwrap());
        session.select(1000, 0).unwrap();
        assert_eq!(session.page("processes", 0).unwrap(), processes);
        assert_eq!(session.page("events", 0).unwrap(), events);
    }
}
