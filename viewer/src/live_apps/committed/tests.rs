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
        events_first: 0,
        events_end: 0,
    }
}

#[path = "fixture.rs"]
mod fixture;

fn connection() -> Engine {
    let connection = crate::data::connection();
    for index in [7, 9] {
        let (table, columns, _) = persistence::schema::TABLES[index];
        let extension = persistence::schema::IDENTITY_FIELDS
            .map(|(name, kind)| format!("{name} {kind}"))
            .join(",");
        connection
            .query(&format!(
                "CREATE TABLE {} ({columns},{extension})",
                table.name()
            ))
            .unwrap();
    }
    fixture::refresh(&connection);
    connection
}

fn measured(connection: &Engine) {
    measured_at(connection, 1);
}

fn measured_at(connection: &Engine, publication: u64) {
    use AnyValue::{Binary, Int32, UInt64};
    fixture::insert(
        connection,
        "processes",
        &[
            ("publication", UInt64(publication)),
            ("pid", Int32(7)),
            ("awake_ns_at_start", UInt64(9)),
            ("user_ns", UInt64(9_007_199_254_740_993)),
            ("system_ns", UInt64(9_007_199_254_740_995)),
            ("energy_nj", UInt64(9_007_199_254_740_997)),
            ("instructions", UInt64(1)),
            ("super_instructions", UInt64(2)),
            ("cycles", UInt64(3)),
            ("idle_wakeups", UInt64(4)),
            ("interrupt_wakeups", UInt64(5)),
            ("pageins", UInt64(6)),
            ("resident_bytes", UInt64(9_007_199_254_740_999)),
            ("footprint_bytes", UInt64(8)),
            ("disk_read_bytes", UInt64(9)),
            ("disk_written_bytes", UInt64(10)),
            ("threads", Int32(1)),
            ("faults", Int32(2)),
            ("context_switches", Int32(3)),
            ("mach_syscalls", Int32(4)),
            ("unix_syscalls", Int32(5)),
            ("identity_version", UInt64(1)),
            ("identity_availability", UInt64(0)),
            ("usage_availability", UInt64(0)),
            ("process_unique_id", UInt64(9_007_199_254_741_001)),
            ("pid_version", Int32(-7)),
            ("executable_name", Binary(b"a\xff\\")),
            ("app_path", Binary(b"/A.app")),
        ],
    );
}

#[test]
fn decodes_requested_committed_frames_with_an_empty_middle_publication() {
    // Goal: a batch keeps exact endpoint order, including an observed zero-member publication.
    // Method: decode publications one and three around an empty second endpoint, retain summaries.
    let connection = connection();
    measured_at(&connection, 1);
    measured_at(&connection, 3);
    measured_at(&connection, 99);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut endpoints = [endpoint(), endpoint(), endpoint()];
    endpoints[1].stamp.publication = 2;
    endpoints[2].stamp.publication = 3;
    let mut actual = Vec::new();

    Frame::committed_many(&endpoints, &connection, "app_rows", &operation, |frame| {
        actual.push((
            frame.stamp.publication,
            frame.process_count(),
            frame
                .members
                .first()
                .map(|member| member.usage.unwrap().user_ns),
        ));
        Ok(())
    })
    .unwrap();

    assert_eq!(
        actual,
        vec![
            (1, 1, Some(9_007_199_254_740_993)),
            (2, 0, None),
            (3, 1, Some(9_007_199_254_740_993)),
        ]
    );
}

#[test]
fn rejects_duplicate_pid_rows_before_delivering_a_batch_endpoint() {
    // Goal: ambiguous row association cannot produce even a partial endpoint callback.
    // Method: decode two scheduled rows for one PID at the first requested publication.
    let connection = connection();
    measured(&connection);
    measured(&connection);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut callbacks = 0;

    let result = Frame::committed_many(
        &[endpoint()],
        &connection,
        "app_rows",
        &operation,
        |_frame| {
            callbacks += 1;
            Ok(())
        },
    );

    assert!(matches!(
        result,
        Err(Error::Bound("duplicate committed App PID"))
    ));
    assert_eq!(callbacks, 0);
}

#[test]
fn rejects_repeated_endpoint_publications_in_a_batch_request() {
    // Goal: one batch cannot assign the same source publication to two endpoints.
    // Method: request two identical publication identities from an otherwise empty table.
    let connection = connection();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut callbacks = 0;

    let result = Frame::committed_many(
        &[endpoint(), endpoint()],
        &connection,
        "app_rows",
        &operation,
        |_frame| {
            callbacks += 1;
            Ok(())
        },
    );

    assert!(matches!(
        result,
        Err(Error::Bound("committed App batch publication order"))
    ));
    assert_eq!(callbacks, 0);
}

