#[cfg(test)]
mod query_support;

#[cfg(test)]
mod tests {
    use collector::machine::Memory;
    use collector::processes::{Process, Task, Usage};
    use collector::ring::{
        DictionaryCapacity, Dropped, Found, FoundEvent, Identity, IdentityObservation,
        IdentityPublication, IdentityUnavailable, Place, ProcessCreation, ProcessIdentity,
        ProgramBytes, ProgramReference, Readings, RingReader, RunIdentity, Shape,
        UsageAvailability, Writer,
    };
    use collector::tracker::Event;
    use persistence::{Encoder, IdentitySource, Limits, Observation, Window, WindowIdentity};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const SHAPE: Shape = Shape {
        slot_count: 2,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 4,
        coalition_capacity: 1,
        event_capacity: 4,
    };

    struct Fixture {
        directory: PathBuf,
        place: Place,
        writer: Option<Writer>,
        store: persistence::Store,
        run: RunIdentity,
        shape: Shape,
    }

    impl Fixture {
        fn new() -> Self {
            Self::new_with_shape(SHAPE)
        }

        fn new_with_shape(shape: Shape) -> Self {
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir()
                .join(format!("identity-encoder-{}-{number}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            let place = Place {
                lock_path: directory.join("lock"),
                name_prefix: "fdidenc".into(),
            };
            let store = persistence::Store::open(&directory.join("recording")).unwrap();
            let run = RunIdentity {
                recording: store.recording(),
                ..run()
            };
            let writer = Writer::create_with_identity(
                &place,
                shape,
                DictionaryCapacity {
                    entries: 4,
                    arena_bytes: 32768,
                },
                &run,
                0,
            )
            .unwrap();
            Self {
                directory,
                place,
                writer: Some(writer),
                store,
                run,
                shape,
            }
        }

        fn intern(&mut self, name: &[u8], app: Option<&[u8]>) -> ProgramReference {
            let mut program = None;
            self.writer
                .as_mut()
                .unwrap()
                .intern_program(
                    &ProgramBytes {
                        executable_name: name,
                        app_path: app,
                    },
                    &mut program,
                )
                .unwrap();
            program.unwrap()
        }

        fn publish_refused(&mut self, run_tick: u64) {
            let program = self.intern(b"helper\xff", Some(b"/Apps/Outer.app"));
            let identity = measured_identity(1, UsageAvailability::Refused, program);
            let events = if run_tick == 0 {
                vec![Event::Start {
                    pid: 1,
                    coalition: None,
                }]
            } else {
                Vec::new()
            };
            let event_identity = if run_tick == 0 {
                vec![IdentityObservation {
                    usage: UsageAvailability::NotApplicable,
                    ..identity
                }]
            } else {
                Vec::new()
            };
            self.publish(run_tick, &[], &[identity], &events, &event_identity);
        }

        fn publish(
            &mut self,
            run_tick: u64,
            processes: &[Process],
            identities: &[IdentityObservation],
            events: &[Event],
            event_identities: &[IdentityObservation],
        ) {
            let refused = identities
                .iter()
                .filter(|identity| identity.usage == UsageAvailability::Refused)
                .count();
            let gone = identities
                .iter()
                .filter(|identity| identity.usage == UsageAvailability::Gone)
                .count();
            self.writer
                .as_mut()
                .unwrap()
                .publish_with_identity(
                    &Readings {
                        run_tick,
                        cpus: &[],
                        memory: &Memory::default(),
                        interfaces: &[],
                        ioreport: None,
                        smc: None,
                        processes,
                        processes_refused: u32::try_from(refused).unwrap(),
                        processes_gone: u32::try_from(gone).unwrap(),
                        coalitions: &[],
                        events,
                        dropped: Dropped::default(),
                    },
                    None,
                    &IdentityPublication {
                        processes: identities,
                        events: event_identities,
                    },
                )
                .unwrap();
        }

        fn publish_mixed(&mut self) {
            let refused = self.intern(b"refused", Some(b"/Refused.app"));
            let measured = self.intern(b"measured", Some(b"/Measured.app"));
            let gone = self.intern(b"gone", Some(b"/Gone.app"));
            let second = self.intern(b"second", Some(b"/Second.app"));
            let identities = [
                mixed_identity(1, UsageAvailability::Refused, refused),
                mixed_identity(7, UsageAvailability::Measured, measured),
                mixed_identity(8, UsageAvailability::Gone, gone),
                mixed_identity(9, UsageAvailability::Measured, second),
            ];
            let mut second = process();
            second.pid = 9;
            second.usage.user_ns = 222;
            second.usage.energy_nj = 444;
            self.publish(0, &[process(), second], &identities, &[], &[]);
        }

        fn window(&self, number: u64) -> Window {
            Window::new(
                WindowIdentity {
                    run: self.run,
                    window: number,
                },
                Limits::from_shape(&self.shape).unwrap(),
            )
        }

        fn retire(&mut self) {
            let writer = self.writer.take().unwrap();
            let name = writer.name().to_owned();
            drop(writer);
            collector::ring::unlink_generation(&name).unwrap();
        }

        fn stage(&self, reader: &mut RingReader, window: &mut Window, publication: u64) {
            assert_eq!(reader.identity(), Some(self.run));
            let mut tick = persistence::tick_buffer(&self.shape).unwrap();
            let mut identities =
                Vec::with_capacity(usize::try_from(self.shape.process_capacity).unwrap());
            assert!(matches!(
                reader
                    .read_identity_into(&mut tick, &mut identities)
                    .unwrap(),
                Found::Publication { .. }
            ));
            let mut events = Vec::new();
            let mut event_identities = Vec::new();
            for _index in 0..4 {
                let mut identity = IdentityObservation::EMPTY;
                match reader.read_event_identity(&mut identity).unwrap() {
                    FoundEvent::Event {
                        number,
                        tick,
                        event,
                    } => {
                        events.push((number, tick, event));
                        event_identities.push(identity);
                    }
                    FoundEvent::Nothing => break,
                    FoundEvent::Missed { .. } => panic!("fixture event lost"),
                }
            }
            let numbered: Vec<_> = events
                .iter()
                .map(|(number, tick, event)| persistence::NumberedEvent {
                    number: *number,
                    tick: *tick,
                    event,
                })
                .collect();
            window
                .push_tick_with_identity(
                    &Observation {
                        publication,
                        wall_ns: Some(100_000_000_000 + tick.run_tick * 1_000_000_000),
                        continuous_ns: None,
                        awake_ns: None,
                    },
                    &tick,
                    &numbered,
                    &IdentitySource {
                        reader,
                        processes: &identities,
                        events: &event_identities,
                    },
                )
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.writer.is_some() {
                self.retire();
            }
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn run() -> RunIdentity {
        RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        }
    }

    fn measured_identity(
        pid: i32,
        usage: UsageAvailability,
        program: ProgramReference,
    ) -> IdentityObservation {
        IdentityObservation {
            pid,
            usage,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: 0,
                    pid_version: -7,
                },
                program,
            },
        }
    }

