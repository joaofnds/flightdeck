//! Empty table schemas must retain the complete source-derived column groups.
use super::*;

#[test]
fn ordinary_empty_schema_numeric_recovers() {
    // Goal: valid empty numeric tables remain recoverable through public Store admission.
    // Method: encode a real completed tick, reopen and compare every raw file after Idle.
    empty_schema_valid(empty_schema_fixture(false));
}

#[test]
fn ordinary_empty_schema_captured_recovers() {
    // Goal: empty captured process tables retain their identity schema during recovery.
    // Method: encode an actual captured tick and require unchanged raw files at Idle.
    empty_schema_valid(empty_schema_fixture(true));
}

#[test]
fn ordinary_empty_schema_system_recovers() {
    // Goal: a system-enabled tick with no core states retains its frequency column.
    // Method: recover production-encoded empty core states from an actual system source.
    empty_schema_valid(empty_schema_system_fixture());
}

#[test]
fn ordinary_empty_schema_missing_identity_refuses() {
    // Goal: absent rows cannot justify dropping the source's captured identity group.
    // Method: remove the whole group from a valid empty process table and reopen Store.
    let (fixture, object, budget) = empty_schema_stage(empty_schema_fixture(true));
    rewrite(&object.join("pending/processes.parquet"), |frame| {
        assert_eq!(frame.height(), 0);
        for (name, _) in crate::schema::IDENTITY_FIELDS {
            frame.drop_in_place(name).unwrap();
        }
    });
    empty_schema_refuses(&fixture, &object, &budget);
}

#[test]
fn ordinary_empty_schema_missing_frequency_refuses() {
    // Goal: empty core states cannot omit the window's system frequency column.
    // Method: drop frequency_khz after encoding a real system-enabled source tick.
    let (fixture, object, budget) = empty_schema_stage(empty_schema_system_fixture());
    rewrite(&object.join("pending/core_states.parquet"), |frame| {
        assert_eq!(frame.height(), 0);
        frame.drop_in_place("frequency_khz").unwrap();
    });
    empty_schema_refuses(&fixture, &object, &budget);
}

#[test]
fn ordinary_empty_schema_extra_column_refuses() {
    // Goal: empty required tables admit only the source-derived schema.
    // Method: add a framed empty column without changing any row count or other value.
    let (fixture, object, budget) = empty_schema_stage(empty_schema_fixture(false));
    rewrite(&object.join("pending/processes.parquet"), |frame| {
        assert_eq!(frame.height(), 0);
        frame
            .with_column(Column::new("unexpected".into(), Vec::<u64>::new()))
            .unwrap();
    });
    empty_schema_refuses(&fixture, &object, &budget);
}

#[test]
fn ordinary_empty_schema_wrong_type_refuses() {
    // Goal: empty columns retain their source-derived storage types.
    // Method: change a required empty process column's type while retaining its name.
    let (fixture, object, budget) = empty_schema_stage(empty_schema_fixture(false));
    rewrite(&object.join("pending/processes.parquet"), |frame| {
        assert_eq!(frame.height(), 0);
        frame
            .with_column(Column::new("pid".into(), Vec::<u64>::new()))
            .unwrap();
    });
    empty_schema_refuses(&fixture, &object, &budget);
}

fn empty_schema_fixture(captured: bool) -> Fixture {
    let mut fixture = proof_fixture(captured, 1);
    if captured {
        publish_captured_pair_at(&mut fixture, 2, 2);
    } else {
        publish_numeric_pair_at(&mut fixture, 2, 2);
    }
    fixture.writer.finish();
    fixture
}

fn empty_schema_system_fixture() -> Fixture {
    let mut fixture = Fixture::new_profile(false, true);
    let place = collector::ring::Place {
        lock_path: fixture.base.join("schema-system.lock"),
        name_prefix: format!(
            "/fdsch{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let mut shape = fixture.reader().shape();
    shape.event_capacity = 4;
    let writer = collector::ring::Writer::create_with_system(
        &place,
        shape,
        collector::ring::DictionaryCapacity {
            entries: 4,
            arena_bytes: 16 * 1024,
        },
        &fixture.run,
        1,
    )
    .unwrap();
    let previous = fixture.writer.name().to_owned();
    fixture.writer = writer;
    collector::ring::unlink_generation(&previous).unwrap();
    fixture.place = place;
    fixture.binding = fixture.reader().object_binding().unwrap();
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    publish_at(&mut fixture.writer, 0);
    empty_schema_system_fixture_publish(&mut fixture);
    fixture.writer.finish();
    fixture
}

fn empty_schema_system_fixture_publish(fixture: &mut Fixture) {
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &collector::ring::ProgramBytes {
                executable_name: b"schema-\xff",
                app_path: None,
            },
            &mut program,
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
            program: program.unwrap(),
        },
    };
    let readings = collector::ring::Readings {
        run_tick: 2,
        cpus: &[],
        memory: &collector::machine::Memory::default(),
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &std::array::from_fn::<_, 2, _>(|_| collector::tracker::Event::Exec { pid: 41 }),
        dropped: collector::ring::Dropped::default(),
    };
    let system = collector::ring::SystemPublication {
        reading: &collector::system::SystemReading::default(),
        machine: &collector::system::MachineFacts::default(),
        clock: None,
        process_gpu_ns: &[],
    };
    fixture
        .writer
        .publish_with_disks(
            &readings,
            Some(&collector::ring::ObservationTimes {
                wall_ns: 2,
                continuous_ns: 2,
                awake_ns: 2,
            }),
            &collector::ring::IdentityPublication {
                processes: &[],
                events: &[identity; 2],
            },
            &collector::ring::SupplementalReadings {
                battery: None,
                disks: None,
                system: Some(&system),
            },
        )
        .unwrap();
}

fn empty_schema_stage(fixture: Fixture) -> (Fixture, PathBuf, Arc<crate::resources::ByteBudget>) {
    eprintln!(
        "ORDINARY-SCHEMA root={} source={}",
        fixture.base.display(),
        fixture.writer.name()
    );
    let budget = registration_budget();
    let object;
    {
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(&complete_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        complete_payload(&attempt, &fixture, 1);
    }
    (fixture, object, budget)
}

fn empty_schema_valid(fixture: Fixture) {
    let (fixture, object, budget) = empty_schema_stage(fixture);
    let raw = raw_bytes(&object.join("pending"));
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    finish_recovery(&mut owner);
    drop(owner);
    for (name, expected) in raw {
        assert_eq!(fs::read(object.join("ready").join(name)).unwrap(), expected);
    }
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 3);
    assert_eq!(frontier.event_end(), 3);
}

fn empty_schema_refuses(
    fixture: &Fixture,
    object: &Path,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let pending = object.join("pending");
    let raw = raw_bytes(&pending);
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), budget, &ledger)
        .unwrap();
    assert_eq!(failure(&mut owner).kind(), io::ErrorKind::InvalidData);
    drop(owner);
    assert_eq!(fixture.slot(), occupied);
    assert_eq!(raw_bytes(&pending), raw);
    assert!(!pending.join("metadata").exists());
}