#[test]
fn rejects_a_batch_larger_than_one_window_before_delivering_endpoints() {
    // Goal: batch metadata cannot grow beyond the existing 61-observation window bound.
    // Method: request 62 distinct empty endpoints and require an explicit refusal.
    let connection = connection();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let endpoints: Vec<_> = (0..=ENDPOINTS_MAX)
        .map(|index| {
            let mut endpoint = endpoint();
            endpoint.stamp.publication = u64::try_from(index).unwrap() + 1;
            endpoint
        })
        .collect();
    let mut callbacks = 0;

    let result = Frame::committed_many(&endpoints, &connection, "app_rows", &operation, |_frame| {
        callbacks += 1;
        Ok(())
    });

    assert!(matches!(
        result,
        Err(Error::Bound("committed App batch endpoints"))
    ));
    assert_eq!(callbacks, 0);
}

#[test]
fn delivers_all_sixty_one_empty_endpoints_at_the_window_bound() {
    // Goal: the inclusive window bound accepts observations with no scheduled members.
    // Method: request 61 empty publications and retain only callback counts and sizes.
    let connection = connection();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let endpoints: Vec<_> = (0..ENDPOINTS_MAX)
        .map(|index| {
            let mut endpoint = endpoint();
            endpoint.stamp.publication = u64::try_from(index).unwrap() + 1;
            endpoint
        })
        .collect();
    let mut callbacks = 0;
    let mut members = 0;

    Frame::committed_many(&endpoints, &connection, "app_rows", &operation, |frame| {
        callbacks += 1;
        members += frame.process_count();
        Ok(())
    })
    .unwrap();

    assert_eq!(callbacks, 61);
    assert_eq!(members, 0);
}