    #[test]
    fn preserves_refused_usage_identity_in_a_later_self_contained_window() {
        // Goal: unavailable usage retains independently readable identity after ring retirement.
        // Method: stage two real v4 publications, retire the ring, encode only the second.
        let mut fixture = Fixture::new();
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut first = fixture.window(0);
        fixture.publish_refused(0);
        fixture.stage(&mut reader, &mut first, 1);
        let mut second = fixture.window(1);
        fixture.publish_refused(60);
        fixture.stage(&mut reader, &mut second, 2);
        drop(reader);
        fixture.retire();

        let (connection, source, events) = encoded(&fixture, &second);

        assert_refused_process(&connection, &source);
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {events}"), [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        let (first_connection, _processes, first_events) = encoded(&fixture, &first);
        assert!(
            first_connection
                .query_row(
                    &format!(
                        "SELECT kind=1 AND identity_version=1 \
            AND executable_name=? FROM {first_events}"
                    ),
                    duckdb::params![b"helper\xff".to_vec()],
                    |row| row.get::<_, bool>(0)
                )
                .unwrap()
        );
        commit_second(&fixture, &second);
        let reopened =
            persistence::query::processes_at_second(&fixture.directory.join("recording"), 160)
                .unwrap();
        assert!(reopened.processes.is_empty());
    }

