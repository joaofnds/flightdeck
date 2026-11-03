use super::*;
use crate::{Limits, Manifest, Observation, Window, WindowIdentity};

fn encode_payload(
    attempt: &RegisteredAttempt<'_>,
    fixture: &Fixture,
    publication: u64,
    omit_event: bool,
) -> Manifest {
    let input = attempt.input();
    let AttemptOperation::Ordinary { window, .. } = input.operation else {
        panic!("ordinary");
    };
    let identity = WindowIdentity {
        run: input.run,
        window,
    };
    let mut reader = fixture.reader();
    let tick = encode_payload_tick(&mut reader, publication);
    let observation = original_observation(publication, &tick);
    let mut staged = Window::new(identity, Limits::from_shape(&reader.shape()).unwrap());
    staged.push_tick(&observation, &tick, &[]).unwrap();
    let mut tables = encode_original_tables(attempt, &staged);
    let mut rows = encode_original_rows(attempt, &mut reader, &tick);
    if omit_event {
        rows.retain(|row| row.event.number != 1);
    }
    let index = encode_original_index(attempt, &rows, &tick, identity);
    tables
        .iter_mut()
        .find(|table| table.name == "events")
        .unwrap()
        .rows = index.rows;
    Manifest {
        identity,
        publication_first: publication,
        publication_end: publication + 1,
        events_first: index.first,
        events_end: index.end,
        wall_ns: observation.wall_ns.map(|value| crate::TimeBounds {
            min: value,
            max: value,
        }),
        continuous_ns: observation.continuous_ns.map(|value| crate::TimeBounds {
            min: value,
            max: value,
        }),
        awake_ns: observation.awake_ns.map(|value| crate::TimeBounds {
            min: value,
            max: value,
        }),
        losses: crate::Losses {
            publications: publication - input.losses.publication_start,
            events: index.end - index.first - index.rows,
            unknown: input.losses.unknown.is_some(),
        },
        data_directory: format!(
            "data/{}/ready",
            collector::recording::identity_text(attempt.transaction())
        ),
        tables,
        event_batches: Some(u32::try_from(index.batches.len()).unwrap()),
    }
}

fn encode_payload_tick(reader: &mut RingReader, publication: u64) -> collector::ring::Tick {
    reader.seek_publication(publication).unwrap();
    let shape = reader.shape();
    let mut tick = crate::tick_buffer(&shape).unwrap();
    let found = reader.read_into(&mut tick).unwrap();
    match found {
        collector::ring::Found::Publication {
            publication: number,
            ..
        } => assert_eq!(number, publication),
        other => panic!("expected publication: {other:?}"),
    }
    tick
}

fn encode_original_tables(
    attempt: &RegisteredAttempt<'_>,
    staged: &Window,
) -> Vec<crate::TableManifest> {
    let mut tables = Vec::new();
    for table in crate::schema::TABLES {
        if matches!(table, crate::schema::Table::Events) {
            tables.push(crate::TableManifest {
                name: "events",
                rows: 0,
            });
            continue;
        }
        let mut frame = crate::frame(table, staged).unwrap();
        tables.push(crate::TableManifest {
            name: table.name(),
            rows: collector::u64_from_usize(frame.height()),
        });
        crate::encoder::encode_write(
            &mut frame,
            attempt
                .create_file(&format!("{}.parquet", table.name()))
                .unwrap(),
        )
        .unwrap();
    }
    tables
}

fn encode_original_rows(
    attempt: &RegisteredAttempt<'_>,
    reader: &mut RingReader,
    tick: &collector::ring::Tick,
) -> Vec<crate::event_recovery::batch::Row> {
    let input = attempt.input();
    let shape = reader.shape();
    let publication = reader.next_publication() - 1;
    let mut rows = Vec::with_capacity(usize::try_from(shape.event_capacity).unwrap());
    reader.seek_event(input.losses.event_start).unwrap();
    for _ in 0..shape.event_capacity {
        if reader.next_event() >= tick.events.end {
            break;
        }
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if input.captured {
            reader.read_event_identity(&mut identity).unwrap()
        } else {
            reader.read_event().unwrap()
        };
        match found {
            collector::ring::FoundEvent::Event {
                number,
                tick: run_tick,
                event,
            } => {
                let mut row = crate::event_recovery::batch::copy_row(
                    reader,
                    &crate::NumberedEvent {
                        number,
                        tick: run_tick,
                        event: &event,
                    },
                    input.captured.then_some(&identity),
                )
                .unwrap();
                row.publication = if number >= tick.events.start {
                    Some(publication)
                } else {
                    None
                };
                rows.push(row);
            }
            collector::ring::FoundEvent::Missed { .. } => {}
            collector::ring::FoundEvent::Nothing => break,
        }
    }
    rows
}

