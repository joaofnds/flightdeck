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
                state_capacity: 1,
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
}
