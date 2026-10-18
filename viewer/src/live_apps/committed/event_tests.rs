use super::*;

fn event(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO events (publication, event, run_tick, pid, kind, availability, noticed, \
        identity_version, identity_availability, usage_availability, process_unique_id, \
        pid_version, executable_name, app_path) VALUES (1,10,0,7,2,3,0,1,0,3,700,2, \
        from_hex('776f726b6572ff'), from_hex('2f422e617070'))",
        )
        .unwrap();
}

fn selected() -> Endpoint {
    let mut selected = endpoint();
    selected.events_first = 10;
    selected.events_end = 11;
    selected
}

fn capture(connection: &Connection, selected: &Endpoint) -> Result<Box<Frame>> {
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut frame = None;
    let result = Frame::committed(selected, connection, "app_rows", &operation, &mut frame);
    if let Err(error) = result {
        assert!(frame.is_none());
        return Err(error);
    }

    Ok(frame.unwrap())
}

#[test]
fn owns_exec_program_bytes_after_the_query_and_source_are_released() {
    // Goal: committed events retain original bytes and exact creation after query release.
    // Method: decode one Exec with non-UTF-8 bytes, drop DuckDB, and inspect the owned frame.
    let connection = connection();
    event(&connection);

    let frame = capture(&connection, &selected()).unwrap();
    drop(connection);

    assert_eq!(frame.execs.len(), 1);
    let exec = &frame.execs[0];
    assert_eq!(exec.number, 10);
    assert_eq!(exec.creation.unwrap().unique_id, 700);
    assert_eq!(exec.creation.unwrap().pid_version, 2);
    let program = exec.program.unwrap();
    assert_eq!(&frame.arena[program.start..program.name_end], b"worker\xff");
    assert_eq!(&frame.arena[program.name_end..program.end], b"/B.app");
}

macro_rules! malformed_event {
    ($name:ident, $mutation:expr, $reason:expr) => {
        #[test]
        fn $name() {
            // Goal: malformed saved event metadata cannot install a partial selected frame.
            // Method: alter one field of a real typed event row and require explicit refusal.
            let connection = connection();
            event(&connection);
            connection.execute_batch($mutation).unwrap();

            let result = capture(&connection, &selected());

            assert!(matches!(result, Err(Error::Bound(reason)) if reason == $reason));
        }
    };
}

malformed_event!(
    rejects_an_event_from_another_tick,
    "UPDATE events SET run_tick=1",
    "committed App event tick"
);
malformed_event!(
    rejects_an_event_before_the_recorded_range,
    "UPDATE events SET event=9",
    "committed App event order"
);
malformed_event!(
    rejects_an_event_at_the_exclusive_recorded_end,
    "UPDATE events SET event=11",
    "committed App event range"
);
malformed_event!(
    rejects_duplicate_event_numbers,
    "INSERT INTO events SELECT * FROM events",
    "committed App event order"
);
malformed_event!(
    rejects_an_unknown_event_kind,
    "UPDATE events SET kind=4",
    "committed App row kind"
);
malformed_event!(
    does_not_treat_a_zero_event_kind_as_a_scheduled_member,
    "UPDATE events SET kind=0",
    "committed App row kind"
);
malformed_event!(
    rejects_numeric_usage_on_an_exec,
    "UPDATE events SET usage_availability=0",
    "committed App event program usage"
);
malformed_event!(
    rejects_numeric_usage_on_a_start,
    "UPDATE events SET kind=1,usage_availability=0",
    "committed App event program usage"
);
malformed_event!(
    rejects_not_applicable_exit_usage,
    "UPDATE events SET kind=3",
    "committed App usage availability"
);

#[test]
fn retains_event_gaps_without_inventing_missing_members() {
    // Goal: retained events survive a missing event number without fabricating a fact.
    // Method: decode two Execs separated by one lost number in the recorded interval.
    let connection = connection();
    event(&connection);
    connection
        .execute_batch(
            "INSERT INTO events SELECT * REPLACE (12::UBIGINT AS event) \
        FROM events",
        )
        .unwrap();
    let mut selected = selected();
    selected.events_end = 13;

    let frame = capture(&connection, &selected).unwrap();

    assert_eq!(
        frame
            .execs
            .iter()
            .map(|exec| exec.number)
            .collect::<Vec<_>>(),
        vec![10, 12]
    );
    assert_eq!(frame.process_count(), 0);
}

#[test]
fn rejects_more_than_the_retained_event_bound_before_decoding() {
    // Goal: event vectors cannot exceed the live frame's declared event count.
    // Method: select 16385 typed event rows with distinct retained numbers.
    let connection = connection();
    event(&connection);
    connection
        .execute_batch(
            "INSERT INTO events SELECT events.* REPLACE (number AS event) \
        FROM events CROSS JOIN range(11,16395) entries(number)",
        )
        .unwrap();
    let mut selected = selected();
    selected.events_end = 16395;

    let result = capture(&connection, &selected);

    assert!(matches!(
        result,
        Err(Error::Bound("committed App event bound"))
    ));
}

#[test]
fn charges_scheduled_and_event_programs_to_one_interval_bound() {
    // Goal: event identities cannot evade the existing 24000-program transient limit.
    // Method: combine 24000 scheduled members with one captured Exec.
    let connection = connection();
    dense_publication(&connection, 1, 24000);
    event(&connection);

    let result = capture(&connection, &selected());

    assert!(matches!(
        result,
        Err(Error::Bound("committed App process bound"))
    ));
}

#[test]
fn accepts_the_exact_retained_event_bound() {
    // Goal: an endpoint at the event limit is not lost to a fencepost refusal.
    // Method: decode 16384 typed Exec rows and retain their independent event numbers.
    let connection = connection();
    event(&connection);
    connection
        .execute_batch(
            "INSERT INTO events SELECT events.* REPLACE (number AS event) \
        FROM events CROSS JOIN range(11,16394) entries(number)",
        )
        .unwrap();
    let mut selected = selected();
    selected.events_end = 16394;

    let frame = capture(&connection, &selected).unwrap();

    assert_eq!(frame.execs.len(), 16384);
    assert_eq!(frame.execs[0].number, 10);
    assert_eq!(frame.execs[16383].number, 16393);
    assert_eq!(frame.arena.len(), 16384 * 13);
}
