#[cfg(test)]
#[path = "../src/bin/recording_check/capture.rs"]
mod capture;
#[cfg(test)]
#[path = "../src/bin/recording_check/compare.rs"]
mod compare;
#[cfg(test)]
#[path = "encoder_fixture/mod.rs"]
pub mod fixture;
#[path = "../src/bin/recording_check/identity_reference.rs"]
mod identity_reference;
#[cfg(test)]
#[path = "../src/bin/recording_check/rows.rs"]
mod rows;

#[cfg(test)]
mod tests {
    use super::{capture, compare, fixture, rows};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use collector::recording::private;
    use collector::ring::{
        DictionaryCapacity, Found, FoundEvent, Identity, IdentityObservation, IdentityPublication,
        IdentityUnavailable, ObservationTimes, Place, ProcessCreation, ProcessIdentity,
        ProgramBytes, Readings, RingReader, RunIdentity, Shape, Tick, UsageAvailability, Writer,
    };
    use collector::tracker::{Event, Noticed};
    use persistence::{
        Begin, Encoder, IdentitySource, Limits, Losses, Manifest, NumberedEvent, Observation,
        Store, TimeBounds, Window, WindowIdentity,
    };

    struct Recording(PathBuf);
    struct Frame {
        publication: u64,
        tick: Tick,
        events: Vec<Event>,
    }

    #[test]
    fn captures_ordinary_legacy_numeric_readings_and_events_before_sealing() {
        // Goal: the ordinary v3 writer remains capturable with exact numeric/event fidelity.
        // Method: complete a real ring, capture v1, stage its readings, seal and verify after exit.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let identity = WindowIdentity {
            run: RunIdentity {
                recording: store.recording(),
                run: Identity([31, 32]),
                boot: Identity([41, 42]),
            },
            window: 0,
        };
        let shape = fixture::profile(false);
        let place = Place {
            lock_path: recording.0.join("legacy.lock"),
            name_prefix: format!("/fdlegacycap{}", std::process::id()),
        };
        let mut writer = Writer::create_recording(&place, shape, &identity.run, 0).unwrap();
        let mut source = frame(&shape, 0);
        source.tick.events = 1..6;
        publish_captured_legacy(&mut writer, &source);
        writer.finish();
        let mut reader = RingReader::open(&place.lock_path).unwrap();
        assert_eq!(3, reader.source_layout_version());
        let reference = recording.0.join("source.reference");

        let captured = capture::run(&place.lock_path, &reference, 10);
        let window = stage_captured_legacy(&mut reader, &identity, &shape);
        commit_identity(&store, &window, |_| {});
        drop(reader);
        let name = writer.name().to_owned();
        drop(writer);
        collector::ring::unlink_generation(&name).unwrap();