#[test]
fn cancels_a_batch_before_delivering_the_next_endpoint() {
    // Goal: a superseded batch cannot continue publishing later endpoint facts.
    // Method: cancel the owning operation after its first callback and request a second.
    let connection = connection();
    measured_at(&connection, 1);
    measured_at(&connection, 2);
    let cancellation = Arc::new(crate::Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let mut endpoints = [endpoint(), endpoint()];
    endpoints[1].stamp.publication = 2;
    let mut callbacks = 0;

    let result = Frame::committed_many(&endpoints, &connection, "app_rows", &operation, |_frame| {
        callbacks += 1;
        cancellation.cancel()?;
        Ok(())
    });

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert_eq!(callbacks, 1);
}

#[test]
fn refuses_an_oversized_process_group_in_the_batch_preflight() {
    // Goal: a single endpoint cannot exceed the shared process interval bound.
    // Method: create 24,001 small rows and require refusal before any callback.
    let connection = connection();
    measured(&connection);
    fixture::duplicate(&connection, "processes", 24_000);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut callbacks = 0;

    let result = Frame::committed_many(
        &[endpoint()],
        &connection,
        "app_rows",
        &operation,
        |_frame| {
            callbacks += 1;
            Ok(())
        },
    );

    assert!(matches!(
        result,
        Err(Error::Bound("committed App process bound"))
    ));
    assert_eq!(callbacks, 0);
}

macro_rules! chunk_boundary {
    ($name:ident, $bytes:expr, $count:expr) => {
        #[test]
        fn $name() {
            // Goal: normal groups obey the declared byte boundary including the sentinel.
            // Method: vary one exact BLOB-byte sum around the independently calculated target.
            let mut endpoints = [endpoint(), endpoint()];
            endpoints[1].stamp.publication = 2;
            let sizes = [
                PublicationSize {
                    publication: 1,
                    size: FrameSize {
                        count: 1000,
                        members: 1000,
                        starts: 0,
                        execs: 0,
                        exits: 0,
                        bytes: 4_000_000,
                    },
                },
                PublicationSize {
                    publication: 2,
                    size: FrameSize {
                        count: 1000,
                        members: 1000,
                        starts: 0,
                        execs: 0,
                        exits: 0,
                        bytes: $bytes,
                    },
                },
            ];

            let count = committed_many_chunk_count(&endpoints, &sizes).unwrap();

            assert_eq!(count, $count);
        }
    };
}

chunk_boundary!(
    packs_a_normal_group_one_byte_below_the_logical_target,
    3_515_985,
    2
);
chunk_boundary!(
    packs_a_normal_group_exactly_at_the_logical_target,
    3_515_986,
    2
);
chunk_boundary!(
    splits_a_normal_group_one_byte_above_the_logical_target,
    3_515_987,
    1
);

#[test]
fn ties_the_logical_row_charge_to_the_selected_columns_and_extra_row_reserve() {
    // Goal: changing the numeric projection cannot silently loosen its logical accounting.
    // Method: count the shared SELECT fields plus publication and verify the sentinel formula.
    assert_eq!(PROCESS_COLUMNS.split(',').count() + 6, BATCH_COLUMNS);
    assert_eq!(ROW_LOGICAL_BYTES, 432);
    assert_eq!(SENTINEL_LOGICAL_BYTES, 8_622);
    assert_eq!(BATCH_LOGICAL_BYTES_MAX, 8_388_608);
}

#[test]
fn delivers_no_endpoint_for_an_empty_batch_request() {
    // Goal: an empty request does no member work and produces no fabricated endpoint.
    // Method: call the batch adapter with an empty endpoint slice and count callbacks.
    let connection = connection();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut callbacks = 0;

    Frame::committed_many(&[], &connection, "app_rows", &operation, |_frame| {
        callbacks += 1;
        Ok(())
    })
    .unwrap();

    assert_eq!(callbacks, 0);
}

#[test]
fn rejects_logical_size_arithmetic_that_cannot_be_represented() {
    // Goal: the chunk planner refuses invalid size metadata instead of wrapping its charge.
    // Method: supply an impossible in-memory row count to its checked byte calculation.
    let size = FrameSize {
        count: usize::MAX,
        members: 0,
        starts: 0,
        execs: 0,
        exits: 0,
        bytes: 1,
    };

    let result = size.logical_bytes();

    assert!(matches!(
        result,
        Err(Error::Bound("committed App batch logical bytes"))
    ));
}

fn dense_publication(connection: &Engine, publication: u64, count: u32) {
    assert!(count > 0);
    assert!(count <= 24_000);
    measured_at(connection, publication);
    let mut group = connection
        .query(&format!(
            "SELECT * FROM processes WHERE publication={publication}"
        ))
        .unwrap()
        .new_from_index(0, usize::try_from(count).unwrap());
    let pids: Vec<_> = (1..=count).map(|pid| i32::try_from(pid).unwrap()).collect();
    let creations: Vec<_> = (1..=count).map(u64::from).collect();
    group
        .with_column(polars::prelude::Column::new("pid".into(), pids))
        .unwrap();
    group
        .with_column(polars::prelude::Column::new(
            "process_unique_id".into(),
            creations,
        ))
        .unwrap();
    group
        .with_column(polars::prelude::Column::new(
            "resident_bytes".into(),
            vec![11_u64; usize::try_from(count).unwrap()],
        ))
        .unwrap();
    let mut other = connection
        .query(&format!(
            "SELECT * FROM processes WHERE publication<>{publication}"
        ))
        .unwrap();
    other.vstack_mut(&group).unwrap();
    connection.register("processes", other).unwrap();
    fixture::refresh(connection);
}

#[test]
fn preserves_adjacent_totals_across_an_actual_normal_query_group_boundary() {
    // Goal: releasing a materialized result cannot break the next endpoint's rate baseline.
    // Method: decode two 12,000-row groups whose combined charge exceeds 8 MiB, retain one Frame.
    let connection = connection();
    dense_publication(&connection, 1, 12_000);
    dense_publication(&connection, 2, 12_000);
    let changed = connection
        .query(
            "SELECT * REPLACE (user_ns + CASE WHEN publication=2 THEN 1 ELSE 0 END AS user_ns) \
        FROM processes",
        )
        .unwrap();
    connection.register("processes", changed).unwrap();
    fixture::refresh(&connection);
    let mut endpoints = [endpoint(), endpoint()];
    endpoints[1].stamp.publication = 2;
    endpoints[1].run_tick = 1;
    endpoints[1].stamp.continuous_ns = Some(2_000_000_000);
    endpoints[1].stamp.awake_ns = Some(2_000_000_000);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let target = crate::live_app_history::Target {
        kind: crate::live_app_response::KeyKind::Bundle,
        bytes: b"/A.app",
    };
    let mut previous = None;
    let mut restored = None;

    Frame::committed_many(&endpoints, &connection, "app_rows", &operation, |frame| {
        restored = Some(
            crate::live_app_history::TracePoint::capture(&frame, previous.as_deref(), &target)?
                .json(0, false),
        );
        previous = Some(frame);
        Ok(())
    })
    .unwrap();

    let restored = restored.unwrap();
    assert_eq!(restored["members"], 12_000);
    assert_eq!(restored["elapsed_ns"], "1000000000");
    assert_eq!(restored["cpu_ns"]["known"], "12000");
    assert_eq!(restored["resident_bytes"]["known"], "132000");
}

#[test]
fn delivers_two_oversized_valid_endpoints_as_separate_singletons() {
    // Goal: the logical batch target preserves individually valid endpoints above 8 MiB.
    // Method: decode two 24,000-row endpoints, each requiring a bounded singleton query.
    let connection = connection();
    dense_publication(&connection, 1, 24_000);
    dense_publication(&connection, 2, 24_000);
    let mut endpoints = [endpoint(), endpoint()];
    endpoints[1].stamp.publication = 2;
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut actual = Vec::new();

    Frame::committed_many(&endpoints, &connection, "app_rows", &operation, |frame| {
        actual.push((frame.stamp().publication, frame.process_count()));
        Ok(())
    })
    .unwrap();

    assert_eq!(actual, vec![(1, 24_000), (2, 24_000)]);
}

#[test]
fn owns_raw_program_bytes_and_exact_integers_above_double_precision() {
    // Goal: committed adaptation preserves typed counters, creations and unescaped byte keys.
    // Method: capture typed integer/binary rows, drop Polars, and inspect owned endpoint facts.
    let connection = connection();
    measured(&connection);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut frame = None;

    Frame::committed(&endpoint(), &connection, "app_rows", &operation, &mut frame).unwrap();
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
    fixture::insert(
        &connection,
        "processes",
        &[
            ("publication", AnyValue::UInt64(1)),
            ("pid", AnyValue::Int32(1)),
            ("identity_version", AnyValue::UInt64(1)),
            ("identity_availability", AnyValue::UInt64(0)),
            ("usage_availability", AnyValue::UInt64(1)),
            ("process_unique_id", AnyValue::UInt64(11)),
            ("pid_version", AnyValue::Int32(-1)),
            ("executable_name", AnyValue::Binary(b"refused")),
        ],
    );
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut frame = None;

    Frame::committed(&endpoint(), &connection, "app_rows", &operation, &mut frame).unwrap();

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

    let result = Frame::committed(&endpoint(), &connection, "app_rows", &operation, &mut frame);

    assert!(matches!(
        result,
        Err(Error::Bound("duplicate committed App PID"))
    ));
    assert!(frame.is_none());
}

#[path = "event_tests.rs"]
mod event_tests;

macro_rules! saved_thread_cell {
    ($name:ident, $value:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // A saved optional signed Task gauge must preserve zero and reject absent/negative.
            let connection = connection();
            measured(&connection);
            fixture::replace(&connection, "processes", &[("threads", $value)]);
            let operation = Arc::new(crate::Cancellation::default())
                .operation(0)
                .unwrap();
            let mut frame = None;
            Frame::committed(&endpoint(), &connection, "app_rows", &operation, &mut frame).unwrap();
            let mut sample = None;

            crate::live_app_response::Sample::capture(frame.as_ref().unwrap(), None, &mut sample)
                .unwrap();
            let sample = sample.unwrap();
            let detail = sample
                .app_detail(&crate::live_app_response::Detail {
                    selected: &sample.selected,
                    app_index: 0,
                    page: crate::live_app_response::Page {
                        offset: 0,
                        limit: 512,
                    },
                })
                .unwrap();

            assert_eq!(detail["processes"][0]["threads"], $expected);
        }
    };
}

saved_thread_cell!(
    saved_null_threads_are_unavailable,
    AnyValue::Null,
    serde_json::json!({"known":null,"unavailable":"unknown"})
);
saved_thread_cell!(
    saved_negative_threads_are_unavailable,
    AnyValue::Int32(-1),
    serde_json::json!({"known":null,"unavailable":"unknown"})
);
saved_thread_cell!(
    saved_zero_threads_are_measured,
    AnyValue::Int32(0),
    serde_json::json!({"known":"0","unavailable":null})
);
saved_thread_cell!(
    saved_positive_threads_are_measured,
    AnyValue::Int32(8),
    serde_json::json!({"known":"8","unavailable":null})
);