fn encode_original_index(
    attempt: &RegisteredAttempt<'_>,
    rows: &[crate::event_recovery::batch::Row],
    tick: &collector::ring::Tick,
    identity: WindowIdentity,
) -> crate::event_table::index::Index {
    let input = attempt.input();
    let batches = if rows.is_empty() {
        vec![]
    } else {
        let mut frame = crate::event_recovery::batch::frame(rows, input.captured).unwrap();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(0).unwrap()).unwrap();
        vec![crate::event_recovery::Batch {
            first: rows[0].event.number,
            end: rows.last().unwrap().event.number + 1,
            rows: collector::u64_from_usize(rows.len()),
        }]
    };
    let index = crate::event_table::index::Index {
        identity,
        first: input.losses.event_start,
        end: tick.events.end,
        rows: collector::u64_from_usize(rows.len()),
        identities: input.captured,
        batches,
    };
    index
        .write_file(&mut attempt.create_event_index().unwrap())
        .unwrap();
    index
}

fn original_observation(publication: u64, tick: &collector::ring::Tick) -> Observation {
    Observation {
        publication,
        wall_ns: tick.times.map(|times| times.wall_ns),
        continuous_ns: tick.times.map(|times| times.continuous_ns),
        awake_ns: tick.times.map(|times| times.awake_ns),
    }
}

fn publish_at(writer: &mut collector::ring::Writer, window: u64) {
    let memory = collector::machine::Memory::default();
    let readings = collector::ring::Readings {
        run_tick: window * 60,
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
        dropped: collector::ring::Dropped::default(),
    };
    let times = collector::ring::ObservationTimes {
        wall_ns: window * 60 + 1,
        continuous_ns: window * 60_000_000_000 + 1,
        awake_ns: window * 60_000_000_000 + 1,
    };
    if writer.dictionary_usage().unwrap().is_some() {
        writer
            .publish_with_identity(
                &readings,
                Some(&times),
                &collector::ring::IdentityPublication {
                    processes: &[],
                    events: &[],
                },
            )
            .unwrap();
    } else {
        writer.publish_observed(&readings, &times);
    }
}

fn publish_captured_exec(fixture: &mut Fixture) {
    publish_captured_pair_at(fixture, 2, 2);
}
fn publish_captured_pair_at(fixture: &mut Fixture, run_tick: u64, continuous_ns: u64) {
    let mut reference = None;
    fixture
        .writer
        .intern_program(
            &collector::ring::ProgramBytes {
                executable_name: b"program-\xff",
                app_path: Some(b"/Applications/Binary-\xfe.app"),
            },
            &mut reference,
        )
        .unwrap();
    let identity = collector::ring::IdentityObservation {
        pid: 41,
        usage: collector::ring::UsageAvailability::NotApplicable,
        identity: collector::ring::ProcessIdentity::Measured {
            creation: collector::ring::ProcessCreation {
                unique_id: 991,
                pid_version: 7,
            },
            program: reference.unwrap(),
        },
    };
    let memory = collector::machine::Memory::default();
    fixture
        .writer
        .publish_with_identity(
            &collector::ring::Readings {
                run_tick,
                cpus: &[],
                memory: &memory,
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[
                    collector::tracker::Event::Exec { pid: 41 },
                    collector::tracker::Event::Exec { pid: 41 },
                ],
                dropped: collector::ring::Dropped::default(),
            },
            Some(&collector::ring::ObservationTimes {
                wall_ns: continuous_ns,
                continuous_ns,
                awake_ns: continuous_ns,
            }),
            &collector::ring::IdentityPublication {
                processes: &[],
                events: &[identity, identity],
            },
        )
        .unwrap();
}

