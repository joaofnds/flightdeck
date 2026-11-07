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
/// Selects the newest reading the session lists, the last of its second's alternatives.
#[cfg(test)]
pub(crate) fn select_newest(session: &mut viewer::LiveSession) -> serde_json::Value {
    let second = session.catalog()["last_second"].as_u64().unwrap();
    let first = session.select(second, 0).unwrap();
    let alternatives = first["alternatives"].as_array().unwrap().len();
    session.select(second, alternatives - 1).unwrap()
}

#[cfg(test)]
mod tests {
    use collector::machine::Memory;
    use collector::recording::{RegisteredRun, Registration};
    use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, Writer};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use viewer::LiveSession;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Recording {
        root: PathBuf,
        writer: Writer,
        elapsed: u64,
        persister: std::sync::Mutex<persistence::worker::HeldWindows>,
    }

    impl Recording {
        fn new() -> Self {
            Self::with_process_capacity(1)
        }

        fn with_process_capacity(process_capacity: u64) -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("flightdeck-live-{}-{serial}", std::process::id()));
            drop(persistence::Store::open(&root).unwrap());
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
                persister: std::sync::Mutex::default(),
            }
        }

        fn seal(&self) {
            self.seal_window(0);
        }

        /// Saves the windows the ring holds past what is saved through `window_index`, with the
        /// persister's own worker.
        fn seal_window(&self, window_index: u64) {
            let store = persistence::Store::open(&self.root).unwrap();
            let run = RegisteredRun {
                identity: self.writer.identity().unwrap(),
                ring_name: self.writer.name().to_owned(),
                collector_pid: 0,
            };
            persistence::worker::seal_held_windows(
                &store,
                &run,
                Some(window_index),
                &mut self.persister.lock().unwrap(),
                &super::identity_fixture::Underived,
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

    impl Drop for Recording {
        fn drop(&mut self) {
            let name = self.writer.name().to_owned();
            collector::ring::unlink_generation(&name).unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
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
    fn holds_the_seconds_after_a_saved_selection_in_its_context() {
        // Goal: a selection's context holds seconds on both sides of it, so a step to a later
        // second shows readings the context already holds.
        // Method: seal seconds up to 451 past 1000, select 1000,
        // then read the context's last second and the publications its trends hold.
        let mut recording = Recording::new();
        for second in [1000, 1449, 1450, 1451] {
            recording.publish(second);
        }
        recording.seal();
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
        recording.seal();
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
    fn keeps_repeated_wall_seconds_as_distinct_observations() {
        // Goal: a repeated wall second preserves both publications and their identities.
        // Method: publish the same fake wall clock twice and inspect both alternatives.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1000);
        recording.seal();
        let mut session = LiveSession::open(&recording.root).unwrap();

        let first = session.select(1000, 0).unwrap();
        let second = session.select(1000, 1).unwrap();

        assert_eq!(first["alternatives"].as_array().unwrap().len(), 2);
        assert_eq!(first["selected"]["publication"], 1);
        assert_eq!(second["selected"]["publication"], 2);
        assert_ne!(first["summary"]["segment"], second["summary"]["segment"]);
    }

    #[test]
    fn joins_consecutive_points_after_an_earlier_historical_gap() {
        // Goal: a gap in the saved readings does not split the valid interval after it.
        // Method: preserve an early committed gap and read the context after it.
        let mut recording = Recording::new();
        recording.publish(1000);
        recording.publish(1002);
        recording.publish(1003);
        recording.seal();
        let mut session = LiveSession::open(&recording.root).unwrap();

        let response = session.select(1003, 0).unwrap();
        let points = response["trends"].as_array().unwrap();

        assert_ne!(points[0]["segment"], points[1]["segment"]);
        assert_eq!(points[1]["segment"], points[2]["segment"]);
        assert_eq!(response["summary"]["segment"], points[2]["segment"]);
    }

    #[test]
    fn preserves_network_packets_and_errors_across_a_real_ring_seal() {
        // Goal: packets and errors retain their exact values and identity after sealing.
        // Method: publish two counter samples through a real ring and reopen saved history.
        let mut recording = Recording::new();
        recording.publish_resources(1000);
        recording.publish_resources(1001);
        recording.seal();
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
}

#[cfg(test)]
mod registered_checks {
    use super::identity_fixture::Fixture;
    use viewer::LiveSession;

    #[test]
    fn a_live_session_reads_a_saved_minute_of_a_registered_recording() {
        // Goal: everything a live session answers from a registered recording reaches it through
        // the interface the desktop app uses.
        // Method: save a minute through the worker, open a live session and select its second.
        let mut fixture = Fixture::registered();
        fixture.publish(1000);
        fixture.seal();
        let mut session = LiveSession::open(&fixture.root).unwrap();

        let selected = session.select(1000, 0).unwrap();

        assert_eq!(session.catalog()["last_second"], 1000);
        assert_eq!(selected["selected"]["publication"], 1);
    }

    #[test]
    fn a_live_session_counts_a_reading_with_no_clock_beside_the_one_it_selects() {
        // Goal: a reading no clock can place reaches the desktop app as a count, and costs it
        // nothing of the reading beside it.
        // Method: save a minute of a reading with clocks and one without, and select the first.
        let mut fixture = Fixture::registered();
        fixture.publish_with_system_rows(1000);
        fixture.publish_without_clock();
        fixture.seal();
        let mut session = LiveSession::open(&fixture.root).unwrap();

        let selected = session.select(1000, 0).unwrap();

        assert_eq!(session.catalog()["unknown_clock_ticks"], 1);
        assert_eq!(selected["selected"]["publication"], 1);
    }
}
