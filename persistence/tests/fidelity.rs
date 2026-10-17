#[cfg(test)]
#[path = "../src/bin/recording_check/capture.rs"]
mod capture;
#[cfg(test)]
#[path = "../src/bin/recording_check/compare.rs"]
mod compare;
#[cfg(test)]
#[path = "encoder_fixture/mod.rs"]
pub mod fixture;
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
        IdentityUnavailable, ObservationTimes, Place, ProcessIdentity, Readings, RingReader,
        RunIdentity, Shape, Tick, UsageAvailability, Writer,
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
    fn refuses_identity_sources_before_recording_a_numeric_only_reference() {
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
            Err(persistence::Error::Bound("identity source preservation"))
        ));
        assert!(!reference.try_exists().unwrap());
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

        commit_identity(&store, &window);

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
        let source = persistence::query::parquet_source(&original).unwrap();
        let target = persistence::query::parquet_source(&extended).unwrap();

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
                entries: 1,
                arena_bytes: 32,
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
        source.tick.processes_refused = 0;
        source.tick.processes_gone = 0;
        stage_identity_publish(&mut writer, &source);
        write_reference(reference, &identity.run, &[source]);
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
        let processes: Vec<_> = source
            .tick
            .processes
            .iter()
            .map(|process| source_identity(process.pid, UsageAvailability::Measured))
            .collect();
        let events: Vec<_> = source
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
                    processes_refused: 0,
                    processes_gone: 0,
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

    fn commit_identity(store: &Store, window: &Window) {
        let Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("fresh identity fixture was committed");
        };
        let tables = Encoder.encode(window, pending.path()).unwrap();
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