fn proof_fixture(captured: bool, slots: u64) -> Fixture {
    let mut fixture = Fixture::new_profile(false, captured);
    let place = collector::ring::Place {
        lock_path: fixture.base.join("proof-source.lock"),
        name_prefix: format!(
            "/fdp{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let mut shape = fixture.reader().shape();
    shape.slot_count = slots;
    shape.event_capacity = 4;
    let writer = if captured {
        collector::ring::Writer::create_with_identity(
            &place,
            shape,
            collector::ring::DictionaryCapacity {
                entries: 4,
                arena_bytes: 16 * 1024,
            },
            &fixture.run,
            1,
        )
        .unwrap()
    } else {
        collector::ring::Writer::create_recording(&place, shape, &fixture.run, 1).unwrap()
    };
    let old = fixture.writer.name().to_owned();
    fixture.writer = writer;
    collector::ring::unlink_generation(&old).unwrap();
    fixture.place = place;
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    publish_at(&mut fixture.writer, 0);
    fixture
}
fn omission_of_a_retained_publication_refuses_metadata_in_both_profiles(captured: bool) {
    // Goal: a retained omitted publication cannot become an exact loss.
    // Method: register1 with four slots, encode only2 and close the window with3.
    let mut fixture = proof_fixture(captured, 4);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    publish_at(&mut fixture.writer, 0);
    let manifest = encode_payload(&attempt, &fixture, 2, false);
    publish_at(&mut fixture.writer, 1);
    let before = fixture.slot();
    let path = metadata_path(&fixture, &attempt);
    let result = issue(attempt, &manifest, fixture.reader(), &budget);
    assert!(
        result.is_err(),
        "readable publication1 was certified lost; captured={captured}"
    );
    assert_eq!(fixture.slot(), before);
    assert_eq!(
        File::open(&path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

fn publish_numeric_event_pair(fixture: &mut Fixture) {
    publish_numeric_pair_at(fixture, 2, 2);
}
fn publish_numeric_pair_at(fixture: &mut Fixture, run_tick: u64, continuous_ns: u64) {
    let memory = collector::machine::Memory::default();
    let readings = collector::ring::Readings {
        run_tick,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[
            collector::tracker::Event::Exec { pid: 41 },
            collector::tracker::Event::Exec { pid: 41 },
        ],
        dropped: collector::ring::Dropped::default(),
    };
    fixture.writer.publish_observed(
        &readings,
        &collector::ring::ObservationTimes {
            wall_ns: continuous_ns,
            continuous_ns,
            awake_ns: continuous_ns,
        },
    );
}
fn omission_of_a_retained_event_refuses_metadata_in_both_profiles(captured: bool) {
    // Goal: ordinary event conservation cannot certify a readable omitted event as loss.
    // Method: genuinely overwrite publication1, retain both events of2, encode only event2.
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    if captured {
        publish_captured_exec(&mut fixture);
    } else {
        publish_numeric_event_pair(&mut fixture);
    }
    let manifest = encode_payload(&attempt, &fixture, 2, true);
    assert_eq!(manifest.losses.events, 1);
    publish_at(&mut fixture.writer, 1);
    let before = fixture.slot();
    let path = metadata_path(&fixture, &attempt);
    let result = issue(attempt, &manifest, fixture.reader(), &budget);
    assert!(
        result.is_err(),
        "readable event1 was certified lost; captured={captured}"
    );
    assert_eq!(fixture.slot(), before);
    assert_eq!(
        File::open(&path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn genuine_publication_overwrite_preserves_the_admitted_original_tick() {
    // Goal: loss proof permits a real overwritten prefix and retains raw rows after later wrap.
    // Method: one source slot overwrites1 before encoding2, then the next window overwrites2.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
            .unwrap();
        publish_at(&mut fixture.writer, 0);
        let manifest = encode_payload(&attempt, &fixture, 2, false);
        publish_at(&mut fixture.writer, 1);
        let mut publication = issue(attempt, &manifest, fixture.reader(), &budget).unwrap();
        let mut done = false;
        for _ in 0..512 {
            if publication.step().unwrap() == PublicationStep::Idle {
                done = true;
                break;
            }
        }
        assert!(done);
        assert_eq!(
            store
                .source_frontier(
                    &fixture.run,
                    &budget,
                    &mut crate::canonical::ReadWork::default()
                )
                .unwrap()
                .unwrap()
                .publication_end(),
            3
        );
    }
}

#[test]
fn event_rows_and_genuine_overwritten_complement_have_distinct_positive_controls() {
    // Goal: retain every admitted row even when the source later loses that same row.
    // Method: compare full event retention with an actually overwritten omitted event1.
    for captured in [false, true] {
        for overwritten in [false, true] {
            let mut fixture = proof_fixture(captured, 1);
            let store = Store::open(&fixture.root).unwrap();
            let budget = registration_budget();
            let attempt = store
                .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
                .unwrap();
            if captured {
                publish_captured_exec(&mut fixture);
            } else {
                publish_numeric_event_pair(&mut fixture);
            }
            let manifest = encode_payload(&attempt, &fixture, 2, overwritten);
            if overwritten {
                for _ in 0..2 {
                    if captured {
                        publish_captured_exec(&mut fixture);
                    } else {
                        publish_numeric_event_pair(&mut fixture);
                    }
                }
            }
            publish_at(&mut fixture.writer, 1);
            let mut publication = issue(attempt, &manifest, fixture.reader(), &budget).unwrap();
            let mut done = false;
            for _ in 0..512 {
                if publication.step().unwrap() == PublicationStep::Idle {
                    done = true;
                    break;
                }
            }
            assert!(done);
            assert_eq!(
                store
                    .source_frontier(
                        &fixture.run,
                        &budget,
                        &mut crate::canonical::ReadWork::default()
                    )
                    .unwrap()
                    .unwrap()
                    .event_end(),
                3
            );
        }
    }
}

#[test]
fn omission_publication_numeric() {
    // Goal: expose readable numeric publication loss through Store.
    // Method: run the four-slot omission regression independently of captured mode.
    omission_of_a_retained_publication_refuses_metadata_in_both_profiles(false);
}
#[test]
fn omission_publication_captured() {
    // Goal: expose readable captured publication loss through Store.
    // Method: run the four-slot omission regression with the captured producer.
    omission_of_a_retained_publication_refuses_metadata_in_both_profiles(true);
}
#[test]
fn omission_event_numeric() {
    // Goal: expose readable numeric event loss through Store.
    // Method: run the event omission regression independently of captured mode.
    omission_of_a_retained_event_refuses_metadata_in_both_profiles(false);
}
#[test]
fn omission_event_captured() {
    // Goal: expose readable captured event loss through Store.
    // Method: run the event omission regression with actual captured rows.
    omission_of_a_retained_event_refuses_metadata_in_both_profiles(true);
}

fn metadata_path(fixture: &Fixture, attempt: &RegisteredAttempt<'_>) -> PathBuf {
    fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string())
        .join("pending/metadata")
}
fn issue<'store>(
    attempt: RegisteredAttempt<'store>,
    manifest: &Manifest,
    reader: RingReader,
    budget: &Arc<crate::resources::ByteBudget>,
) -> io::Result<NormalPublication<'store>> {
    let mut proof = attempt.prove_ordinary(manifest, reader, budget)?;
    for _ in 0..10000 {
        let before = proof.work().proof_operations;
        let step = proof.step()?;
        assert!(proof.work().proof_operations - before <= 16384);
        if step == ProofStep::Complete {
            return proof.into_publication();
        }
    }
    panic!("proof continuation bound");
}

fn finish_publication(publication: &mut NormalPublication<'_>) {
    for _ in 0..10000 {
        if publication.step().unwrap() == PublicationStep::Idle {
            return;
        }
    }
    panic!("publication continuation bound");
}
fn make_writer(fixture: &Fixture, place: &collector::ring::Place) -> collector::ring::Writer {
    let reader = fixture.reader();
    if reader.dictionary_capacity().is_some() {
        collector::ring::Writer::create_with_identity(
            place,
            reader.shape(),
            collector::ring::DictionaryCapacity {
                entries: 4,
                arena_bytes: 16 * 1024,
            },
            &fixture.run,
            1,
        )
        .unwrap()
    } else {
        collector::ring::Writer::create_recording(place, reader.shape(), &fixture.run, 1).unwrap()
    }
}
fn replace_with_empty_generation(fixture: &mut Fixture) {
    fixture.writer.finish();
    let old_name = fixture.writer.name().to_owned();
    let temporary = collector::ring::Place {
        lock_path: fixture.base.join("temporary-empty.lock"),
        name_prefix: format!(
            "/fdq{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let writer = make_writer(fixture, &temporary);
    let old = std::mem::replace(&mut fixture.writer, writer);
    drop(old);
    let next = make_writer(fixture, &fixture.place);
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = next;
    collector::ring::unlink_generation(&old_name).unwrap();
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    fixture.writer.finish();
}
fn copy_detached_rows(fixture: &Fixture) -> Vec<crate::event_recovery::batch::Row> {
    let mut reader = fixture.reader();
    reader.seek_event(1).unwrap();
    let mut rows = Vec::with_capacity(2);
    for _ in 0..2 {
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if fixture.input(false).captured {
            reader.read_event_identity(&mut identity).unwrap()
        } else {
            reader.read_event().unwrap()
        };
        let collector::ring::FoundEvent::Event {
            number,
            tick,
            event,
        } = found
        else {
            panic!("real event");
        };
        let row = crate::event_recovery::batch::copy_row(
            &reader,
            &crate::NumberedEvent {
                number,
                tick,
                event: &event,
            },
            fixture.input(false).captured.then_some(&identity),
        )
        .unwrap();
        assert_eq!(row.publication, None);
        rows.push(row);
    }
    rows
}
#[test]
fn nonempty_terminal_preserves_actual_rows_and_inherited_missed_suffix_in_both_profiles() {
    // Goal: actual generation rollover supplies nonempty terminal rows plus a proved loss suffix.
    // Method: save1, retain events from unowned3, roll to empty4 and publish the copied exact rows.
    for captured in [false, true] {
        terminal_rollover(captured);
    }
}
fn terminal_rollover(captured: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    let manifest = encode_payload(&attempt, &fixture, 1, false);
    publish_at(&mut fixture.writer, 1);
    finish_publication(&mut issue(attempt, &manifest, fixture.reader(), &budget).unwrap());
    if captured {
        publish_captured_pair_at(&mut fixture, 61, 61_000_000_001);
    } else {
        publish_numeric_pair_at(&mut fixture, 61, 61_000_000_001);
    }
    let rows = copy_detached_rows(&fixture);
    replace_with_empty_generation(&mut fixture);
    let input = rollover_input(&fixture, &store, &budget);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(0).unwrap()).unwrap();
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let bytes = fs::read(object.join("pending/0000.parquet")).unwrap();
    let recovery = crate::event_recovery::Recovery {
        run: fixture.run,
        events_first: 1,
        events_end: 3,
        rows: 2,
        losses: crate::Losses {
            publications: 2,
            events: 0,
            unknown: false,
        },
    };
    let proof = attempt
        .prove_terminal(
            &recovery,
            &[crate::event_recovery::Batch {
                first: 1,
                end: 3,
                rows: 2,
            }],
            fixture.reader(),
            &budget,
        )
        .unwrap();
    drop(finish_owned_proof(proof).unwrap());
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    drop(store);
    terminal_rollover_reopen(&fixture, &object, &bytes, &rows, &budget);
}
fn terminal_rollover_reopen(
    fixture: &Fixture,
    object: &Path,
    bytes: &[u8],
    rows: &[crate::event_recovery::batch::Row],
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let store = Store::open(&fixture.root).unwrap();
    finish_publication(&mut store.resume_publication(budget).unwrap());
    assert_eq!(fs::read(object.join("ready/0000.parquet")).unwrap(), bytes);
    let directory = PrivateDirectory::open(&object.join("ready")).unwrap();
    let captured = fixture.input(true).captured;
    let actual = crate::event_recovery::read::read_batch_file(
        directory.open_file("0000.parquet").unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 3,
            rows: 2,
        },
        captured,
        true,
        budget,
    )
    .unwrap();
    let expected = crate::event_recovery::batch::frame(rows, captured).unwrap();
    assert!(actual.equals_missing(&expected));
    let metadata =
        crate::canonical::CanonicalMetadata::read(&mut directory.open_file("metadata").unwrap())
            .unwrap();
    assert_eq!(metadata.loss.inherited.publications, 2);
    assert_eq!(metadata.loss.publications_new, 0);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.events_new, 0);
    assert_eq!(metadata.loss.publication_end, 4);
    assert_eq!(metadata.loss.event_end, 3);
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
    assert!(frontier.complete());
}

use std::os::fd::{AsRawFd, FromRawFd};
struct WritableHeader {
    pointer: *mut AtomicU64,
    length: usize,
    _file: File,
}
unsafe extern "C" {
    fn shm_open(name: *const std::ffi::c_char, flags: i32, ...) -> i32;
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
}
impl WritableHeader {
    fn open(fixture: &Fixture) -> Self {
        let name = std::ffi::CString::new(fixture.writer.name()).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), 0) };
        assert!(descriptor >= 0);
        let old = unsafe { File::from_raw_fd(descriptor) };
        let length = usize::try_from(old.metadata().unwrap().len()).unwrap();
        assert!(length <= 1024 * 1024);
        let original = unsafe { mmap(std::ptr::null_mut(), length, 1, 1, old.as_raw_fd(), 0) };
        assert_ne!(original.addr(), usize::MAX);
        let bytes = unsafe { std::slice::from_raw_parts(original.cast::<u8>(), length) }.to_vec();
        assert_eq!(unsafe { munmap(original, length) }, 0);
        drop(old);
        // Source objects deliberately refuse new writable opens. As in the existing worker
        // forged-source fixture, recreate only this owned name with an initial writable fd.
        collector::ring::unlink_generation(fixture.writer.name()).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), 0xa02, 0o400_u32) };
        assert!(descriptor >= 0);
        let file = unsafe { File::from_raw_fd(descriptor) };
        file.set_len(u64::try_from(length).unwrap()).unwrap();
        let pointer = unsafe { mmap(std::ptr::null_mut(), length, 3, 1, file.as_raw_fd(), 0) };
        assert_ne!(pointer.addr(), usize::MAX);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), length);
        }
        Self {
            pointer: pointer.cast(),
            length,
            _file: file,
        }
    }
    fn word(&mut self, position: usize) -> &AtomicU64 {
        assert!(position < 8);
        unsafe { &*self.pointer.add(position) }
    }
}
impl Drop for WritableHeader {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.pointer.cast(), self.length) }, 0);
    }
}