    fn assert_refused_process(connection: &duckdb::Connection, source: &str) {
        let null_payload = persistence::schema::Table::Processes
            .columns()
            .skip(2)
            .map(|column| format!("{column} IS NULL"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let valid: bool = connection
            .query_row(
                &format!(
                    "SELECT publication=2 AND pid=1 \
            AND identity_version=1 AND identity_availability=0 AND usage_availability=1 \
            AND process_unique_id=0 AND pid_version=-7 AND executable_name=? AND app_path=? \
            AND {null_payload} FROM {source}"
                ),
                duckdb::params![b"helper\xff".to_vec(), b"/Apps/Outer.app".to_vec()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(valid);
    }

    fn commit_second(fixture: &Fixture, second: &Window) {
        let pending = match fixture.store.begin(&second.identity).unwrap() {
            persistence::Begin::Pending(pending) => pending,
            persistence::Begin::Committed(_) => panic!("new window"),
        };
        let tables = Encoder.encode(second, pending.path()).unwrap();
        let manifest = persistence::Manifest {
            identity: second.identity,
            publication_first: 2,
            publication_end: 3,
            events_first: second.event_range().start,
            events_end: second.event_range().end,
            wall_ns: Some(persistence::TimeBounds {
                min: 160_000_000_000,
                max: 160_000_000_000,
            }),
            continuous_ns: None,
            awake_ns: None,
            losses: persistence::Losses::default(),
            data_directory: String::new(),
            tables,
        };
        fixture.store.commit(pending, manifest).unwrap();
    }

    fn encoded(fixture: &Fixture, window: &Window) -> (duckdb::Connection, String, String) {
        let pending = fixture
            .directory
            .join(format!("encoded-{}", window.identity.window));
        std::fs::create_dir(&pending).unwrap();
        Encoder.encode(window, &pending).unwrap();
        let connection = duckdb::Connection::open_in_memory().unwrap();
        let source = |table| {
            super::query_support::table_source(
                &connection,
                table,
                persistence::query::TableSource::Parquet(
                    &pending.join(format!("{}.parquet", table.name())),
                ),
            )
            .unwrap()
        };
        let processes = source(persistence::schema::Table::Processes);
        let events = source(persistence::schema::Table::Events);
        (connection, processes, events)
    }

    fn process() -> Process {
        Process {
            pid: 7,
            usage: Usage {
                awake_ns_at_start: 0,
                user_ns: 9_007_199_254_740_993,
                system_ns: u64::MAX,
                energy_nj: u64::MAX - 1,
                instructions: 1,
                super_instructions: 2,
                cycles: 3,
                idle_wakeups: 4,
                interrupt_wakeups: 5,
                pageins: 6,
                resident_bytes: 7,
                footprint_bytes: 8,
                disk_read_bytes: 9,
                disk_written_bytes: 10,
            },
            task: Task {
                threads: i32::MIN,
                faults: i32::MAX,
                context_switches: -1,
                mach_syscalls: 0,
                unix_syscalls: 1,
            },
        }
    }

    fn unavailable(
        pid: i32,
        usage: UsageAvailability,
        reason: IdentityUnavailable,
    ) -> IdentityObservation {
        IdentityObservation {
            pid,
            usage,
            identity: ProcessIdentity::Unavailable {
                creation: None,
                reason,
            },
        }
    }

    #[test]
    fn preserves_one_exact_measured_row_when_identity_is_refused() {
        // Goal: failed identity does not duplicate or round original measured counters.
        // Method: publish wide integer values through v4 and query the encoded numeric row.
        let mut fixture = Fixture::new();
        let identity = unavailable(7, UsageAvailability::Measured, IdentityUnavailable::Refused);
        fixture.publish(0, &[process()], &[identity], &[], &[]);
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        fixture.stage(&mut reader, &mut window, 1);

        let (connection, processes, _events) = encoded(&fixture, &window);

        let actual: (u64, u64, u64, u64, i32, i32, u8, u8, u8) = connection
            .query_row(
                &format!(
                    "SELECT count(*) OVER(), user_ns, system_ns, energy_nj, threads, faults, \
                identity_version, identity_availability, usage_availability FROM {processes}"
                ),
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            actual,
            (
                1,
                9_007_199_254_740_993,
                u64::MAX,
                u64::MAX - 1,
                i32::MIN,
                i32::MAX,
                1,
                1,
                0
            )
        );
        let complete: bool = connection
            .query_row(
                &format!(
                    "SELECT publication=1 AND pid=7 \
            AND awake_ns_at_start=0 AND instructions=1 AND super_instructions=2 AND cycles=3 \
            AND idle_wakeups=4 AND interrupt_wakeups=5 AND pageins=6 AND resident_bytes=7 \
            AND footprint_bytes=8 AND disk_read_bytes=9 AND disk_written_bytes=10 \
            AND context_switches=-1 AND mach_syscalls=0 AND unix_syscalls=1 \
            FROM {processes}"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(complete);
    }

    #[test]
    fn preserves_refused_exit_usage_independently_of_gone_identity() {
        // Goal: event availability and identity availability retain separate unknown reasons.
        // Method: publish a refused Exit with unavailable identity, then normalize the file.
        let mut fixture = Fixture::new();
        let event = Event::Exit {
            pid: 7,
            last: Err(collector::processes::Unreadable::Refused),
            noticed: collector::tracker::Noticed::AlreadyExited,
        };
        fixture.publish(
            0,
            &[],
            &[],
            &[event],
            &[unavailable(
                7,
                UsageAvailability::Refused,
                IdentityUnavailable::Gone,
            )],
        );
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        fixture.stage(&mut reader, &mut window, 1);

        let (connection, _processes, events) = encoded(&fixture, &window);

        let valid: bool = connection
            .query_row(
                &format!(
                    "SELECT kind=3 AND noticed=1 \
            AND availability=1 AND identity_version=1 AND identity_availability=2 \
            AND usage_availability=1 AND user_ns IS NULL AND executable_name IS NULL \
            AND process_unique_id IS NULL FROM {events}"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(valid);
    }

    #[test]
    fn preserves_gone_identity_independently_of_measured_usage() {
        // Goal: gone metadata stays distinct from refused metadata and measured zero.
        // Method: round-trip a typed unavailable observation with a numeric process.
        let mut fixture = Fixture::new();
        fixture.publish(
            0,
            &[process()],
            &[unavailable(
                7,
                UsageAvailability::Measured,
                IdentityUnavailable::Gone,
            )],
            &[],
            &[],
        );
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        fixture.stage(&mut reader, &mut window, 1);

        let (connection, processes, _events) = encoded(&fixture, &window);

        assert_eq!(
            connection
                .query_row(
                    &format!(
                        "SELECT identity_availability, \
            usage_availability FROM {processes}"
                    ),
                    [],
                    |row| Ok((row.get::<_, u8>(0)?, row.get::<_, u8>(1)?))
                )
                .unwrap(),
            (2, 0)
        );
    }

    #[test]
    fn preserves_unsupported_identity_with_gone_usage() {
        // Goal: no numeric row is invented for usage gone with unsupported identity.
        // Method: publish an identity-only observation and validate normalized SQL NULLs.
        let mut fixture = Fixture::new();
        fixture.publish(
            0,
            &[],
            &[unavailable(
                7,
                UsageAvailability::Gone,
                IdentityUnavailable::Unsupported,
            )],
            &[],
            &[],
        );
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        fixture.stage(&mut reader, &mut window, 1);

        let (connection, processes, _events) = encoded(&fixture, &window);

        assert_eq!(
            connection
                .query_row(
                    &format!(
                        "SELECT identity_version, \
            identity_availability, usage_availability, user_ns FROM {processes}"
                    ),
                    [],
                    |row| Ok((
                        row.get::<_, u8>(0)?,
                        row.get::<_, u8>(1)?,
                        row.get::<_, u8>(2)?,
                        row.get::<_, Option<u64>>(3)?
                    ))
                )
                .unwrap(),
            (1, 3, 2, None)
        );
    }

    #[test]
    fn preserves_each_events_independent_program_and_creation() {
        // Goal: event identity is not reconstructed from its same-PID process observation.
        // Method: encode a process and Exit with different captured programs and creations.
        let mut fixture = Fixture::new();
        let current = fixture.intern(b"current", None);
        let departed = fixture.intern(b"departed\xfe", Some(b"/Old.app"));
        let process_identity = measured_identity(7, UsageAvailability::Measured, current);
        let event_identity = IdentityObservation {
            pid: 7,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: u64::MAX,
                    pid_version: i32::MIN,
                },
                program: departed,
            },
        };
        let event = Event::Exit {
            pid: 7,
            last: Ok(process().usage),
            noticed: collector::tracker::Noticed::OnExit,
        };
        fixture.publish(
            0,
            &[process()],
            &[process_identity],
            &[event],
            &[event_identity],
        );
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        fixture.stage(&mut reader, &mut window, 1);
        drop(reader);
        fixture.retire();

        let (connection, processes, events) = encoded(&fixture, &window);

        let valid: bool = connection
            .query_row(
                &format!(
                    "SELECT availability=0 \
            AND identity_version=1 AND usage_availability=0 AND process_unique_id=? \
            AND pid_version=? AND executable_name=? AND app_path=? AND user_ns=? FROM {events}"
                ),
                duckdb::params![
                    u64::MAX,
                    i32::MIN,
                    b"departed\xfe".to_vec(),
                    b"/Old.app".to_vec(),
                    9_007_199_254_740_993_u64
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert!(valid);
        assert_eq!(
            connection
                .query_row(
                    &format!("SELECT executable_name FROM {processes}"),
                    [],
                    |row| row.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
            b"current"
        );
    }

    #[test]
    fn refuses_another_readers_reference_without_changing_an_accepted_window() {
        // Goal: references cannot be copied through a reopened handle or damage accepted data.
        // Method: reject the second publication under another reader, then retry the owner.
        let mut fixture = Fixture::new();
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        fixture.publish_refused(0);
        fixture.stage(&mut reader, &mut window, 1);
        fixture.publish_refused(1);
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(4);
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        let other = RingReader::open(&fixture.place.lock_path).unwrap();
        let before = window.staged_bytes();
        let observation = Observation {
            publication: 2,
            wall_ns: None,
            continuous_ns: None,
            awake_ns: None,
        };

        let result = window.push_tick_with_identity(
            &observation,
            &tick,
            &[],
            &IdentitySource {
                reader: &other,
                processes: &identities,
                events: &[],
            },
        );

        assert!(matches!(result, Err(persistence::Error::Io(_))));
        assert_eq!(window.tick_count(), 1);
        assert_eq!(window.staged_bytes(), before);
        window
            .push_tick_with_identity(
                &observation,
                &tick,
                &[],
                &IdentitySource {
                    reader: &reader,
                    processes: &identities,
                    events: &[],
                },
            )
            .unwrap();
        assert_eq!(window.tick_count(), 2);
    }

    #[test]
    fn deduplicates_maximum_length_programs_across_a_full_window_and_refuses_tick_overflow() {
        // Goal: long paths stay owned once, and a full window refuses before mutation.
        // Method: stage 61 real publications sharing one program and retry publication 62.
        let mut fixture = Fixture::new();
        let name = vec![b'x'; 4095];
        let mut app = vec![b'a'; 4095];
        app[0] = b'/';
        app[4091..].copy_from_slice(b".app");
        let program = fixture.intern(&name, Some(&app));
        let identities = [
            measured_identity(1, UsageAvailability::Refused, program),
            measured_identity(2, UsageAvailability::Gone, program),
        ];
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        let started = std::time::Instant::now();
        for index in 0..61 {
            fixture.publish(index, &[], &identities, &[], &[]);
            fixture.stage(&mut reader, &mut window, index + 1);
        }
        let elapsed = started.elapsed();
        let usage = window.identity_program_usage().unwrap();
        let bytes = window.staged_bytes();
        fixture.publish(61, &[], &identities, &[], &[]);
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut read_identities = Vec::with_capacity(4);
        reader
            .read_identity_into(&mut tick, &mut read_identities)
            .unwrap();

        let result = window.push_tick_with_identity(
            &Observation {
                publication: 62,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
            },
            &tick,
            &[],
            &IdentitySource {
                reader: &reader,
                processes: &read_identities,
                events: &[],
            },
        );

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("window ticks"))
        ));
        assert_eq!(window.tick_count(), 61);
        assert_eq!(window.staged_bytes(), bytes);
        assert_eq!(usage.entries, 1);
        assert_eq!(usage.arena_bytes, 8190);
        eprintln!(
            "repeated_program_ticks=61 entries={} raw_bytes={} retained_bytes={} elapsed_ns={}",
            usage.entries,
            usage.arena_bytes,
            bytes,
            elapsed.as_nanos()
        );
    }
    #[test]
    fn refuses_a_measured_identity_without_its_numeric_process() {
        // Goal: measured identity cannot silently become an identity-only numeric row.
        // Method: change a decoded refused observation to measured and inspect refusal.
        let mut fixture = Fixture::new();
        fixture.publish_refused(1);
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(4);
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        identities[0].usage = UsageAvailability::Measured;
        let mut window = fixture.window(0);
        let before = window.staged_bytes();

        let result = window.push_tick_with_identity(
            &Observation {
                publication: 1,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
            },
            &tick,
            &[],
            &IdentitySource {
                reader: &reader,
                processes: &identities,
                events: &[],
            },
        );

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("identity measured process"))
        ));
        assert_eq!(window.tick_count(), 0);
        assert_eq!(window.staged_bytes(), before);
        assert!(window.identity_program_usage().is_none());
    }

    #[test]
    fn refuses_identity_rows_past_the_source_process_capacity() {
        // Goal: an oversized identity section fails without creating the owned dictionary.
        // Method: duplicate a decoded row beyond the validated source capacity.
        let mut fixture = Fixture::new();
        fixture.publish_refused(1);
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(4);
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        let oversized = vec![identities[0]; 5];
        let mut window = fixture.window(0);
        let before = window.staged_bytes();

        let result = window.push_tick_with_identity(
            &Observation {
                publication: 1,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
            },
            &tick,
            &[],
            &IdentitySource {
                reader: &reader,
                processes: &oversized,
                events: &[],
            },
        );

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("identity process rows"))
        ));
        assert_eq!(window.tick_count(), 0);
        assert_eq!(window.staged_bytes(), before);
        assert!(window.identity_program_usage().is_none());
    }
    #[test]
    fn refuses_the_first_publication_past_the_saved_identity_row_bound() {
        // Goal: every admitted identity table fits the reader's existing row limit.
        // Method: stage 16 maximum-capacity publications, reject the 17th, and reopen the 16.
        let shape = Shape {
            process_capacity: 65_536,
            ..SHAPE
        };
        let mut fixture = Fixture::new_with_shape(shape);
        let program = fixture.intern(b"boundary", None);
        let identities: Vec<_> = (0..65_536)
            .map(|pid| measured_identity(pid, UsageAvailability::Refused, program))
            .collect();
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        for index in 0..16 {
            fixture.publish(index, &[], &identities, &[], &[]);
            fixture.stage(&mut reader, &mut window, index + 1);
        }
        let before = window.staged_bytes();
        fixture.publish(16, &[], &identities, &[], &[]);
        let mut tick = persistence::tick_buffer(&shape).unwrap();
        let mut decoded = Vec::with_capacity(65_536);
        reader.read_identity_into(&mut tick, &mut decoded).unwrap();

        let result = window.push_tick_with_identity(
            &Observation {
                publication: 17,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
            },
            &tick,
            &[],
            &IdentitySource {
                reader: &reader,
                processes: &decoded,
                events: &[],
            },
        );

        assert!(matches!(
            result,
            Err(persistence::Error::Bound("process identity rows"))
        ));
        assert_eq!(window.tick_count(), 16);
        assert_eq!(window.staged_bytes(), before);
        drop(reader);
        fixture.retire();
        let (connection, processes, _events) = encoded(&fixture, &window);
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {processes}"), [], |row| row
                    .get::<_, u64>(
                    0
                ))
                .unwrap(),
            1_048_576
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct MixedProcess {
        pid: i32,
        usage: u8,
        unique: u64,
        version: i32,
        name: Vec<u8>,
        app: Vec<u8>,
        user: Option<u64>,
        energy: Option<u64>,
        payload_absent: bool,
    }

