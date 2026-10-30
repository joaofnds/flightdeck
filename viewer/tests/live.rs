#[cfg(test)]
#[path = "live/apps.rs"]
mod app_checks;
#[cfg(test)]
#[path = "live/committed_apps.rs"]
mod committed_app_checks;
#[cfg(test)]
#[path = "../src/live_forged_source.rs"]
mod forged_source;
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
            self.seal_window(0);
        }

        fn seal_window(&self, window_index: u64) {
            let mut reader =
                collector::ring::RingReader::open_generation(self.writer.name()).unwrap();
            let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
            let identity = persistence::WindowIdentity {
                run: self.writer.identity().unwrap(),
                window: window_index,
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
                        if times.continuous_ns >= (window_index + 1) * 60_000_000_000 {
                            break;
                        }
                        if times.continuous_ns < window_index * 60_000_000_000 {
                            continue;
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

        fn publish_resources(&mut self, second: u64) {
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
            let mut name = [0; 16];
            name[..3].copy_from_slice(b"en0");
            let interfaces = [collector::interfaces::Interface {
                index: 7,
                name: collector::interfaces::Name::from_bytes(name, 3).unwrap(),
                received_bytes: 1000 + self.elapsed * 2048,
                sent_bytes: 2000 + self.elapsed * 1024,
                received_packets: 10 + self.elapsed * 8,
                sent_packets: 20,
                receive_errors: 1 + self.elapsed * 2,
                send_errors: 2 + self.elapsed * 4,
                receive_drops: 0,
            }];

            self.writer.publish_observed(
                &Readings {
                    run_tick: self.elapsed,
                    cpus: &cpus,
                    memory: &memory,
                    interfaces: &interfaces,
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

        fn publish_unavailable(&mut self, usage: collector::ring::UsageAvailability) {
            let memory = Memory::default();
            let observations = [collector::ring::IdentityObservation {
                pid: 42,
                usage,
                identity: collector::ring::ProcessIdentity::Unavailable {
                    creation: None,
                    reason: collector::ring::IdentityUnavailable::Unsupported,
                },
            }];
            self.writer
                .publish_with_identity(
                    &Readings {
                        run_tick: 0,
                        cpus: &[],
                        memory: &memory,
                        interfaces: &[],
                        ioreport: None,
                        smc: None,
                        processes: &[],
                        processes_refused: u32::from(
                            usage == collector::ring::UsageAvailability::Refused,
                        ),
                        processes_gone: u32::from(
                            usage == collector::ring::UsageAvailability::Gone,
                        ),
                        coalitions: &[],
                        events: &[],
                        dropped: Dropped::default(),
                    },
                    Some(&ObservationTimes {
                        wall_ns: 1_000_000_000_000,
                        continuous_ns: 1_000_000_000,
                        awake_ns: 1_000_000_000,
                    }),
                    &collector::ring::IdentityPublication {
                        processes: &observations,
                        events: &[],
                    },
                )
                .unwrap();
        }

        fn publish_without_clocks(&mut self) {
            let memory = Memory::default();
            self.writer.publish(&Readings {
                run_tick: self.elapsed,
                cpus: &[],
                memory: &memory,
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: Dropped::default(),
            });
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
            self.publish_rows(second, events, processes, &[]);
        }

        fn publish_coalition(&mut self, second: u64, coalition: &collector::coalitions::Coalition) {
            self.publish_rows(second, &[], &[], std::slice::from_ref(coalition));
        }

        fn publish_rows(
            &mut self,
            second: u64,
            events: &[collector::tracker::Event],
            processes: &[collector::processes::Process],
            coalitions: &[collector::coalitions::Coalition],
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
                    coalitions,
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

    #[test]
    fn refuses_same_second_scalar_pressure_without_losing_latest_raw_rows() {
        // Goal: same-second alternatives cannot make byte-pressure refusal destroy accepted data.
        // Method: poll bounded real publications with a tied wall clock, preserving every identity.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();
        for publication in 2_u64..=3000 {
            recording.publish(1000);
            let before = session.catalog();

            match session.poll() {
                Ok(changed) => assert!(changed),
                Err(error) => {
                    assert!(matches!(
                        error,
                        persistence::Error::Bound("live scalar cache")
                    ));
                    let after = session.catalog();
                    assert_eq!(after["tail_cache_bytes"], before["tail_cache_bytes"]);
                    assert_eq!(after["tail_ticks"], before["tail_ticks"]);
                    assert!(after["tail_cache_bytes"].as_u64().unwrap() <= 128 * 1024 * 1024);
                    let selected = session.follow_latest().unwrap();
                    assert_eq!(selected["selected"]["publication"], publication - 1);
                    let alternatives = selected["alternatives"].as_array().unwrap();
                    assert_eq!(
                        alternatives.len(),
                        usize::try_from(publication - 1).unwrap()
                    );
                    let identities: std::collections::BTreeSet<_> = alternatives
                        .iter()
                        .map(|identity| identity["publication"].as_u64().unwrap())
                        .collect();
                    assert_eq!(identities.len(), alternatives.len());
                    assert_eq!(
                        session.page("memory", 0).unwrap()["rows"][0][1],
                        (9_007_199_254_740_993 + publication - 2).to_string()
                    );
                    assert!(session.poll().is_err());
                    assert_eq!(
                        session.catalog()["tail_cache_bytes"],
                        before["tail_cache_bytes"]
                    );
                    println!(
                        "refused_publication={publication} accepted_cache_bytes={}",
                        after["tail_cache_bytes"]
                    );
                    return;
                }
            }
        }
        panic!("same-second context must exercise scalar cache refusal");
    }

    #[test]
    fn preserves_every_populated_coalition_cell_before_and_after_sealing() {
        // Goal: native coalition formatting preserves independently specified exact cells.
        // Method: publish distinct native counters, assert the whole page, seal and assert again.
        let mut recording = Recording::new();
        recording.publish_coalition(
            1000,
            &collector::coalitions::Coalition {
                id: 9_007_199_254_740_993,
                tasks_started: 101,
                tasks_exited: 17,
                cpu_ns: 9_007_199_254_740_995,
                interrupt_wakeups: 23,
                idle_wakeups: 29,
                disk_read_bytes: 9_007_199_254_740_997,
                disk_written_bytes: 9_007_199_254_740_999,
                gpu_ns: 9_007_199_254_741_001,
                energy_nj: u64::MAX,
            },
        );
        let columns = serde_json::json!([
            "publication",
            "coalition",
            "tasks_started",
            "tasks_exited",
            "cpu_ns",
            "interrupt_wakeups",
            "idle_wakeups",
            "disk_read_bytes",
            "disk_written_bytes",
            "gpu_ns",
            "energy_nj"
        ]);
        let rows = serde_json::json!([[
            "1",
            "9007199254740993",
            "101",
            "17",
            "9007199254740995",
            "23",
            "29",
            "9007199254740997",
            "9007199254740999",
            "9007199254741001",
            "18446744073709551615"
        ]]);

        let mut live = LiveSession::open(&recording.root).unwrap();
        live.follow_latest().unwrap();
        let page = live.page("coalitions", 0).unwrap();

        assert_eq!(page["columns"], columns);
        assert_eq!(page["rows"], rows);
        assert_eq!(page["total"], 1);
        recording.seal();
        let mut saved = viewer::Session::open(&recording.root).unwrap();
        saved.select(1000, 0).unwrap();
        let sealed = saved.page("coalitions", 0).unwrap();
        assert_eq!(sealed["columns"], columns);
        assert_eq!(sealed["rows"], rows);
        assert_eq!(sealed["total"], 1);
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            let name = self.writer.name().to_owned();
            collector::ring::unlink_generation(&name).unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    struct LegacyRing(Writer);

    impl Drop for LegacyRing {
        fn drop(&mut self) {
            collector::ring::unlink_generation(self.0.name()).unwrap();
        }
    }

    #[test]
    fn follows_the_current_collector_after_an_identityless_legacy_ring() {
        // Goal: unsupported old generations do not block current identified live readings.
        // Method: register a real v2 predecessor through the collector's recovery boundary.
        let mut recording = Recording::new();
        let place = Place {
            lock_path: recording.root.join("legacy.lock"),
            name_prefix: format!(
                "/fdlegacy{}x{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        };
        let reader = collector::ring::RingReader::open_generation(recording.writer.name()).unwrap();
        let legacy = LegacyRing(Writer::create(&place, reader.shape()).unwrap());
        assert!(legacy.0.identity().is_none());
        let current = recording.writer.identity().unwrap();
        let mut registry = collector::recording::Registry::default();
        registry
            .capture_previous(&recording.root, &place, current.recording)
            .unwrap();
        registry
            .add(
                &recording.root,
                collector::recording::RegisteredRun {
                    identity: current,
                    ring_name: recording.writer.name().to_owned(),
                    collector_pid: std::process::id(),
                },
            )
            .unwrap();
        registry.write(&recording.root).unwrap();
        recording.publish(1000);

        let mut session = LiveSession::open(&recording.root).unwrap();
        let response = session.follow_latest().unwrap();

        assert_eq!(response["selected"]["publication"], 1);
        assert_eq!(
            response["selected"],
            session.catalog()["latest_observation"]
        );
        assert_eq!(
            session.catalog()["reader_losses"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            session.page("memory", 0).unwrap()["observation"],
            response["selected"]
        );
    }

    #[test]
    fn identifies_the_producer_of_the_latest_observed_live_run() {
        // Goal: the app can distinguish its candidate from the source it actually follows.
        // Method: publish through a registration owned by this process and inspect its catalog.
        let mut recording = Recording::new();
        recording.publish(1000);

        let session = LiveSession::open(&recording.root).unwrap();

        assert_eq!(
            session.catalog()["latest_collector_pid"],
            std::process::id()
        );
    }

    #[test]
    fn rejects_an_identified_ring_registered_as_a_different_run() {
        // Goal: skipping an identityless predecessor never admits a mismatched current run.
        // Method: retain the actual ring but replace its registered run identity.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut registry = collector::recording::Registry::read(&recording.root).unwrap();
        registry.runs[0].identity.run.0[0] ^= 1;
        registry.write(&recording.root).unwrap();

        let result = LiveSession::open(&recording.root);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("changed live identity"))
        ));
    }

    #[test]
    fn follows_current_points_when_its_sealed_history_becomes_unreadable() {
        // Goal: following current live readings does not require saved context to stay readable.
        // Method: remove this fixture's saved data after opening history; retain current raw rows.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.seal();
        recording.publish(1001);
        let mut session = LiveSession::open(&recording.root).unwrap();
        assert_eq!(session.catalog()["windows"], 1);
        std::fs::remove_dir_all(recording.root.join("data")).unwrap();
        session.select(1000, 0).unwrap();
        assert!(session.catalog()["history_error"].is_string());

        let response = session.follow_latest().unwrap();

        assert_eq!(response["second"], 1001);
        assert_eq!(response["context_seconds"], 900);
        assert_eq!(
            response["selected"],
            session.catalog()["latest_observation"]
        );
        assert_eq!(response["selected"]["publication"], 2);
        assert_eq!(response["summary"]["publication"], 2);
        assert_eq!(response["trends"].as_array().unwrap().len(), 2);
        let page = session.page("memory", 0).unwrap();
        assert_eq!(page["observation"], response["selected"]);
        assert_eq!(page["rows"][0][1], "9007199254740994");
    }

    #[test]
    fn follows_the_newest_publication_across_wall_ties_and_rollback() {
        // Goal: latest identity follows arrival order rather than greatest wall second.
        // Method: observe a rollback with two tied publications and then inspect the older sibling.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(900);
        recording.publish(900);
        let mut session = LiveSession::open(&recording.root).unwrap();

        let response = session.follow_latest().unwrap();

        assert_eq!(response["second"], 900);
        assert_eq!(
            response["selected"],
            session.catalog()["latest_observation"]
        );
        assert_eq!(response["selected"]["publication"], 3);
        assert_eq!(response["summary"]["publication"], 3);
        assert_eq!(response["alternatives"][0]["publication"], 2);
        assert_eq!(response["alternatives"][1]["publication"], 3);
        assert_eq!(response["trends"].as_array().unwrap().len(), 2);
        assert_eq!(
            session.page("memory", 0).unwrap()["rows"][0][1],
            "9007199254740995"
        );
        let pinned = session.select(900, 0).unwrap();
        assert_eq!(pinned["selected"]["publication"], 2);
        assert_eq!(
            session.page("memory", 0).unwrap()["rows"][0][1],
            "9007199254740994"
        );
    }

    #[test]
    fn follows_only_observed_points_within_a_fixed_fifteen_minute_context() {
        // Goal: warm-up, missing seconds and the 900-second boundary invent no observations.
        // Method: observe sparse real publications through trim boundaries and inspect trends.
        let mut recording = Recording::new();
        let mut session = LiveSession::open(&recording.root).unwrap();
        assert!(session.follow_latest().is_err());
        recording.publish(1000);
        session.poll().unwrap();
        let warm = session.follow_latest().unwrap();
        assert_eq!(warm["context_seconds"], 900);
        assert_eq!(warm["trends"].as_array().unwrap().len(), 1);
        for second in [1002, 1899, 1900] {
            recording.publish(second);
            session.poll().unwrap();
        }

        let response = session.follow_latest().unwrap();

        let publications: Vec<_> = response["trends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["publication"].as_u64().unwrap())
            .collect();
        assert_eq!(publications, [2, 3, 4]);
        assert_eq!(response["context_seconds"], 900);
        assert_eq!(response["second"], 1900);
        assert_ne!(
            response["trends"][0]["segment"],
            response["trends"][1]["segment"]
        );
    }

    #[test]
    fn follows_the_exact_inclusive_time_window_independently_of_cache_trimming() {
        // Goal: the charted follow context, the 900 seconds and the lead-in before them, includes
        // its lower bound and excludes both sides. Method: a future deque front retains an older
        // outside point across a wall rollback.
        let mut recording = Recording::new();
        recording.publish(2000);
        recording.publish(940);
        recording.publish(941);
        recording.publish(1900);
        let mut session = LiveSession::open(&recording.root).unwrap();
        let catalog = session.catalog();
        assert_eq!(catalog["tail_ticks"], 4);
        assert_eq!(catalog["first_second"], 940);
        assert_eq!(catalog["last_second"], 2000);

        let response = session.follow_latest().unwrap();

        let publications: Vec<_> = response["trends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["publication"].as_u64().unwrap())
            .collect();
        assert_eq!(publications, [3, 4]);
        assert_eq!(response["context_seconds"], 900);
        assert_eq!(response["second"], 1900);
        assert_eq!(response["selected"]["publication"], 4);
        assert_eq!(response["trends"][0]["wall_ns"], 941_000_000_000_u64);
    }

    #[test]
    fn thins_the_hour_before_the_full_detail_context_to_one_reading_per_stretch() {
        // Goal: charts over 30 and 60 minutes get readings older than the full-detail seconds
        // without the viewer sending every second of them. Method: seal 70 minutes of three
        // readings each, ask for the older tail at the newest second, and read what it keeps.
        let base = 100_000;
        let mut recording = Recording::new();
        for minute in 0..70 {
            recording.elapsed = minute * 60;
            for offset in 0..3 {
                recording.publish(base + minute * 60 + offset);
            }
            recording.seal_window(minute);
        }
        let mut session = LiveSession::open(&recording.root).unwrap();
        let second = base + 69 * 60 + 2;

        let tail = session.earlier_context(second).unwrap();

        assert_eq!(
            tail["earlier_stride_seconds"],
            viewer::EARLIER_STRIDE_SECONDS
        );
        let seconds: Vec<u64> = tail["earlier_trends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["wall_ns"].as_u64().unwrap() / 1_000_000_000)
            .collect();
        let expected: Vec<u64> = (9..53).map(|minute| base + minute * 60).collect();
        assert_eq!(seconds, expected);
        assert!(seconds.iter().all(|kept| *kept >= second - 3659));
        assert!(seconds.iter().all(|kept| *kept < second - 959));
        let segments: std::collections::BTreeSet<u64> = tail["earlier_trends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["segment"].as_u64().unwrap())
            .collect();
        assert_eq!(segments.len(), expected.len());
    }

    #[test]
    fn draws_one_line_from_the_older_tail_into_the_full_detail_context_when_readings_are_unbroken()
    {
        // Goal: the tail and the context share a segment where no reading is missing between
        // them, so a chart has no notch at the join. Method: seal consecutive seconds across the
        // limit of the tail, then again with a gap at the limit, and read the tail's segment.
        let limit = 200_000;
        let newest = limit + viewer::CHARTED_SECONDS - 1 + 5;
        for gap in [false, true] {
            let mut recording = Recording::new();
            for second in limit - 4..limit {
                recording.publish(second);
            }
            if gap {
                recording.elapsed += 1;
            }
            for second in limit..limit + 4 {
                recording.publish(second);
            }
            recording.seal_window(0);
            let mut session = LiveSession::open(&recording.root).unwrap();

            let tail = session.earlier_context(newest).unwrap();

            let points = tail["earlier_trends"].as_array().unwrap();
            assert_eq!(points.len(), 1);
            assert_eq!(points[0]["wall_ns"], (limit - 4) * 1_000_000_000);
            assert_eq!(points[0]["segment"] == 1, !gap);
        }
    }

    #[test]
    fn answers_later_strides_from_the_minutes_it_kept() {
        // Goal: following the newest second does not read a sealed minute again at each stride.
        // Method: read the tail, remove the saved data, and read it for later seconds inside the
        // stride and at the next one.
        let limit = 200_000;
        let newest = limit + viewer::CHARTED_SECONDS - 1;
        let mut recording = Recording::new();
        for second in limit - 4..limit {
            recording.publish(second);
        }
        recording.seal_window(0);
        let mut session = LiveSession::open(&recording.root).unwrap();
        let tail = session.earlier_context(newest).unwrap();
        assert_eq!(tail["earlier_trends"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(recording.root.join("data")).unwrap();

        assert_eq!(session.earlier_context(newest + 9).unwrap(), tail);
        assert_eq!(session.earlier_context(newest + 10).unwrap(), tail);
    }

    #[test]
    fn gives_the_older_tail_it_keeps_only_while_it_keeps_every_minute_the_tail_needs() {
        // Goal: a page can show its readings at once with the tail already read, and leave a
        // tail it would have to read for later. Method: ask for the kept tail before reading
        // it, then for a later stride inside the minute it read, and for one that needs the
        // next minute.
        let limit = 200_000;
        let newest = limit + viewer::CHARTED_SECONDS - 1;
        let mut recording = Recording::new();
        for second in limit - 4..limit {
            recording.publish(second);
        }
        recording.seal_window(0);
        recording.elapsed = 60;
        for second in limit + 100..limit + 104 {
            recording.publish(second);
        }
        recording.seal_window(1);
        let mut session = LiveSession::open(&recording.root).unwrap();
        assert_eq!(session.earlier_context_kept(newest).unwrap(), None);

        let tail = session.earlier_context(newest).unwrap();

        assert_eq!(
            session.earlier_context_kept(newest + 10).unwrap(),
            Some(tail)
        );
        assert_eq!(session.earlier_context_kept(newest + 90).unwrap(), None);
    }

    #[test]
    fn has_no_older_tail_while_the_recording_is_younger_than_the_full_detail_context() {
        // Goal: a young recording needs no older tail. Method: ask for it with one sealed minute.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1001);
        recording.seal();
        let mut session = LiveSession::open(&recording.root).unwrap();

        let tail = session.earlier_context(1001).unwrap();

        assert_eq!(tail["earlier_trends"], serde_json::json!([]));
    }

    #[test]
    fn rejects_a_canceled_follow_and_retries_only_after_beginning_the_new_epoch() {
        // Goal: a stale follow produces neither a usable response nor an old page.
        // Method: cancel a selected session, reject its epoch, then explicitly begin and retry.
        let mut recording = Recording::new();
        recording.publish(1000);
        let control = std::sync::Arc::new(viewer::Cancellation::default());
        let mut session = LiveSession::open_with(&recording.root, control.clone(), 0).unwrap();
        session.follow_latest().unwrap();
        control.cancel().unwrap();

        assert!(session.follow_latest().is_err());
        assert!(session.page("memory", 0).is_err());
        assert!(session.begin(0).is_err());
        session.begin(control.epoch()).unwrap();
        let response = session.follow_latest().unwrap();

        assert_eq!(
            response["selected"],
            session.catalog()["latest_observation"]
        );
        assert_eq!(
            session.page("memory", 0).unwrap()["observation"],
            response["selected"]
        );
        assert!(!session.poll().unwrap());
    }

    fn follows_unavailable_usage(usage: collector::ring::UsageAvailability, code: &str) {
        let mut recording = Recording::identity_source();
        recording.publish_unavailable(usage);
        let mut session = LiveSession::open(&recording.root).unwrap();

        let response = session.follow_latest().unwrap();
        let page = session.page("processes", 0).unwrap();

        assert_eq!(
            response["selected"],
            session.catalog()["latest_observation"]
        );
        assert_eq!(response["selected"]["publication"], 1);
        assert_eq!(response["summary"]["publication"], 1);
        assert_eq!(page["observation"], response["selected"]);
        assert_eq!(page["rows"].as_array().unwrap().len(), 1);
        assert_eq!(page["rows"][0][1], "42");
        assert!(
            page["rows"][0].as_array().unwrap()[2..21]
                .iter()
                .all(serde_json::Value::is_null)
        );
        assert_eq!(page["rows"][0][23], code);
        assert_reader_losses_absent(&session.catalog(), 1);
        assert!(!session.poll().unwrap());
    }

    #[test]
    fn follows_the_latest_refused_usage_without_inventing_numeric_cells() {
        // Goal: refusal is still a current observation, with unavailable numeric cells.
        // Method: publish one real v4 refused observation and follow its public process page.
        follows_unavailable_usage(collector::ring::UsageAvailability::Refused, "1");
    }

    #[test]
    fn follows_the_latest_gone_usage_without_inventing_numeric_cells() {
        // Goal: a departed process stays distinct from refusal and measured zero usage.
        // Method: publish one real v4 gone observation and follow its public process page.
        follows_unavailable_usage(collector::ring::UsageAvailability::Gone, "2");
    }

    #[test]
    fn follows_exact_latest_rows_after_real_raw_cache_byte_pressure() {
        // Goal: following survives byte eviction without changing reader progress or losses.
        // Method: publish 12,000 process observations per tick beyond the 128 MiB cache bound,
        // the last tick without an event, since the log holds the 32 before it.
        let mut fixture = super::identity_fixture::Fixture::new_mode(true, 12_000, None);
        fixture.publish(1000);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        for second in 1001..=1031 {
            fixture.publish(second);
            assert!(session.poll().unwrap());
        }
        let before = session.catalog();
        assert!(before["tail_ticks"].as_u64().unwrap() < 32);

        let response = session.follow_latest().unwrap();
        let first = session.page("processes", 0).unwrap();
        let last = session.page("processes", 11_776).unwrap();

        assert_eq!(response["selected"], before["latest_observation"]);
        assert_eq!(response["selected"]["publication"], 32);
        assert_eq!(response["summary"]["publication"], 32);
        assert_eq!(first["observation"], response["selected"]);
        assert_eq!(last["observation"], response["selected"]);
        assert_eq!(first["rows"][0][26], r"refused\xff");
        assert!(first["rows"][0][3].is_null());
        assert_eq!(first["rows"][6][3], "9007199254741024");
        assert_eq!(last["rows"].as_array().unwrap().last().unwrap()[1], "12000");
        assert_eq!(session.catalog()["reader_losses"], before["reader_losses"]);
        assert_reader_losses_absent(&session.catalog(), 1);
        assert_eq!(
            session.catalog()["tail_cache_bytes"],
            before["tail_cache_bytes"]
        );
        assert!(before["tail_cache_bytes"].as_u64().unwrap() <= 128 * 1024 * 1024);
        assert!(!session.poll().unwrap());
        fixture.publish_with_events(1032, &[]);
        assert!(session.poll().unwrap());
        let advanced = session.follow_latest().unwrap();
        assert_eq!(advanced["selected"]["publication"], 33);
        assert_eq!(
            session.page("processes", 0).unwrap()["rows"][6][3],
            "9007199254741025"
        );
        assert_eq!(session.catalog()["reader_losses"], before["reader_losses"]);
        assert!(!session.poll().unwrap());
    }

    #[test]
    fn follows_the_last_valid_observation_when_the_source_latest_has_no_clocks() {
        // Goal: an unplaceable publication invents no live point and reports unknown loss.
        // Method: publish absent clocks through the public ring writer between observed ticks.
        let mut recording = Recording::new();
        recording.publish(1000);
        let mut session = LiveSession::open(&recording.root).unwrap();
        let previous = session.follow_latest().unwrap();
        let previous_page = session.page("memory", 0).unwrap();
        recording.publish_without_clocks();

        assert!(!session.poll().unwrap());
        let response = session.follow_latest().unwrap();

        assert_eq!(response, previous);
        assert_eq!(session.page("memory", 0).unwrap(), previous_page);
        assert_eq!(
            session.catalog()["reader_losses"][0]["losses"],
            serde_json::json!({"publications":0,"events":0,"unknown":true})
        );
        assert!(!session.poll().unwrap());
        recording.publish(1002);
        assert!(session.poll().unwrap());
        let advanced = session.follow_latest().unwrap();
        assert_eq!(advanced["selected"]["publication"], 3);
        assert_eq!(advanced["selected"]["reader_losses"]["unknown"], true);
        assert_eq!(
            session.page("memory", 0).unwrap()["rows"][0][1],
            "9007199254740995"
        );
        assert!(!session.poll().unwrap());
    }

    #[test]
    #[ignore = "opt-in actual populated live follow/select CPU and wall benchmark"]
    fn benchmarks_follow_and_manual_select_on_the_same_populated_live_session() {
        // Goal: measure actual follow and manual selection against the same cached live workload.
        // Method: seed three real sealed windows, poll 120 more points, then time 24 reads each.
        let mut recording = Recording::new();
        for minute in 0..3 {
            recording.elapsed = minute * 60;
            recording.publish(1000 + minute * 60);
            recording.publish(1001 + minute * 60);
            recording.seal_window(minute);
        }
        let mut session = LiveSession::open(&recording.root).unwrap();
        for second in 1122..1242 {
            recording.publish(second);
            session.poll().unwrap();
        }
        assert_eq!(session.catalog()["windows"], 3);
        let followed = session.follow_latest().unwrap();
        let selected = session.select(1241, 0).unwrap();
        assert_eq!(followed["selected"], selected["selected"]);
        assert_eq!(followed["trends"].as_array().unwrap().len(), 126);
        let timebase = collector::clock::Timebase::read().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();

        for follow in [false, true] {
            let before = collector::processes::read_usage(timebase, pid)
                .unwrap()
                .unwrap();
            let started = std::time::Instant::now();
            for _attempt in 0..24 {
                let response = if follow {
                    session.follow_latest()
                } else {
                    session.select(1241, 0)
                }
                .unwrap();
                assert_eq!(response["selected"], followed["selected"]);
                std::hint::black_box(response);
            }
            let elapsed_ns = started.elapsed().as_nanos();
            let after = collector::processes::read_usage(timebase, pid)
                .unwrap()
                .unwrap();
            let footprint = persistence::resources::Footprint::read().unwrap();
            println!(
                "{}",
                serde_json::json!({"follow":follow,"iterations":24,
                "sealed_windows":3,"observed_points":126,"wall_ns":elapsed_ns,
                "process_cpu_ns":after.user_ns+after.system_ns-before.user_ns-before.system_ns,
                "physical_footprint_bytes":footprint.current_bytes,
                "lifetime_footprint_max_bytes":footprint.lifetime_bytes_max,
                "live_cache_bytes":session.catalog()["tail_cache_bytes"]})
            );
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
    fn freezes_identity_pages_and_summary_before_typed_capture() {
        // Goal: native retention preserves schemas, exact counters and captured identity bytes.
        // Method: freeze every public live page and summary from the existing real-ring fixture.
        let mut fixture = super::identity_fixture::Fixture::new();
        fixture.publish_with_system_rows(1000);
        fixture.publish_with_system_rows(1001);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let response = session.follow_latest().unwrap();
        let mut summary = response["summary"].clone();
        summary.as_object_mut().unwrap().remove("run");
        summary.as_object_mut().unwrap().remove("boot");
        let mut pages = serde_json::Map::new();
        for name in persistence::schema::TABLES.map(persistence::schema::Table::name) {
            let mut page = session.page(name, 0).unwrap();
            page.as_object_mut().unwrap().remove("observation");
            pages.insert(name.to_owned(), page);
        }
        let mut derived = session.page("core_level_states", 0).unwrap();
        derived.as_object_mut().unwrap().remove("observation");
        pages.insert("core_level_states".to_owned(), derived);
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("live/item1_expected.json")).unwrap();
        assert_eq!(
            serde_json::json!({"summary":summary,"pages":pages}),
            expected
        );
    }

    #[test]
    fn observes_real_staging_frames_queries_and_table_cells_within_a_scope() {
        // Goal: removed-work guards cannot pass through a disconnected or fake-zero recorder.
        // Method: stage and seal real ticks, then select their saved summary through Session.
        let mut recording = Recording::new();
        recording.publish_resources(1000);
        recording.publish_resources(1001);
        let measured = persistence::work::observe();

        recording.seal();
        let mut session = viewer::Session::open(&recording.root).unwrap();
        session.select(1001, 0).unwrap();
        let work = measured.counts();

        assert_eq!(work.staged_ticks, 2);
        assert!(work.frames > 0);
        assert!(work.sql_plans > 0);
        assert!(work.collections > 0);
        assert!(work.string_cells > 0);
    }

    #[test]
    fn ingests_populated_machine_readings_without_staging_queries_or_string_cells() {
        // Goal: normal metric ingestion does no persistence or text-table work.
        // Method: publish two resource ticks into a real ring and inspect the selected summary.
        let mut recording = Recording::new();
        recording.publish_resources(1000);
        recording.publish_resources(1001);
        let measured = persistence::work::observe();

        let mut session = LiveSession::open(&recording.root).unwrap();
        let response = session.follow_latest().unwrap();
        let work = measured.counts();

        assert_eq!(response["summary"]["network_rx_bps"], 1024.0);
        assert_eq!(response["summary"]["gpu_active_percent"], 40.0);
        assert_eq!(
            response["summary"]["memory_total_bytes"],
            9_007_199_254_740_993_u64
        );
        println!("live_ingestion_work={work:?}");
        assert_eq!(work, persistence::work::Counts::default());
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
    fn holds_the_seconds_after_a_saved_selection_in_its_context() {
        // Goal: a selection's context holds seconds on both sides of it, so a step to a later
        // second shows readings the context already holds.
        // Method: seal seconds up to 451 past 1000 and one later in the ring, select 1000,
        // then read the context's last second and the publications its trends hold.
        let mut recording = Recording::new();
        for second in [1000, 1449, 1450, 1451] {
            recording.publish(second);
        }
        recording.seal();
        recording.publish(1500);
        let mut session = LiveSession::open(&recording.root).unwrap();

        let selected = session.select(1000, 0).unwrap();

        let publications: Vec<_> = selected["trends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["publication"].as_u64().unwrap())
            .collect();
        assert_eq!(publications, [1, 2, 3]);
        assert_eq!(selected["context_last_second"], 1450);
        assert_eq!(selected["second"], 1000);
        assert_eq!(selected["selected"]["publication"], 1);
        assert_eq!(selected["summary"]["publication"], 1);
    }

    #[test]
    fn ends_a_selections_context_at_the_newest_second() {
        // Goal: a selection less than 450 seconds before the newest second holds the seconds up
        // to the newest, read from the ring that has not sealed them.
        // Method: publish 1000 and 1100 without sealing, select 1000, then read the context's
        // last second and the publications its trends hold.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1100);
        let mut session = LiveSession::open(&recording.root).unwrap();

        let selected = session.select(1000, 0).unwrap();

        let publications: Vec<_> = selected["trends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["publication"].as_u64().unwrap())
            .collect();
        assert_eq!(publications, [1, 2]);
        assert_eq!(selected["context_last_second"], 1100);
        assert_eq!(selected["selected"]["publication"], 1);
    }

    #[test]
    fn ends_a_selections_context_at_a_saved_second_the_ring_no_longer_holds() {
        // Goal: the context reaches the newest second the recording saved, even when the ring
        // holds only older seconds after the wall clock moved back.
        // Method: seal 1000 and 1100, publish eight seconds before 1100 so the ring overwrites
        // both, select 1000, then read the context's last second.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1100);
        recording.seal();
        for second in 1001..=1008 {
            recording.publish(second);
        }
        let mut session = LiveSession::open(&recording.root).unwrap();

        let selected = session.select(1000, 0).unwrap();

        assert_eq!(selected["context_last_second"], 1100);
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
    fn preserves_network_packets_and_errors_across_a_real_ring_seal() {
        // Goal: packets and errors retain their exact values and identity after sealing.
        // Method: publish two counter samples through a real ring and reopen saved history.
        let mut recording = Recording::new();
        recording.publish_resources(1000);
        recording.publish_resources(1001);
        let mut live = LiveSession::open(&recording.root).unwrap();

        let before = live.select(1001, 0).unwrap();

        assert_eq!(before["summary"]["network_rx_packets_per_second"], 4.0);
        assert_eq!(before["summary"]["network_tx_packets_per_second"], 0.0);
        assert_eq!(before["summary"]["network_rx_errors_per_second"], 1.0);
        assert_eq!(before["summary"]["network_tx_errors_per_second"], 2.0);
        recording.seal();
        let mut saved = viewer::Session::open(&recording.root).unwrap();

        let after = saved.select(1001, 0).unwrap();

        assert_eq!(before["summary"], after["summary"]);
        assert_eq!(before["trends"], after["trends"]);
        assert_eq!(before["selected"]["run"], after["selected"]["run"]);
        assert_eq!(before["selected"]["boot"], after["selected"]["boot"]);
        assert_eq!(
            before["selected"]["publication"],
            after["selected"]["publication"]
        );
    }

    #[test]
    fn preserves_the_scalar_point_shape_across_a_real_ring_seal() {
        // Goal: public live point fields remain exactly equal after committing the same ticks.
        // Method: publish positive GPU counters through the real ring, then seal and reopen
        // history.
        let mut recording = Recording::new();
        recording.publish_resources(1000);
        recording.publish_resources(1001);
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
        // Method: exceed the raw cache while all publications remain in a 64-slot ring, the
        // last without an event, since the log holds the 32 before it.
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

        assert_rehydrated_apps(
            &session,
            Rehydrated {
                publication: 1,
                point_count: 32,
            },
        );
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
        fixture.publish_with_events(1032, &[]);
        assert!(session.poll().unwrap());
        assert_eq!(session.catalog()["latest_observation"]["publication"], 33);
        assert_eq!(session.catalog()["reader_losses"], catalog["reader_losses"]);
        assert_eq!(session.page("processes", 0).unwrap(), first);
        assert_eq!(session.page("events", 0).unwrap(), events);
        rehydrates_captured_identity_after_real_eviction_without_disturbing_polling_later(
            &mut session,
        );
        assert!(session.catalog()["tail_cache_bytes"].as_u64().unwrap() <= 128 * 1024 * 1024);
        println!(
            "{}",
            serde_json::json!({"identity_observations_per_tick":12000, "publications":33,
                "retained_tail_ticks_before_rehydration":catalog["tail_ticks"],
                "retained_cache_bytes_before_rehydration":catalog["tail_cache_bytes"]})
        );
    }

    /// Checks that selections after the rehydration show the Apps and rows of their own seconds.
    fn rehydrates_captured_identity_after_real_eviction_without_disturbing_polling_later(
        session: &mut LiveSession,
    ) {
        session.select(1001, 0).unwrap();
        assert_rehydrated_apps(
            session,
            Rehydrated {
                publication: 2,
                point_count: 33,
            },
        );
        session.select(1032, 0).unwrap();
        assert_eq!(
            session.page("processes", 0).unwrap()["rows"][6][3],
            "9007199254741025"
        );
    }

    #[derive(Clone, Copy)]
    struct Rehydrated {
        publication: u64,
        point_count: usize,
    }

    fn assert_rehydrated_apps(session: &LiveSession, rehydrated: Rehydrated) {
        let publication = rehydrated.publication;
        let page = session.apps_page(0).unwrap();
        let kind = viewer::live_app_response::KeyKind::Bundle;
        let key = b"/Apps/Current.app";
        let detail = session.app_detail(kind, key, 0).unwrap();
        let history = session.app_history(kind, key).unwrap();

        assert_eq!(
            page["selected"]["stamp"]["publication"],
            publication.to_string()
        );
        assert_eq!(detail["selected"], page["selected"]);
        assert_eq!(history["selected"], page["selected"]);
        assert_eq!(detail["app"]["resident_bytes"]["known"], "11");
        assert_eq!(detail["app"]["footprint_bytes"]["known"], "12");
        assert_eq!(detail["processes"][0]["pid"], 7);
        assert_eq!(detail["processes"][0]["creation"]["unique_id"], "7");
        assert_eq!(
            history["points"].as_array().unwrap().len(),
            rehydrated.point_count
        );
        assert_eq!(
            history["points"][0]["cpu_percent_one_core"],
            serde_json::Value::Null
        );
        let selected = &history["points"][usize::try_from(publication - 1).unwrap()];
        assert_eq!(selected["resident_bytes"]["known"], "11");
        assert_eq!(
            detail["app"]["cpu_percent_one_core"],
            selected["cpu_percent_one_core"]
        );
        assert_eq!(detail["app"]["energy_watts"], selected["energy_watts"]);
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
