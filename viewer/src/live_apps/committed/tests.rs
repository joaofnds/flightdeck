use super::*;
use collector::ring::Identity;
use std::sync::Arc;

fn endpoint() -> Endpoint {
    Endpoint {
        identity: RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        },
        stamp: Stamp {
            boot: [5, 6],
            run: [3, 4],
            publication: 1,
            continuous_ns: Some(1_000_000_000),
            awake_ns: Some(1_000_000_000),
        },
        run_tick: 0,
        wall_ns: Some(1_001_000_000_000),
        usage_refused: 0,
        usage_gone: 0,
    }
}

fn connection() -> Connection {
    let connection = crate::data::connection().unwrap();
    let columns = persistence::schema::TABLES[7].1;
    connection
        .execute_batch(&format!("CREATE TABLE processes ({columns})"))
        .unwrap();
    for (name, kind) in persistence::schema::IDENTITY_FIELDS {
        connection
            .execute_batch(&format!("ALTER TABLE processes ADD COLUMN {name} {kind}"))
            .unwrap();
    }
    connection
}

fn measured(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO processes VALUES (1, 7, 9, 9007199254740993, 9007199254740995, \
        9007199254740997, 1, 2, 3, 4, 5, 6, 9007199254740999, 8, 9, 10, \
        1, 2, 3, 4, 5, 1, 0, 0, 9007199254741001, -7, from_hex('61ff5c'), \
        from_hex('2f412e617070'))",
        )
        .unwrap();
}

#[test]
fn owns_raw_program_bytes_and_exact_integers_above_double_precision() {
    // Goal: committed adaptation preserves typed counters, creations and unescaped byte keys.
    // Method: capture literal UBIGINT/BLOB rows, drop DuckDB, and inspect owned endpoint facts.
    let connection = connection();
    measured(&connection);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut frame = None;

    Frame::committed(
        &endpoint(),
        &connection,
        "processes",
        &operation,
        &mut frame,
    )
    .unwrap();
    drop(connection);
    let frame = frame.unwrap();

    let member = &frame.members[0];
    assert_eq!(member.creation.unwrap().unique_id, 9_007_199_254_741_001);
    assert_eq!(member.creation.unwrap().pid_version, -7);
    assert_eq!(
        member.usage.unwrap(),
        Usage {
            awake_ns_at_start: 9,
            user_ns: 9_007_199_254_740_993,
            system_ns: 9_007_199_254_740_995,
            energy_nj: 9_007_199_254_740_997,
            instructions: 1,
            super_instructions: 2,
            cycles: 3,
            idle_wakeups: 4,
            interrupt_wakeups: 5,
            pageins: 6,
            resident_bytes: 9_007_199_254_740_999,
            footprint_bytes: 8,
            disk_read_bytes: 9,
            disk_written_bytes: 10,
        }
    );
    let span = member.program.unwrap();
    assert_eq!(&frame.arena[span.start..span.name_end], b"a\xff\\");
    assert_eq!(&frame.arena[span.name_end..span.end], b"/A.app");
}

#[test]
fn retains_identity_only_refusal_without_fabricating_numeric_usage() {
    // Goal: an identity-only scheduled member retains its bytes and unavailable usage.
    // Method: capture a refusal with a wholly NULL original payload and inspect the member.
    let connection = connection();
    connection
        .execute_batch(
            "INSERT INTO processes (publication, pid, identity_version, identity_availability, \
        usage_availability, process_unique_id, pid_version, executable_name) \
        VALUES (1, 1, 1, 0, 1, 11, -1, from_hex('72656675736564'))",
        )
        .unwrap();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut frame = None;

    Frame::committed(
        &endpoint(),
        &connection,
        "processes",
        &operation,
        &mut frame,
    )
    .unwrap();

    let frame = frame.unwrap();
    assert!(matches!(frame.members[0].usage, Err(Unavailable::Refused)));
    assert_eq!(frame.members[0].creation.unwrap().unique_id, 11);
    assert!(frame.members[0].program.is_ok());
}

#[test]
fn rejects_duplicate_pid_associations_without_installing_a_partial_frame() {
    // Goal: duplicate selected process associations cannot become arbitrary App membership.
    // Method: supply two rows for one PID and require refusal with empty output.
    let connection = connection();
    measured(&connection);
    measured(&connection);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut frame = None;

    let result = Frame::committed(
        &endpoint(),
        &connection,
        "processes",
        &operation,
        &mut frame,
    );

    assert!(matches!(
        result,
        Err(Error::Bound("duplicate committed App PID"))
    ));
    assert!(frame.is_none());
}