#[test]
fn proof_error_and_nothing_leave_the_logical_publication_cursor_unchanged() {
    // Goal: failed source loads and Nothing cannot skip or charge a complement position.
    // Method: alter only this isolated mapped fixture's newest header, then restore and retry.
    for newest in [0, u64::MAX] {
        let mut fixture = proof_fixture(false, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
            .unwrap();
        publish_at(&mut fixture.writer, 0);
        let manifest = encode_payload(&attempt, &fixture, 2, false);
        publish_at(&mut fixture.writer, 1);
        let mut header = WritableHeader::open(&fixture);
        let mut proof = attempt
            .prove_ordinary(&manifest, fixture.reader(), &budget)
            .unwrap();
        let previous = header.word(4).swap(newest, Ordering::Release);
        // These are negative fault inputs only; neither supplies positive loss evidence.
        let mut failed = false;
        for _ in 0..10000 {
            if proof.step().is_err() {
                failed = true;
                break;
            }
        }
        header.word(4).store(previous, Ordering::Release);
        assert!(failed);
        assert_eq!(proof.work().proof_publication_next, 1);
        assert_eq!(proof.work().proof_event_next, 1);
        let publication = finish_owned_proof(proof).unwrap();
        finish_publication(&mut { publication });
    }
}
fn finish_owned_proof(mut proof: NormalProof<'_>) -> io::Result<NormalPublication<'_>> {
    for _ in 0..10000 {
        let before = proof.work().proof_operations;
        let step = proof.step()?;
        assert!(proof.work().proof_operations - before <= 16384);
        if step == ProofStep::Complete {
            return proof.into_publication();
        }
    }
    panic!("proof continuation bound");
}

#[test]
fn retained_original_tick_is_a_positive_control_in_both_profiles() {
    // Goal: a readable source tick is accepted when that same tick is in the raw object.
    // Method: use four slots, retain1 across the next-window boundary and publish its raw copy.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 4);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
            .unwrap();
        let manifest = encode_payload(&attempt, &fixture, 1, false);
        publish_at(&mut fixture.writer, 1);
        finish_publication(&mut issue(attempt, &manifest, fixture.reader(), &budget).unwrap());
        assert_eq!(
            store
                .source_frontier(
                    &fixture.run,
                    &budget,
                    &mut crate::canonical::ReadWork::default()
                )
                .unwrap()
                .unwrap()
                .publication_end(),
            2
        );
    }
}
#[test]
fn proof_generation_change_refuses_before_any_metadata_and_retries_after_restore() {
    // Goal: a proof cannot concatenate checked outcomes from different source generations.
    // Method: change only an owned negative fixture's generation after proof entry, then restore.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    publish_at(&mut fixture.writer, 0);
    let manifest = encode_payload(&attempt, &fixture, 2, false);
    publish_at(&mut fixture.writer, 1);
    let path = metadata_path(&fixture, &attempt);
    let mut header = WritableHeader::open(&fixture);
    let mut proof = attempt
        .prove_ordinary(&manifest, fixture.reader(), &budget)
        .unwrap();
    let old = header.word(2).fetch_add(1, Ordering::Release);
    let mut refused = false;
    for _ in 0..10000 {
        if proof.step().is_err() {
            refused = true;
            break;
        }
    }
    header.word(2).store(old, Ordering::Release);
    assert!(refused);
    assert!(!path.exists());
    assert_eq!(proof.work().proof_publication_next, 1);
    finish_publication(&mut finish_owned_proof(proof).unwrap());
}

fn rollover_input(
    fixture: &Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
) -> AttemptInput {
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    let mut input = fixture.input(true);
    input.operation = AttemptOperation::Terminal { event_end: 3 };
    input.losses = LossTransfer {
        predecessor: frontier.predecessor().clone(),
        publication_start: frontier.publication_end(),
        publication_inherited_end: 4,
        event_start: frontier.event_end(),
        event_inherited_end: frontier.event_end(),
        publications: 2,
        events: 0,
        unknown: None,
        prior_window: frontier.prior_window(),
    };
    input
}

#[test]
fn checked_source_read_rejects_a_generation_change_after_the_actual_load() {
    // Goal: post-load binding is checked even when the source operation returned valid data.
    // Method: perform a real timing read, then change this negative fixture's generation.
    let fixture = proof_fixture(false, 4);
    let budget = registration_budget();
    let mut header = WritableHeader::open(&fixture);
    let input = fixture.input(false);
    let mut source = normal_proof::SourceProof::new(
        fixture.reader(),
        &input,
        budget.reserve(64 * 1024).unwrap(),
    )
    .unwrap();
    let mut old = 0;
    let result = source.checked_read(&input, |reader| {
        reader.seek_publication(1)?;
        let found = reader.read_timing_into(&mut collector::ring::PublicationTiming::default());
        old = header.word(2).fetch_add(1, Ordering::Release);
        found
    });
    header.word(2).store(old, Ordering::Release);
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(source.positions(), (1, 1));
}

#[test]
fn native_proof_admission_and_metadata_retry_keep_the_shared_resource_bounds() {
    // Goal: native raw EIO and effect-before-error metadata sync preserve proof authority.
    // Method: run isolated Store children with the existing bounded syscall interposer.
    if let Some(base) = std::env::var_os("FLIGHTDECK_LOSS_PROOF_BASE") {
        native_proof_child(Path::new(&base));
        return;
    }
    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    for boundary in ["raw", "metadata"] {
        let mut fixture = proof_fixture(true, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
            .unwrap();
        publish_captured_pair_at(&mut fixture, 2, 2);
        let manifest = encode_payload(&attempt, &fixture, 2, false);
        publish_at(&mut fixture.writer, 1);
        manifest
            .write(&mut File::create(fixture.base.join("manifest")).unwrap())
            .unwrap();
        let pending = metadata_path(&fixture, &attempt)
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        drop(attempt);
        drop(store);
        let root = fixture.root.canonicalize().unwrap();
        let counts = fixture.base.join("proof-fds");
        fs::write(&counts, b"").unwrap();
        fs::write(fixture.base.join("observed"), b"").unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                concat!(
                    "store::registration_tests::loss_proof_tests::",
                    "native_proof_admission_and_metadata_retry_keep_the_shared_resource_bounds"
                ),
                "--nocapture",
            ])
            .env("DYLD_INSERT_LIBRARIES", &library)
            .env("FLIGHTDECK_LOSS_PROOF_BASE", &fixture.base)
            .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
            .env("FLIGHTDECK_REGISTRATION_ARMED", fixture.base.join("armed"))
            .env(
                "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
                fixture.base.join("observed"),
            )
            .env("FLIGHTDECK_FD_ROOT", &root)
            .env("FLIGHTDECK_FD_LOG", &counts);
        if boundary == "raw" {
            command.env(
                "FLIGHTDECK_INVENTORY_READ_TARGET",
                pending.join("events/index"),
            );
        } else {
            command
                .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", pending.join("metadata"))
                .env("FLIGHTDECK_SYNC_EFFECT", "1");
        }
        assert!(OwnedChild(command.spawn().unwrap()).wait().success());
        native_proof_counts(&fixture, boundary);
    }
}
fn native_proof_child(base: &Path) {
    let root = base.join("recording");
    let store = Store::open(&root).unwrap();
    let independently_borrowed_root = PrivateDirectory::open(&root).unwrap();
    let before = fs::read(root.join("foreground")).unwrap();
    let manifest = Manifest::read(&mut File::open(base.join("manifest")).unwrap()).unwrap();
    let reader =
        RingReader::open_generation(&std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap())
            .unwrap();
    let budget = registration_budget();
    let mut proof = store
        .resume_attempt()
        .unwrap()
        .prove_ordinary(&manifest, reader, &budget)
        .unwrap();
    fs::write(base.join("armed"), b"a").unwrap();
    let mut failed = false;
    for _ in 0..10000 {
        match proof.step() {
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(5));
                failed = true;
                break;
            }
            Ok(ProofStep::Continue) => {}
            Ok(ProofStep::Complete) => panic!("native proof fault was not reached"),
        }
    }
    assert!(failed);
    assert_eq!(fs::read(root.join("foreground")).unwrap(), before);
    if std::env::var_os("FLIGHTDECK_INVENTORY_READ_TARGET").is_some() {
        assert_eq!(proof.work().proof_publication_next, 1);
        assert_eq!(proof.work().proof_event_next, 1);
    }
    fs::remove_file(base.join("armed")).unwrap();
    let mut publication = finish_owned_proof(proof).unwrap();
    let work = publication.work();
    println!(
        "proof-native work={work:?} reservation_peak={}",
        budget.used_peak()
    );
    finish_publication(&mut publication);
    drop(publication);
    assert_eq!(budget.used(), 0);
    drop(independently_borrowed_root);
}
fn native_proof_counts(fixture: &Fixture, boundary: &str) {
    assert!(!fs::read(fixture.base.join("observed")).unwrap().is_empty());
    let bytes = fs::read(fixture.base.join("proof-fds")).unwrap();
    assert_eq!(bytes.len() % 4, 0);
    let records = bytes.as_chunks::<4>().0;
    let namespace_peak = records
        .iter()
        .map(|r| u16::from_le_bytes([r[0], r[1]]))
        .max()
        .unwrap();
    let raw_peak = records
        .iter()
        .map(|r| u16::from_le_bytes([r[2], r[3]]))
        .max()
        .unwrap();
    assert!(namespace_peak <= 8, "{boundary}: {namespace_peak}");
    println!(
        concat!(
            "proof-native boundary={} namespace_peak={} raw_peak={} ",
            "samples={}"
        ),
        boundary,
        namespace_peak,
        raw_peak,
        records.len()
    );
    if let Some(path) = std::env::var_os("FLIGHTDECK_LOSS_PROOF_EVIDENCE") {
        let path = PathBuf::from(path);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(format!("{boundary}-fds.bin")), bytes).unwrap();
        fs::copy(
            fixture.base.join("observed"),
            path.join(format!("{boundary}-fault.bin")),
        )
        .unwrap();
    }
}