    fn mixed_identity(
        pid: i32,
        usage: UsageAvailability,
        program: ProgramReference,
    ) -> IdentityObservation {
        IdentityObservation {
            pid,
            usage,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: 100 + u64::try_from(pid).unwrap(),
                    pid_version: -pid,
                },
                program,
            },
        }
    }

    fn mixed_rows(connection: &duckdb::Connection, source: &str) -> Vec<MixedProcess> {
        let nulls = persistence::schema::Table::Processes
            .columns()
            .skip(2)
            .map(|column| format!("{column} IS NULL"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut statement = connection
            .prepare(&format!(
                "SELECT pid, usage_availability, \
            process_unique_id, pid_version, executable_name, app_path, user_ns, energy_nj, \
            ({nulls}) FROM {source} WHERE identity_version=1 AND identity_availability=0 \
            ORDER BY pid"
            ))
            .unwrap();
        statement
            .query_map([], |row| {
                Ok(MixedProcess {
                    pid: row.get(0)?,
                    usage: row.get(1)?,
                    unique: row.get(2)?,
                    version: row.get(3)?,
                    name: row.get(4)?,
                    app: row.get(5)?,
                    user: row.get(6)?,
                    energy: row.get(7)?,
                    payload_absent: row.get(8)?,
                })
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap()
    }

    fn expected_mixed_rows() -> Vec<MixedProcess> {
        vec![
            MixedProcess {
                pid: 1,
                usage: 1,
                unique: 101,
                version: -1,
                name: b"refused".to_vec(),
                app: b"/Refused.app".to_vec(),
                user: None,
                energy: None,
                payload_absent: true,
            },
            MixedProcess {
                pid: 7,
                usage: 0,
                unique: 107,
                version: -7,
                name: b"measured".to_vec(),
                app: b"/Measured.app".to_vec(),
                user: Some(9_007_199_254_740_993),
                energy: Some(u64::MAX - 1),
                payload_absent: false,
            },
            MixedProcess {
                pid: 8,
                usage: 2,
                unique: 108,
                version: -8,
                name: b"gone".to_vec(),
                app: b"/Gone.app".to_vec(),
                user: None,
                energy: None,
                payload_absent: true,
            },
            MixedProcess {
                pid: 9,
                usage: 0,
                unique: 109,
                version: -9,
                name: b"second".to_vec(),
                app: b"/Second.app".to_vec(),
                user: Some(222),
                energy: Some(444),
                payload_absent: false,
            },
        ]
    }

    #[test]
    fn preserves_ordered_mixed_usage_rows_and_their_own_creations_and_programs() {
        // Goal: numeric indexing skips refused/gone rows without changing their metadata.
        // Method: round-trip one ordered v4 publication with four distinct captured programs.
        let mut fixture = Fixture::new();
        fixture.publish_mixed();
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut window = fixture.window(0);
        fixture.stage(&mut reader, &mut window, 1);
        drop(reader);
        fixture.retire();

        let (connection, processes, _events) = encoded(&fixture, &window);

        assert_eq!(mixed_rows(&connection, &processes), expected_mixed_rows());
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {processes}"), [], |row| row
                    .get::<_, u64>(
                    0
                ))
                .unwrap(),
            4
        );
    }
}