        captured.unwrap();
        let source = fs::read_to_string(&reference).unwrap();
        assert!(source.starts_with("flightdeck-reference-1 "));
        assert_eq!(
            40,
            source
                .lines()
                .filter(|line| line.starts_with("processes "))
                .count()
        );
        assert_eq!(
            5,
            source
                .lines()
                .filter(|line| line.starts_with("events "))
                .count()
        );
        compare::run(&recording.0, &reference).unwrap();
    }

    fn publish_captured_legacy(writer: &mut Writer, source: &Frame) {
        writer.publish_observed(
            &Readings {
                run_tick: source.tick.run_tick,
                cpus: &source.tick.cpus,
                memory: &source.tick.memory,
                interfaces: &source.tick.interfaces,
                ioreport: None,
                smc: None,
                processes: &source.tick.processes,
                processes_refused: source.tick.processes_refused,
                processes_gone: source.tick.processes_gone,
                coalitions: &source.tick.coalitions,
                events: &source.events,
                dropped: source.tick.dropped,
            },
            source.tick.times.as_ref().unwrap(),
        );
    }

    fn stage_captured_legacy(
        reader: &mut RingReader,
        identity: &WindowIdentity,
        shape: &Shape,
    ) -> Window {
        let mut tick = persistence::tick_buffer(shape).unwrap();
        assert!(matches!(
            reader.read_into(&mut tick).unwrap(),
            Found::Publication { publication: 1, .. }
        ));
        let mut events = Vec::with_capacity(5);
        for _ in 0..5 {
            let FoundEvent::Event { event, .. } = reader.read_event().unwrap() else {
                panic!("legacy event missing");
            };
            events.push(event);
        }
        stage(
            identity,
            shape,
            &[Frame {
                publication: 1,
                tick,
                events,
            }],
        )
    }

    #[test]
    fn refuses_independent_event_loss_while_both_publications_remain() {
        // Goal: retained publications cannot hide loss from the independent event log.
        // Method: put two tick events into a one-event v4 log retaining both publication slots.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let identity = RunIdentity {
            recording: store.recording(),
            run: Identity([31, 32]),
            boot: Identity([41, 42]),
        };
        let place = Place {
            lock_path: recording.0.join("event-loss.lock"),
            name_prefix: format!("/fdeventcap{}", std::process::id()),
        };
        let shape = Shape {
            slot_count: 2,
            event_capacity: 1,
            ..fixture::profile(false)
        };
        let mut writer = Writer::create_with_identity(
            &place,
            shape,
            DictionaryCapacity {
                entries: 1,
                arena_bytes: 32,
            },
            &identity,
            0,
        )
        .unwrap();
        publish_event_loss(&mut writer);
        writer.finish();
        assert_event_loss(&place, &shape);
        let reference = recording.0.join("source.reference");

        let result = capture::run(&place.lock_path, &reference, 10);
        let name = writer.name().to_owned();
        drop(writer);
        collector::ring::unlink_generation(&name).unwrap();

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("reference missed events"))
        ));
        let source = fs::read_to_string(&reference).unwrap();
        assert!(source.contains("begin 1 "));
        assert!(!source.contains("complete "));
    }

    fn publish_event_loss(writer: &mut Writer) {
        for index in 0..2 {
            writer
                .publish_with_identity(
                    &Readings {
                        run_tick: index,
                        cpus: &[],
                        memory: &collector::machine::Memory::default(),
                        interfaces: &[],
                        ioreport: None,
                        smc: None,
                        processes: &[],
                        processes_refused: 0,
                        processes_gone: 0,
                        coalitions: &[],
                        events: &[Event::Exec { pid: 9001 }],
                        dropped: collector::ring::Dropped::default(),
                    },
                    Some(&ObservationTimes {
                        wall_ns: 50_000_000_000 + index * 1_000_000_000,
                        continuous_ns: index,
                        awake_ns: index,
                    }),
                    &IdentityPublication {
                        processes: &[],
                        events: &[source_identity(9001, UsageAvailability::NotApplicable)],
                    },
                )
                .unwrap();
        }
    }

    fn assert_event_loss(place: &Place, shape: &Shape) {
        let mut reader = RingReader::open(&place.lock_path).unwrap();
        assert_eq!(4, reader.source_layout_version());
        let mut tick = persistence::tick_buffer(shape).unwrap();
        assert!(matches!(
            reader.read_into(&mut tick).unwrap(),
            Found::Publication { publication: 1, .. }
        ));
        assert_eq!(1..2, tick.events);
        assert!(matches!(
            reader.read_into(&mut tick).unwrap(),
            Found::Publication { publication: 2, .. }
        ));
        assert_eq!(2..3, tick.events);
        reader.seek_event(1).unwrap();
        let mut identity = IdentityObservation::EMPTY;
        assert!(matches!(
            reader.read_event_identity(&mut identity).unwrap(),
            FoundEvent::Missed { first: 1, end: 2 }
        ));
    }

    #[test]
    fn refuses_a_completed_source_without_publications() {
        // Goal: a successful fidelity reference cannot silently omit a v4 identity section.
        // Method: capture an orderly completed v4 source before creating any reference file.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let place = Place {
            lock_path: recording.0.join("ring.lock"),
            name_prefix: format!("/fdfid{}", std::process::id()),
        };
        let identity = RunIdentity {
            recording: store.recording(),
            run: Identity([31, 32]),
            boot: Identity([41, 42]),
        };
        let mut writer = Writer::create_with_identity(
            &place,
            fixture::profile(false),
            DictionaryCapacity {
                entries: 1,
                arena_bytes: 32,
            },
            &identity,
            1,
        )
        .unwrap();
        writer.finish();
        let reference = recording.0.join("source.reference");

        let result = capture::run(&place.lock_path, &reference, 1);

        collector::ring::unlink_generation(writer.name()).unwrap();
        assert!(matches!(
            result,
            Err(persistence::Error::Bound(
                "reference contains no publications"
            ))
        ));
        assert!(
            !fs::read_to_string(&reference)
                .unwrap()
                .contains("complete ")
        );
    }

    #[test]
    fn refuses_a_source_whose_initial_publication_was_overwritten() {
        // Goal: a retained suffix cannot be mistaken for a complete captured source.
        // Method: publish two ticks into one slot before opening the capture reader.
        let recording = Recording::new();

        let result = capture_empty_source(&recording, 1, 2, true);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound(
                "capture opened after initial publications were overwritten"
            ))
        ));
        assert!(!recording.0.join("source.reference").exists());
    }

    #[test]
    fn refuses_a_source_without_orderly_completion() {
        // Goal: timing out never leaves a successful-looking capture.
        // Method: capture one publication from an unfinished source with a one-second budget.
        let recording = Recording::new();

        let result = capture_empty_source(&recording, 2, 1, false);

        assert!(result.is_err());
        assert!(
            !fs::read_to_string(recording.0.join("source.reference"))
                .unwrap()
                .contains("complete ")
        );
    }

    fn capture_empty_source(
        recording: &Recording,
        slots: u64,
        publications: u64,
        finish: bool,
    ) -> persistence::Result<()> {
        assert!(publications <= 2);
        let store = Store::open(&recording.0).unwrap();
        let place = Place {
            lock_path: recording.0.join("ring.lock"),
            name_prefix: format!("/fdfid{}", std::process::id()),
        };
        let identity = RunIdentity {
            recording: store.recording(),
            run: Identity([31, 32]),
            boot: Identity([41, 42]),
        };
        let mut shape = fixture::profile(false);
        shape.slot_count = slots;
        let mut writer = Writer::create_with_identity(
            &place,
            shape,
            DictionaryCapacity {
                entries: 1,
                arena_bytes: 32,
            },
            &identity,
            1,
        )
        .unwrap();
        for index in 0..publications {
            writer
                .publish_with_identity(
                    &Readings {
                        run_tick: index,
                        cpus: &[],
                        memory: &collector::machine::Memory::default(),
                        interfaces: &[],
                        ioreport: None,
                        smc: None,
                        processes: &[],
                        processes_refused: 0,
                        processes_gone: 0,
                        coalitions: &[],
                        events: &[],
                        dropped: collector::ring::Dropped::default(),
                    },
                    Some(&ObservationTimes {
                        wall_ns: 50_000_000_000,
                        continuous_ns: index,
                        awake_ns: index,
                    }),
                    &IdentityPublication {
                        processes: &[],
                        events: &[],
                    },
                )
                .unwrap();
        }
        if finish {
            writer.finish();
        }
        let result = capture::run(&place.lock_path, &recording.0.join("source.reference"), 1);
        let name = writer.name().to_owned();
        drop(writer);
        collector::ring::unlink_generation(&name).unwrap();
        result
    }

    #[test]
    fn every_source_field_survives_encoding_against_the_independent_reference() {
        // Goal: field swaps and invented availability fail independently of encoder mappings.
        // Method: serialize source ticks/events before staging, then compare all ten disk tables.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode(&reference, |_| {});

        compare::run(&recording.0, &reference).unwrap();
    }

    #[test]
    fn refuses_oversized_disk_text_and_blob_without_panicking() {
        // Goal: untrusted disk cells cannot grow a reference row beyond its bound or abort.
        // Method: seal oversized interface names of both storage types and compare them.
        for kind in ["VARCHAR", "BLOB"] {
            let recording = Recording::new();
            let reference = recording.0.join("source.reference");
            recording.encode(&reference, |directory| {
                rewrite_table(
                    directory,
                    persistence::schema::Table::Interfaces,
                    &format!("SELECT * REPLACE (repeat('a',20000)::{kind} AS name) FROM source"),
                );
            });

            let result = compare::run(&recording.0, &reference);

            assert!(matches!(result, Err(persistence::Error::Io(_))));
        }
    }

    #[test]
    fn every_original_field_survives_v4_staging_and_encoding_against_the_reference() {
        // Goal: identity encoding preserves the independent original numeric/event contract.
        // Method: stage real v4 readings with Start coalition 17, seal, and compare every table.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        let store = Store::open(&recording.0).unwrap();
        let identity = WindowIdentity {
            run: RunIdentity {
                recording: store.recording(),
                run: Identity([31, 32]),
                boot: Identity([41, 42]),
            },
            window: 0,
        };
        let window = stage_identity(&recording, &reference, &identity);

        commit_identity(&store, &window, |_| {});

        compare::run(&recording.0, &reference).unwrap();
    }

    #[test]
    fn compares_original_numeric_fields_without_identity_only_presence() {
        // Goal: appended identity metadata does not change numeric reference fidelity.
        // Method: augment private Parquet files and add refused-usage presence before sealing.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode(&reference, augment_identity);

        compare::run(&recording.0, &reference).unwrap();
    }

    #[test]
    fn still_detects_a_changed_numeric_counter_in_augmented_history() {
        // Goal: excluding metadata from numeric fidelity does not exclude measured counters.
        // Method: change one augmented user-time value before committing its window.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode(&reference, |directory| {
            augment_identity(directory);
            rewrite_processes(
                directory,
                "SELECT * REPLACE (user_ns+1 AS user_ns) FROM source",
            );
        });

        let result = compare::run(&recording.0, &reference);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound(
                "source and disk typed values differ"
            ))
        ));
    }

    #[test]
    fn captures_exact_long_raw_identity_and_independent_event_metadata() {
        // Goal: reference capability includes every observation and lossless bounded bytes.
        // Method: compare captured source lines with independent literal protocol expectations.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode_identity(&reference, |_| {});
        let source = fs::read_to_string(&reference).unwrap();
        let name = format!("ff0a5c09{}", "78".repeat(4091));
        let app = format!("2f{}2e617070", "79".repeat(4090));
        let refused = format!(
            "process_identity u1 i1{} u1 u0 u1 u18446744073709551615 \
            i-2147483648 x{name} x{app}\n",
            " n".repeat(19)
        );
        let gone = refused
            .replacen("i1", "i2", 1)
            .replacen("u0 u1", "u0 u2", 1);

        compare::run(&recording.0, &reference).unwrap();

        assert!(source.contains(&refused));
        assert!(source.contains(&gone));
        assert!(source.contains(
            "event_identity u1 u1 i9001 u1 u0 u3 u18446744073709551615 \
            i-2147483648 x6576656e74ff0a5c n\n"
        ));
        assert_eq!(
            4,
            source
                .lines()
                .filter(|line| line.starts_with("process_identity "))
                .count()
        );
        assert_eq!(
            5,
            source
                .lines()
                .filter(|line| line.starts_with("event_identity "))
                .count()
        );
    }

    macro_rules! identity_mutations {
        ($($name:ident, $table:expr, $selection:expr, $error:expr;)*) => {$(
            #[test]
            fn $name() {
                // Goal: disk mutation cannot pass independent captured source fidelity.
                // Method: change private encoded output, seal it, then compare after retirement.
                let recording = Recording::new();
                let reference = recording.0.join("source.reference");
                recording.encode_identity(&reference, |directory| {
                    rewrite_table(directory, $table, $selection);
                });

                let result = compare::run(&recording.0, &reference);

                assert!(matches!(&result, Err(persistence::Error::Bound($error))), "{result:?}");
            }
        )*};
    }

    identity_mutations! {
        detects_a_measured_counter_change_in_captured_v4, persistence::schema::Table::Processes,
            "SELECT * REPLACE(user_ns+1 AS user_ns) FROM source",
            "source and disk typed values differ";
        detects_changed_raw_process_identity, persistence::schema::Table::Processes,
            "SELECT * REPLACE('different'::BLOB AS executable_name) FROM source WHERE pid<3 \
            UNION ALL SELECT * FROM source WHERE pid=3",
            "source and disk typed values differ";
        detects_changed_raw_app_identity, persistence::schema::Table::Processes,
            "SELECT * REPLACE('/Other.app'::BLOB AS app_path) FROM source WHERE pid<3 \
            UNION ALL SELECT * FROM source WHERE pid=3",
            "source and disk typed values differ";
        detects_changed_pid_version, persistence::schema::Table::Processes,
            "SELECT * REPLACE(pid_version+1 AS pid_version) FROM source",
            "source and disk typed values differ";
        refuses_changed_usage_availability, persistence::schema::Table::Processes,
            "SELECT * REPLACE(2::UTINYINT AS usage_availability) FROM source WHERE pid=1 \
            UNION ALL SELECT * FROM source WHERE pid<>1",
            "source and disk typed values differ";
        detects_changed_creation_identity, persistence::schema::Table::Processes,
            "SELECT * REPLACE(process_unique_id-1 AS process_unique_id) FROM source",
            "source and disk typed values differ";
        refuses_changed_identity_version, persistence::schema::Table::Processes,
            "SELECT * REPLACE(9::UTINYINT AS identity_version) FROM source",
            "process identity version";
        refuses_changed_identity_availability, persistence::schema::Table::Processes,
            "SELECT * REPLACE(6::UTINYINT AS identity_availability) FROM source",
            "unavailable process executable";
        refuses_invented_usage_for_identity_only_rows, persistence::schema::Table::Processes,
            "SELECT * REPLACE(0::UBIGINT AS user_ns) FROM source",
            "process usage payload";
        detects_omitted_identity_only_observation, persistence::schema::Table::Processes,
            "SELECT * FROM source WHERE pid<>1",
            "source and disk typed values differ";
        detects_extra_identity_only_observation, persistence::schema::Table::Processes,
            "SELECT * FROM source UNION ALL SELECT * FROM source WHERE pid=1",
            "source and disk typed values differ";
        detects_changed_unavailable_event_identity_reason, persistence::schema::Table::Events,
            "SELECT * REPLACE(1::UTINYINT AS identity_availability) FROM source WHERE event=2 \
            UNION ALL SELECT * FROM source WHERE event<>2",
            "source and disk typed values differ";
        detects_omitted_independent_event_identity, persistence::schema::Table::Events,
            "SELECT * FROM source WHERE event<>1",
            "source and disk typed values differ";
        detects_extra_independent_event_identity, persistence::schema::Table::Events,
            "SELECT * FROM source UNION ALL SELECT * FROM source WHERE event=1",
            "source and disk typed values differ";
        detects_changed_independent_event_identity, persistence::schema::Table::Events,
            "SELECT * REPLACE('event-other'::BLOB AS executable_name) FROM source WHERE event=1 \
            UNION ALL SELECT * FROM source WHERE event<>1",
            "source and disk typed values differ";
    }

    macro_rules! reference_mutations {
        ($($name:ident, $before:expr, $after:expr, $error:expr;)*) => {$(
            #[test]
            fn $name() {
                // Goal: unsupported or incomplete references cannot report fidelity.
                // Method: change one captured reference element and run the real verifier.
                let recording = Recording::new();
                let reference = recording.0.join("source.reference");
                recording.encode_identity(&reference, |_| {});
                let source = fs::read_to_string(&reference).unwrap();
                fs::write(&reference, source.replace($before, $after)).unwrap();

                let result = compare::run(&recording.0, &reference);

                assert!(matches!(result, Err(persistence::Error::Bound($error))));
            }
        )*};
    }

    reference_mutations! {
        refuses_unknown_reference_version, "flightdeck-reference-2", "flightdeck-reference-9",
            "reference version";
        refuses_unknown_identity_capability, "identity-1", "identity-9",
            "reference identity capability";
        refuses_an_unterminated_reference_line, "complete 1\n", "complete 1",
            "reference unterminated line";
        refuses_truncated_reference_completion, "complete 1\n", "", "reference is incomplete";
        refuses_identity_rows_in_a_numeric_only_reference,
            "flightdeck-reference-2 identity-1", "flightdeck-reference-1", "reference row kind";
        refuses_reference_publication_mismatch, "process_identity u1", "process_identity u2",
            "reference row publication";

    }

    #[test]
    fn refuses_completion_without_a_matching_source_tick_in_both_reference_versions() {
        // Goal: an empty comparison cannot claim a captured publication was verified.
        // Method: retain a real header/time but replace its data with a nonexistent publication.
        for identity_capable in [true, false] {
            let recording = Recording::new();
            let reference = recording.0.join("source.reference");
            recording.encode_identity(&reference, |_| {});
            let source = fs::read_to_string(&reference).unwrap();
            let header = source.lines().next().unwrap();
            let header = if identity_capable {
                header.to_owned()
            } else {
                header.replace(
                    "flightdeck-reference-2 identity-1",
                    "flightdeck-reference-1",
                )
            };
            fs::write(
                &reference,
                format!("{header}\nbegin 18446744073709551615 50000000111\ncomplete 1\n"),
            )
            .unwrap();

            let result = compare::run(&recording.0, &reference);

            assert!(matches!(
                result,
                Err(persistence::Error::Bound("reference missing tick"))
            ));
        }
    }

    #[test]
    fn refuses_a_publication_header_time_that_disagrees_with_its_tick() {
        // Goal: reference discovery must use the time actually carried by its source tick.
        // Method: change only the begin time within the same discoverable wall second.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode_identity(&reference, |_| {});
        let source = fs::read_to_string(&reference).unwrap();
        fs::write(
            &reference,
            source.replace("begin 1 50000000111", "begin 1 50000000112"),
        )
        .unwrap();

        let result = compare::run(&recording.0, &reference);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("reference tick time"))
        ));
    }

    #[test]
    fn detects_missing_identity_reference_rows() {
        // Goal: a v2 header alone cannot imply identity fidelity.
        // Method: remove captured identity rows while preserving all original numeric rows.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode_identity(&reference, |_| {});
        let source = fs::read_to_string(&reference).unwrap();
        let source = source
            .lines()
            .filter(|line| !line.starts_with("process_identity "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&reference, source).unwrap();

        let result = compare::run(&recording.0, &reference);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("disk has extra rows"))
        ));
    }

    #[test]
    fn refuses_an_oversized_reference_line() {
        // Goal: verification refuses oversized input before allocating an unbounded line.
        // Method: replace a valid source header with a 32769-byte record.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode_identity(&reference, |_| {});
        fs::write(&reference, "x".repeat(32_769)).unwrap();

        let result = compare::run(&recording.0, &reference);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("reference line limit"))
        ));
    }

    #[test]
    fn refuses_an_oversized_reference_file() {
        // Goal: verification rejects disk input beyond its file bound before reading.
        // Method: grow the existing reference sparsely past 8 GiB.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode_identity(&reference, |_| {});
        fs::OpenOptions::new()
            .write(true)
            .open(&reference)
            .unwrap()
            .set_len(8 * 1024 * 1024 * 1024 + 1)
            .unwrap();

        let result = compare::run(&recording.0, &reference);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("reference exceeds 8 GiB"))
        ));
    }

    #[test]
    fn refuses_unknown_rows_in_an_identity_capable_reference() {
        // Goal: an unsupported source record never implies complete identity fidelity.
        // Method: add an unknown row to an otherwise captured and sealed reference.
        let recording = Recording::new();
        let reference = recording.0.join("source.reference");
        recording.encode_identity(&reference, |_| {});
        let source = fs::read_to_string(&reference).unwrap();
        fs::write(
            &reference,
            source.replace("complete 1", "alien u1\ncomplete 1"),
        )
        .unwrap();

        let result = compare::run(&recording.0, &reference);

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("reference row kind"))
        ));
    }

    impl Recording {
        fn encode_identity(
            &self,
            reference: &std::path::Path,
            extend: impl FnOnce(&std::path::Path),
        ) {
            let store = Store::open(&self.0).unwrap();
            let identity = WindowIdentity {
                run: RunIdentity {
                    recording: store.recording(),
                    run: Identity([31, 32]),
                    boot: Identity([41, 42]),
                },
                window: 0,
            };
            let window = stage_identity(self, reference, &identity);
            commit_identity(&store, &window, extend);
        }
    }

    impl Recording {
        fn encode(&self, reference: &std::path::Path, extend: impl FnOnce(&std::path::Path)) {
            let store = Store::open(&self.0).unwrap();
            let shape = fixture::profile(false);
            let identity = WindowIdentity {
                run: RunIdentity {
                    recording: store.recording(),
                    run: Identity([31, 32]),
                    boot: Identity([41, 42]),
                },
                window: 0,
            };
            let frames = [frame(&shape, 0), frame(&shape, 1)];
            write_reference(reference, &identity.run, &frames);

            let window = stage(&identity, &shape, &frames);
            let Begin::Pending(pending) = store.begin(&identity).unwrap() else {
                panic!("fresh fidelity fixture was committed");
            };
            let mut tables = Encoder.encode(&window, pending.path()).unwrap();
            extend(pending.path());
            encode_row_counts(pending.path(), &mut tables);
            store
                .commit(
                    pending,
                    Manifest {
                        identity,
                        publication_first: 17,
                        publication_end: 19,
                        events_first: 101,
                        events_end: 111,
                        wall_ns: Some(TimeBounds {
                            min: 50_000_000_111,
                            max: 50_000_001_111,
                        }),
                        continuous_ns: Some(TimeBounds {
                            min: 100_000_000_333,
                            max: 100_000_001_333,
                        }),
                        awake_ns: Some(TimeBounds {
                            min: 80_000_000_777,
                            max: 80_000_001_777,
                        }),
                        losses: Losses::default(),
                        data_directory: String::new(),
                        tables,
                    },
                )
                .unwrap();
        }
    }

    impl Recording {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            Self(PathBuf::from(format!(
                "/private/tmp/flightdeck-fidelity-{}-{serial}",
                std::process::id()
            )))
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn encode_row_counts(directory: &std::path::Path, tables: &mut [persistence::TableManifest]) {
        let config = duckdb::Config::default().threads(1).unwrap();
        let connection = duckdb::Connection::open_in_memory_with_flags(config).unwrap();

        for table in tables {
            let path = directory.join(format!("{}.parquet", table.name));
            table.rows = connection
                .query_row(
                    "SELECT count(*) FROM read_parquet(?)",
                    [path.to_str().unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
        }
    }

    fn augment_identity(directory: &std::path::Path) {
        use persistence::schema::Table;
        let nulls = Table::Processes
            .columns()
            .skip(2)
            .map(|column| format!("NULL AS {column}"))
            .collect::<Vec<_>>()
            .join(",");
        let metadata = "1::UTINYINT identity_version,0::UTINYINT identity_availability, \
            18446744073709551615::UBIGINT process_unique_id, \
            (-2147483648)::INTEGER pid_version,'worker\\xFF'::BLOB executable_name, \
            '/Applications/A.app'::BLOB app_path";

        rewrite_processes(
            directory,
            &format!(
                "SELECT *,0::UTINYINT usage_availability,{metadata} FROM source UNION ALL \
                SELECT DISTINCT publication,77777::INTEGER pid,{nulls}, \
                1::UTINYINT usage_availability,{metadata} FROM source"
            ),
        );
        rewrite_table(
            directory,
            Table::Events,
            &format!("SELECT *,availability usage_availability,{metadata} FROM source"),
        );
    }

    fn rewrite_processes(directory: &std::path::Path, selection: &str) {
        rewrite_table(directory, persistence::schema::Table::Processes, selection);
    }

    fn rewrite_table(
        directory: &std::path::Path,
        table: persistence::schema::Table,
        selection: &str,
    ) {
        let config = duckdb::Config::default().threads(1).unwrap();
        let connection = duckdb::Connection::open_in_memory_with_flags(config).unwrap();
        let original = directory.join(format!("{}.parquet", table.name()));
        let extended = directory.join("extended.parquet");
        let source = persistence::query::parquet_literal(&original).unwrap();
        let target = persistence::query::parquet_literal(&extended).unwrap();

        connection
            .execute_batch(&format!(
                "CREATE TABLE source AS SELECT * FROM read_parquet('{source}'); \
                COPY ({selection}) TO '{target}' (FORMAT PARQUET,COMPRESSION ZSTD)"
            ))
            .unwrap();
        fs::rename(extended, original).unwrap();
    }

    fn frame(shape: &Shape, index: u64) -> Frame {
        let mut measured = *shape;
        if index == 1 {
            measured.core_capacity = 0;
            measured.state_capacity = 0;
            measured.fan_capacity = 0;
        }
        let mut tick = fixture::tick(&measured);
        tick.run_tick = index + 4;
        tick.times = Some(ObservationTimes {
            wall_ns: 50_000_000_111 + index * 1000,
            continuous_ns: 100_000_000_333 + index * 1000,
            awake_ns: 80_000_000_777 + index * 1000,
        });
        tick.processes[0].usage.user_ns = 0;
        if index == 1 {
            tick.processes[0].usage.awake_ns_at_start = 80_000_001_000;
            tick.ioreport_read = false;
            tick.smc_read = false;
        }
        let events = vec![
            Event::Start {
                pid: 9001,
                coalition: Some(17),
            },
            Event::Exec { pid: 9001 },
            Event::Exit {
                pid: 9001,
                last: Ok(fixture::usage(101)),
                noticed: Noticed::OnExit,
            },
            Event::Exit {
                pid: 9002,
                last: Err(collector::processes::Unreadable::Refused),
                noticed: Noticed::AlreadyExited,
            },
            Event::Exit {
                pid: 9003,
                last: Err(collector::processes::Unreadable::Gone),
                noticed: Noticed::AlreadyExited,
            },
        ];
        tick.events = 101 + index * 5..106 + index * 5;
        Frame {
            publication: 17 + index,
            tick,
            events,
        }
    }

    fn write_reference(path: &std::path::Path, identity: &RunIdentity, frames: &[Frame]) {
        let mut file = private::open_private_file(path, true).unwrap();
        writeln!(
            file,
            "flightdeck-reference-1 {} {} {} {} {} {}",
            identity.recording.0[0],
            identity.recording.0[1],
            identity.run.0[0],
            identity.run.0[1],
            identity.boot.0[0],
            identity.boot.0[1]
        )
        .unwrap();
        for frame in frames {
            writeln!(
                file,
                "begin {} {}",
                frame.publication,
                frame.tick.times.unwrap().wall_ns
            )
            .unwrap();
            rows::tick(&mut file, frame.publication, &frame.tick).unwrap();
            for (index, event) in frame.events.iter().enumerate() {
                rows::event(
                    &mut file,
                    frame.publication,
                    frame.tick.events.start + u64::try_from(index).unwrap(),
                    frame.tick.run_tick,
                    event,
                )
                .unwrap();
            }
        }
        writeln!(file, "complete {}", frames.len()).unwrap();
    }

    fn stage(identity: &WindowIdentity, shape: &Shape, frames: &[Frame]) -> Window {
        let mut window = Window::new(*identity, Limits::from_shape(shape).unwrap());
        for frame in frames {
            let times = frame.tick.times.unwrap();
            let events: Vec<_> = frame
                .events
                .iter()
                .enumerate()
                .map(|(index, event)| NumberedEvent {
                    number: frame.tick.events.start + u64::try_from(index).unwrap(),
                    tick: frame.tick.run_tick,
                    event,
                })
                .collect();
            window
                .push_tick(
                    &Observation {
                        publication: frame.publication,
                        wall_ns: Some(times.wall_ns),
                        continuous_ns: Some(times.continuous_ns),
                        awake_ns: Some(times.awake_ns),
                    },
                    &frame.tick,
                    &events,
                )
                .unwrap();
        }
        window
    }
    fn stage_identity(
        recording: &Recording,
        reference: &std::path::Path,
        identity: &WindowIdentity,
    ) -> Window {
        let shape = fixture::profile(false);
        let place = Place {
            lock_path: recording.0.join("identity.lock"),
            name_prefix: format!("/fdfid{}", std::process::id()),
        };
        let mut writer = Writer::create_with_identity(
            &place,
            shape,
            DictionaryCapacity {
                entries: 2,
                arena_bytes: 16_384,
            },
            &identity.run,
            0,
        )
        .unwrap();
        let mut source = frame(
            &Shape {
                core_capacity: 0,
                state_capacity: 0,
                fan_capacity: 0,
                ..shape
            },
            0,
        );
        source.publication = 1;
        source.tick.events = 1..6;
        source.tick.ioreport_read = false;
        source.tick.smc_read = false;
        source
            .tick
            .processes
            .retain(|process| [0, 3].contains(&process.pid));
        source.tick.processes_refused = 1;
        source.tick.processes_gone = 1;
        stage_identity_publish(&mut writer, &source);
        writer.finish();
        capture::run(&place.lock_path, reference, 10).unwrap();
        assert!(
            fs::read_to_string(reference)
                .unwrap()
                .starts_with("flightdeck-reference-2 identity-1 ")
        );
        let mut reader = RingReader::open(&place.lock_path).unwrap();
        let window = stage_identity_read(&mut reader, identity, &shape);
        drop(reader);
        let name = writer.name().to_owned();
        drop(writer);
        collector::ring::unlink_generation(&name).unwrap();
        window
    }

    fn stage_identity_read(
        reader: &mut RingReader,
        identity: &WindowIdentity,
        shape: &Shape,
    ) -> Window {
        let mut tick = persistence::tick_buffer(shape).unwrap();
        let mut processes = Vec::with_capacity(40);
        assert!(matches!(
            reader
                .read_identity_into(&mut tick, &mut processes)
                .unwrap(),
            Found::Publication { publication: 1, .. }
        ));
        let mut events = Vec::new();
        let mut identities = Vec::new();
        for _index in 0..5 {
            let mut identity = IdentityObservation::EMPTY;
            let FoundEvent::Event {
                number,
                tick,
                event,
            } = reader.read_event_identity(&mut identity).unwrap()
            else {
                panic!("event missing");
            };
            events.push((number, tick, event));
            identities.push(identity);
        }
        let events: Vec<_> = events
            .iter()
            .map(|(number, tick, event)| NumberedEvent {
                number: *number,
                tick: *tick,
                event,
            })
            .collect();
        let times = tick.times.unwrap();
        let mut window = Window::new(*identity, Limits::from_shape(shape).unwrap());
        window
            .push_tick_with_identity(
                &Observation {
                    publication: 1,
                    wall_ns: Some(times.wall_ns),
                    continuous_ns: Some(times.continuous_ns),
                    awake_ns: Some(times.awake_ns),
                },
                &tick,
                &events,
                &IdentitySource {
                    reader,
                    processes: &processes,
                    events: &identities,
                },
            )
            .unwrap();
        window
    }

    fn stage_identity_publish(writer: &mut Writer, source: &Frame) {
        let processes = stage_identity_publish_processes(writer);
        let event_program = intern(writer, b"event\xff\n\\", None);
        let mut events: Vec<_> = source
            .events
            .iter()
            .map(|event| match event {
                Event::Start { pid, .. } | Event::Exec { pid } => {
                    source_identity(*pid, UsageAvailability::NotApplicable)
                }
                Event::Exit { pid, last, .. } => source_identity(
                    *pid,
                    match last {
                        Ok(_) => UsageAvailability::Measured,
                        Err(collector::processes::Unreadable::Refused) => {
                            UsageAvailability::Refused
                        }
                        Err(collector::processes::Unreadable::Gone) => UsageAvailability::Gone,
                    },
                ),
            })
            .collect();
        events[0] = readable(9001, UsageAvailability::NotApplicable, event_program);
        writer
            .publish_with_identity(
                &Readings {
                    run_tick: source.tick.run_tick,
                    cpus: &source.tick.cpus,
                    memory: &source.tick.memory,
                    interfaces: &source.tick.interfaces,
                    ioreport: None,
                    smc: None,
                    processes: &source.tick.processes,
                    processes_refused: 1,
                    processes_gone: 1,
                    coalitions: &source.tick.coalitions,
                    events: &source.events,
                    dropped: source.tick.dropped,
                },
                source.tick.times.as_ref(),
                &IdentityPublication {
                    processes: &processes,
                    events: &events,
                },
            )
            .unwrap();
    }

    fn stage_identity_publish_processes(writer: &mut Writer) -> Vec<IdentityObservation> {
        let mut name = [b'x'; 4095];
        name[..4].copy_from_slice(&[0xff, b'\n', b'\\', b'\t']);
        let mut app = [b'y'; 4095];
        app[0] = b'/';
        app[4091..].copy_from_slice(b".app");
        let process_program = intern(writer, &name, Some(&app));
        let mut processes = vec![
            readable(0, UsageAvailability::Measured, process_program),
            readable(1, UsageAvailability::Refused, process_program),
            readable(2, UsageAvailability::Gone, process_program),
            source_identity(3, UsageAvailability::Measured),
        ];
        processes[3].identity = ProcessIdentity::Unavailable {
            creation: Some(ProcessCreation {
                unique_id: 99,
                pid_version: 7,
            }),
            reason: IdentityUnavailable::Changed,
        };

        processes
    }

    fn intern(
        writer: &mut Writer,
        executable_name: &[u8],
        app_path: Option<&[u8]>,
    ) -> collector::ring::ProgramReference {
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name,
                    app_path,
                },
                &mut program,
            )
            .unwrap();
        program.unwrap()
    }

    fn readable(
        pid: i32,
        usage: UsageAvailability,
        program: collector::ring::ProgramReference,
    ) -> IdentityObservation {
        IdentityObservation {
            pid,
            usage,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: u64::MAX,
                    pid_version: i32::MIN,
                },
                program,
            },
        }
    }

    fn source_identity(pid: i32, usage: UsageAvailability) -> IdentityObservation {
        IdentityObservation {
            pid,
            usage,
            identity: ProcessIdentity::Unavailable {
                creation: None,
                reason: IdentityUnavailable::Unsupported,
            },
        }
    }

    fn commit_identity(store: &Store, window: &Window, extend: impl FnOnce(&std::path::Path)) {
        let Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("fresh identity fixture was committed");
        };
        let mut tables = Encoder.encode(window, pending.path()).unwrap();
        extend(pending.path());
        encode_row_counts(pending.path(), &mut tables);
        store
            .commit(
                pending,
                Manifest {
                    identity: window.identity,
                    publication_first: 1,
                    publication_end: 2,
                    events_first: 1,
                    events_end: 6,
                    wall_ns: Some(TimeBounds {
                        min: 50_000_000_111,
                        max: 50_000_000_111,
                    }),
                    continuous_ns: Some(TimeBounds {
                        min: 100_000_000_333,
                        max: 100_000_000_333,
                    }),
                    awake_ns: Some(TimeBounds {
                        min: 80_000_000_777,
                        max: 80_000_000_777,
                    }),
                    losses: Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }
}