#[test]
fn consuming_proof_uses_the_existing_ledger_and_preserves_raw_on_exhaustion() {
    // Goal: proof preflight cannot hide another ledger or issue metadata without capacity.
    // Method: exhaust the shared budget, inspect registered bytes, then release and resume.
    let mut fixture = proof_fixture(false, 4);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    let manifest = encode_payload(&attempt, &fixture, 1, false);
    publish_at(&mut fixture.writer, 1);
    let path = metadata_path(&fixture, &attempt);
    let ticks = fs::read(path.parent().unwrap().join("ticks.parquet")).unwrap();
    let before = fixture.slot();
    let held = budget.reserve(budget.maximum() - budget.used()).unwrap();
    assert!(
        matches!(attempt.prove_ordinary(&manifest, fixture.reader(), &budget),
        Err(error) if error.kind() == io::ErrorKind::OutOfMemory)
    );
    assert_eq!(fixture.slot(), before);
    assert!(!path.exists());
    assert_eq!(
        fs::read(path.parent().unwrap().join("ticks.parquet")).unwrap(),
        ticks
    );
    drop(held);
    let publication = issue(
        store.resume_attempt().unwrap(),
        &manifest,
        fixture.reader(),
        &budget,
    )
    .unwrap();
    drop(publication);
    assert_eq!(budget.used(), 0);
}
