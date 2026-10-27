use persistence::engine::AdmittedTable;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{Cursor, Decoder, LoadInput, load_consume};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::{AppsSecond, ByteBudget};

#[test]
fn cursor_steps_consume_one_minute_and_preserve_the_exact_terminal() {
    // Goal: each step releases one minute while the completed cursor keeps the exact raw tail.
    // Method: advance three real sealed minutes separately and compare the terminal-only reader.
    let fixture = catch_up_fixture();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    let input = LoadInput {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
        capture: &AdmittedTable::from_file_budgeted,
    };
    let mut cursor = Cursor::new(&found);
    let mut accepted = Vec::new();

    cursor
        .step(&input, |_, second, _| accepted.push(second.publication))
        .unwrap();
    assert_eq!(accepted, [1]);
    assert_eq!(budget.used(), 0);
    cursor
        .step(&input, |_, second, _| accepted.push(second.publication))
        .unwrap();
    assert_eq!(accepted, [1, 2, 3]);
    assert_eq!(budget.used(), 0);
    cursor
        .step(&input, |_, second, _| accepted.push(second.publication))
        .unwrap();
    let outcome = cursor.finish(&operation).unwrap();
    let expected = super::terminal(
        &fixture.root,
        &found[2],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )
    .unwrap()
    .unwrap();

    assert_eq!(accepted, [1, 2, 3, 4]);
    assert_eq!(outcome.left_out, None);
    assert!(outcome.checked_catalog.is_none());
    assert_eq!(outcome.decoded_catalog.windows, 3);
    assert_eq!(outcome.decoded_catalog.first_known_second, Some(100));
    assert_eq!(outcome.decoded_catalog.last_known_second, Some(220));
    assert_eq!(outcome.decoded_catalog.unknown_clock_ticks, 0);
    let actual = outcome.terminal.unwrap();
    assert_eq!(actual.run, expected.run);
    assert_eq!(actual.machine, expected.machine);
    let (actual, actual_frame) = actual.process.unwrap().into_parts();
    let (expected, expected_frame) = expected.process.unwrap().into_parts();
    assert_eq!(actual.identity, expected.identity);
    assert_eq!(actual.stamp, expected.stamp);
    assert_eq!(actual.run_tick, expected.run_tick);
    assert_eq!(actual.wall_ns, expected.wall_ns);
    assert_eq!(actual.usage_refused, expected.usage_refused);
    assert_eq!(actual.usage_gone, expected.usage_gone);
    assert_eq!(actual.events_first, expected.events_first);
    assert_eq!(actual.events_end, expected.events_end);
    assert_eq!(actual_frame, expected_frame);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cursor_steps_preserve_the_first_live_successor() {
    // Goal: pausing at minute boundaries preserves the first live machine and process rates.
    // Method: seed real ring readers with a stepped cursor and the independent terminal reader.
    let mut fixture = catch_up_fixture();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    let input = LoadInput {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
        capture: &AdmittedTable::from_file_budgeted,
    };
    let mut cursor = Cursor::new(&found);
    let expected = super::terminal(
        &fixture.root,
        &found[2],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )
    .unwrap()
    .unwrap();

    cursor.step(&input, |_, _, _| {}).unwrap();
    cursor.step(&input, |_, _, _| {}).unwrap();
    cursor.step(&input, |_, _, _| {}).unwrap();
    let actual = cursor.finish(&operation).unwrap().terminal.unwrap();
    fixture.publish_with_system_rows(221);
    let expected = terminal_successor(&fixture.root, expected);
    let actual = terminal_successor(&fixture.root, actual);

    assert!(expected.0.follows);
    assert_eq!(expected.0.cpu_percent, Some(50.0));
    assert_eq!(expected.1.elapsed_ns, Some(1_000_000_000));
    assert_eq!(actual, expected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cursor_refuses_a_terminal_handoff_before_all_minutes_are_consumed() {
    // Goal: a partial cursor cannot publish a terminal or catalog as a completed load.
    // Method: consume the oldest real sealed minute and attempt to finish with two still pending.
    let fixture = catch_up_fixture();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    let input = LoadInput {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
        capture: &AdmittedTable::from_file_budgeted,
    };
    let mut cursor = Cursor::new(&found);
    cursor.step(&input, |_, _, _| {}).unwrap();

    let result = cursor.finish(&operation);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound(
            "sealed payload cursor unfinished"
        ))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_between_cursor_steps_releases_owners_but_preserves_a_reader() {
    // Goal: cancelling between minutes accepts no later rows and releases only unretained owners.
    // Method: retain one real payload, cancel before the next step, then drop each owner.
    let fixture = catch_up_fixture();
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    let input = LoadInput {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
        capture: &AdmittedTable::from_file_budgeted,
    };
    let mut cursor = Cursor::new(&found);
    let mut retained = AppsSecond::with_room();
    cursor
        .step(&input, |_, _, apps| {
            retained.take(apps, None, Some(&budget));
        })
        .unwrap();
    let reader = Arc::new(retained);
    let charge = crate::store::startup_tests::retained_app_charge(&reader);
    let mut accepted = 0;
    cancellation.cancel().unwrap();

    let result = cursor.step(&input, |_, _, _| accepted += 1);
    let finished = cursor.finish(&operation);

    assert_eq!(accepted, 0);
    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert!(matches!(finished, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(budget.used(), charge);
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    assert_eq!(reader.processes.as_ref().unwrap().len(), 2);
    drop(reader);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cursor_continues_after_a_refused_minute_without_reusing_its_predecessor() {
    // Goal: a refused minute leaves a gap without discarding later valid minutes or the refusal.
    // Method: damage the middle real marker, advance each minute, and inspect the recovered tail.
    let fixture = catch_up_fixture();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    std::fs::write(&found[1].marker, b"invalid committed minute").unwrap();
    let input = LoadInput {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
        capture: &AdmittedTable::from_file_budgeted,
    };
    let mut cursor = Cursor::new(&found);
    let mut accepted = Vec::new();

    cursor
        .step(&input, |_, second, _| {
            accepted.push((second.publication, second.follows));
        })
        .unwrap();
    cursor
        .step(&input, |_, second, _| {
            accepted.push((second.publication, second.follows));
        })
        .unwrap();
    assert_eq!(accepted, [(1, false)]);
    cursor
        .step(&input, |_, second, _| {
            accepted.push((second.publication, second.follows));
        })
        .unwrap();
    let outcome = cursor.finish(&operation).unwrap();

    assert_eq!(accepted, [(1, false), (4, false)]);
    assert_eq!(outcome.left_out, Some(crate::store::Failure::Refused));
    assert_eq!(outcome.decoded_catalog.windows, 2);
    let terminal = outcome.terminal.unwrap();
    assert_eq!(terminal.machine.ticks.len(), 1);
    assert_eq!(terminal.machine.ticks[0].publication, 4);
    assert_eq!(
        terminal.process.unwrap().into_parts().0.stamp.publication,
        4
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn saved_origin_carries_the_admitted_manifests_actual_losses() {
    // Goal: saved facts distinguish window losses from live reader losses without a new scan.
    // Method: set nonzero losses on a real sealed manifest, then use the complete Store loader.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let losses = persistence::Losses {
        publications: 4,
        events: 7,
        unknown: true,
    };
    change_committed_losses(&fixture.root, &found[0].marker, &losses);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut origins = Vec::new();

    let outcome =
        crate::store::recording::load_payload(&fixture.root, &budget, &operation, |_, _, apps| {
            let crate::store::apps::TickApps::Loaded(apps) = apps else {
                panic!("saved facts were not loaded");
            };
            origins.push(apps.metadata.origin);
        })
        .unwrap();

    assert_eq!(outcome.left_out, None);
    assert_eq!(origins, [crate::store::native::Origin::Saved(losses)]);
    drop(outcome);
    assert_eq!(budget.used(), 0);
}

fn change_committed_losses(
    root: &std::path::Path,
    marker: &std::path::Path,
    losses: &persistence::Losses,
) {
    let mut bytes = std::fs::read(marker).unwrap();
    // Format 1 puts the loss words after identities and the five window/range words.
    let first = b"FDSEAL01".len()
        + 2 * size_of::<u64>()
        + 3 * size_of::<collector::ring::Identity>()
        + 5 * size_of::<u64>();
    for (index, amount) in [
        losses.publications,
        losses.events,
        u64::from(losses.unknown),
    ]
    .into_iter()
    .enumerate()
    {
        let start = first + index * size_of::<u64>();
        bytes[start..start + size_of::<u64>()].copy_from_slice(&amount.to_le_bytes());
    }
    std::fs::write(marker, bytes).unwrap();
    assert_eq!(
        persistence::read_committed(root, marker).unwrap().losses,
        *losses
    );
}

#[test]
fn saved_computer_labels_keep_the_legacy_text_domain_through_the_store_loader() {
    // Goal: decoding complete computer facts adds no native-only limit to saved summaries.
    // Method: replace a real sealed model with 65 bytes and its chip with an empty label.
    use polars::prelude::lit;
    let fixture = crate::recording::sealed_system_minute();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let path = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let model = "m".repeat(65);
    let mut changed = persistence::engine::scan(&path)
        .unwrap()
        .with_column(lit(model.clone()).alias("machine_model"))
        .with_column(lit("").alias("machine_chip"))
        .collect()
        .unwrap();
    let replacement = fixture.root.join("changed-ticks.parquet");
    events_loader_write(&replacement, &mut changed);
    std::fs::rename(replacement, path).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut summaries = Vec::new();

    let outcome = crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |_, second, _| summaries.push(second.complete().unwrap().summary(second)),
    )
    .unwrap();

    assert_eq!(outcome.left_out, None);
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[1]["machine_model"], model);
    assert_eq!(summaries[1]["machine_chip"], "");
    drop(outcome);
    assert_eq!(budget.used(), 0);
}

#[test]
fn terminal_only_load_preserves_exact_source_without_deriving_apps() {
    // Goal: predecessor proof keeps exact terminal input without deriving App totals.
    // Method: trace terminal loading of two populated observations in a real sealed minute.
    use persistence::work::ingestion::{Key, Kind, Purpose, Trace};
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal_minute(0);
    let identity = fixture.writer.identity().unwrap();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    assert_eq!(found.len(), 1);
    let mut decoder = Decoder::default();
    let batch = decoder.decode(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    );
    assert_eq!(batch.seconds.len(), 2);
    assert!(batch.seconds.iter().all(|second| second.apps.is_ok()));
    let expected = decoder.terminal().unwrap();
    let trace = Trace::new(identity.recording);

    let scope = trace.enter();
    let terminal = super::terminal(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )
    .unwrap()
    .unwrap();
    drop(scope);

    assert_eq!(terminal.run, identity);
    assert_eq!(terminal.machine, expected.machine);
    let (endpoint, frame) = terminal.process.unwrap().into_parts();
    let (expected_endpoint, expected_frame) = expected.process.unwrap().into_parts();
    assert_eq!(endpoint.identity, identity);
    assert_eq!(endpoint.stamp.publication, 2);
    assert_eq!(endpoint.run_tick, 1);
    assert_eq!(endpoint.stamp, expected_endpoint.stamp);
    assert_eq!(frame, expected_frame);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    let count = |kind| events.iter().filter(|event| event.kind == kind).count();
    eprintln!(
        "terminal-source-work AppDerivation={} CommittedFrameCapture={}",
        count(Kind::AppDerivation),
        count(Kind::CommittedFrameCapture)
    );
    assert!(
        events
            .iter()
            .all(|event| event.purpose == Purpose::Predecessor)
    );
    assert!(events.iter().any(|event| matches!(event.key,
        Key::Publication { run, publication: 2 } if run == identity)));
    assert_eq!(count(Kind::CommittedFrameCapture), 2);
    assert_eq!(count(Kind::AppDerivation), 0);
}

#[test]
fn terminal_only_load_preserves_the_full_decoders_first_successor() {
    // Goal: discarding presentations keeps exact machine and App rates across the saved tail.
    // Method: seed independent real ring readers from the full and terminal-only decoders.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(57);
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal_minute(0);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let mut decoder = Decoder::default();
    drop(decoder.decode(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    ));
    let full = decoder.terminal().unwrap();
    let terminal = super::terminal(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )
    .unwrap()
    .unwrap();
    assert_eq!(terminal.run, full.run);
    assert_eq!(terminal.machine, full.machine);
    fixture.publish_with_system_rows(102);

    let expected = terminal_successor(&fixture.root, full);
    let resumed = terminal_successor(&fixture.root, terminal);

    assert!(expected.0.follows);
    assert_eq!(expected.0.cpu_percent, Some(50.0));
    assert_eq!(expected.1.elapsed_ns, Some(1_000_000_000));
    assert_eq!(resumed, expected);
    assert_eq!(budget.used(), 0);
}

fn terminal_successor(
    root: &std::path::Path,
    terminal: super::super::Terminal,
) -> (crate::store::MachineSecond, AppsSecond) {
    let mut ring = crate::store::ring::Ring::new(root.to_owned());
    ring.accept(Some(terminal));
    ring.follow().unwrap();
    let mut result = Vec::new();
    ring.read(|_, second, apps| {
        let mut owned = AppsSecond::with_room();
        owned.take(apps, None, None);
        result.push((second.clone(), owned));
        true
    })
    .unwrap();
    assert_eq!(result.len(), 1);

    result.remove(0)
}

#[test]
fn terminal_only_load_refuses_a_changed_consumed_file() {
    // Goal: a saved predecessor gets the same final commitment check as full decoding.
    // Method: make an admitted process table nonprivate before terminal decoding finishes.
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let processes = fixture
        .root
        .join(manifest.data_directory)
        .join("processes.parquet");
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let result = super::terminal(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &|file, budget| {
            let input = AdmittedTable::from_file_budgeted(file, budget)?;
            if input.contains_column("resident_bytes") {
                std::fs::set_permissions(&processes, std::fs::Permissions::from_mode(0o644))
                    .unwrap();
            }
            Ok(input)
        },
    );

    assert!(matches!(
        result,
        Err(crate::recording::Refused::Commitment(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn terminal_only_load_honors_cancellation_after_the_last_table_capture() {
    // Goal: cancellation cannot publish a predecessor even after its inputs were admitted.
    // Method: advance the operation epoch while capturing the minute's event table.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let result = super::terminal(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &|file, budget| {
            let input = AdmittedTable::from_file_budgeted(file, budget)?;
            if input.contains_column("event") {
                cancellation.cancel().unwrap();
            }
            Ok(input)
        },
    );

    assert!(matches!(
        result,
        Err(crate::recording::Refused::Decode(persistence::Error::Io(error)))
        if error.kind() == std::io::ErrorKind::Interrupted
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn terminal_only_load_refuses_a_malformed_earlier_frame_even_with_a_valid_last_frame() {
    // Goal: selecting the terminal frame does not skip semantic validation of earlier rows.
    // Method: duplicate a PID only at the first of two committed process observations.
    use polars::prelude::{col, lit, when};
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let processes = fixture
        .root
        .join(manifest.data_directory)
        .join("processes.parquet");
    let mut damaged = persistence::engine::scan(&processes)
        .unwrap()
        .with_column(
            when(col("publication").eq(lit(1_u64)))
                .then(lit(7_i32))
                .otherwise(col("pid"))
                .alias("pid"),
        )
        .collect()
        .unwrap();
    let replacement = fixture.root.join("damaged-processes.parquet");
    events_loader_write(&replacement, &mut damaged);
    std::fs::rename(replacement, processes).unwrap();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let result = super::terminal(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    );

    assert!(matches!(
        result,
        Err(crate::recording::Refused::Decode(
            persistence::Error::Bound("duplicate committed App PID")
        ))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn terminal_admission_preserves_presentation_refusal_and_recovery() {
    // Goal: an oversized presentation cannot erase its validated predecessor input.
    // Method: expand committed process tables to exceed the 8 MiB bound at each observation.
    for oversized_publication in 1..=2 {
        let fixture = terminal_bound_recording(oversized_publication);
        let mut found = Vec::new();
        super::super::find(&fixture.root, &mut found, None).unwrap();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
        let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
        let mut decoder = Decoder::default();
        let full = decoder.decode(
            &fixture.root,
            &found[0],
            &budget,
            &operation,
            &AdmittedTable::from_file_budgeted,
        );
        let terminal = super::terminal(
            &fixture.root,
            &found[0],
            &budget,
            &operation,
            &AdmittedTable::from_file_budgeted,
        )
        .unwrap()
        .unwrap();

        assert!(terminal.process.is_some());
        assert_eq!(terminal.process.is_some(), decoder.predecessor.is_some());
        if let Some(process) = terminal.process {
            let (endpoint, frame) = process.into_parts();
            let (expected, expected_frame) = decoder.predecessor.take().unwrap().into_parts();
            assert_eq!(endpoint.stamp, expected.stamp);
            assert_eq!(frame, expected_frame);
        }
        assert_eq!(
            full.seconds
                .last()
                .unwrap()
                .apps
                .as_ref()
                .unwrap()
                .missing
                .is_some(),
            oversized_publication == 2
        );
    }
}

fn terminal_bound_recording(oversized_publication: u64) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let processes = fixture
        .root
        .join(manifest.data_directory)
        .join("processes.parquet");
    let mut damaged = terminal_bound_rows(&processes, oversized_publication);
    let replacement = fixture.root.join("oversized-processes.parquet");
    events_loader_write(&replacement, &mut damaged);
    std::fs::rename(replacement, processes).unwrap();

    fixture
}

fn terminal_bound_rows(
    processes: &std::path::Path,
    oversized_publication: u64,
) -> polars::prelude::DataFrame {
    use polars::prelude::{IntoLazy, col, lit, when};
    let original = persistence::engine::scan(processes)
        .unwrap()
        .collect()
        .unwrap();
    let mut expanded = original.clone();
    for copy in 1..210 {
        let more = original
            .clone()
            .lazy()
            .with_columns([
                (col("pid") + lit(copy * 10)).alias("pid"),
                (col("process_unique_id") + lit(u64::try_from(copy * 10).unwrap()))
                    .alias("process_unique_id"),
            ])
            .collect()
            .unwrap();
        expanded.vstack_mut_owned(more).unwrap();
    }

    expanded
        .lazy()
        .with_column(
            when(col("publication").eq(lit(oversized_publication)))
                .then(lit(vec![255_u8; 4095]))
                .otherwise(col("executable_name"))
                .alias("executable_name"),
        )
        .collect()
        .unwrap()
}

#[test]
fn actual_minute_loader_reuses_capture_for_both_exact_sibling_consumers() {
    // Goal: the payload loader cannot discard admitted ticks and reopen a legacy minute.
    // Method: replace ticks inside its real capture factory, before both loader consumers.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let ticks = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let replacement = prepare_changed_ticks(&crate::recording::SealedSource {
        root: &fixture.root,
        marker: &found[0].marker,
    });
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut before = None;
    let captures = AtomicU64::new(0);

    let (machine, apps) = super::load_minute(
        &fixture.root,
        &found[0],
        &mut before,
        &budget,
        &operation,
        &|file, budget| {
            let input = AdmittedTable::from_file_budgeted(file, budget)?;
            if input.contains_column("wall_ns") {
                captures.fetch_add(1, Ordering::Relaxed);
                std::fs::rename(&replacement, &ticks).unwrap();
            }
            Ok(input)
        },
    )
    .unwrap();

    assert_loader_clocks(&machine, &apps, &fixture.writer.identity().unwrap());
    assert_eq!(captures.load(Ordering::Relaxed), 1);
    assert!(before.is_some());
    drop(apps);
    assert_eq!(budget.used(), 0);
}

fn assert_loader_clocks(
    machine: &crate::recording::MachineMinute,
    apps: &[AppsSecond],
    identity: &collector::ring::RunIdentity,
) {
    for second in apps {
        let processes = second.processes.as_ref().unwrap();
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].pid, 1);
        assert_eq!(processes[1].pid, 7);
        let observation = second.observation.unwrap();
        assert_eq!(observation.recording, identity.recording.0);
        assert_eq!(observation.stamp.run, identity.run.0);
        assert_eq!(observation.stamp.boot, identity.boot.0);
    }

    let machine: Vec<_> = machine
        .ticks
        .iter()
        .map(|tick| {
            (
                tick.publication,
                tick.run_tick,
                tick.wall_ns,
                tick.continuous_ns,
                tick.awake_ns,
            )
        })
        .collect();
    let processes: Vec<_> = apps
        .iter()
        .map(|apps| {
            let observation = apps.observation.unwrap();
            (
                observation.stamp.publication,
                observation.run_tick,
                observation.wall_ns,
                observation.stamp.continuous_ns,
                observation.stamp.awake_ns,
            )
        })
        .collect();
    let expected = [
        (1, 0, Some(100_000_000_000), Some(0), Some(0)),
        (
            2,
            1,
            Some(100_000_000_000),
            Some(1_000_000_000),
            Some(1_000_000_000),
        ),
    ];

    assert_eq!(machine, expected);
    assert_eq!(processes, expected);
}

fn prepare_changed_ticks(source: &crate::recording::SealedSource<'_>) -> std::path::PathBuf {
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{
        IntoLazy, ParquetCompression, ParquetWriter, StatisticsOptions, col, lit,
    };
    let manifest = persistence::read_committed(source.root, source.marker).unwrap();
    let path = source
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let mut frame = persistence::engine::scan(&path)
        .unwrap()
        .collect()
        .unwrap()
        .lazy()
        .with_columns([
            (col("wall_ns") + lit(10_000_000_000u64)).alias("wall_ns"),
            (col("continuous_ns") + lit(10_000_000_000u64)).alias("continuous_ns"),
            (col("awake_ns") + lit(10_000_000_000u64)).alias("awake_ns"),
        ])
        .collect()
        .unwrap();
    let replacement = source.root.join("changed-ticks.parquet");
    let file = collector::recording::private::open_private_file(&replacement, true).unwrap();

    ParquetWriter::new(file)
        .with_compression(ParquetCompression::Zstd(Some(
            ZstdLevel::try_new(3).unwrap(),
        )))
        .with_statistics(StatisticsOptions {
            min_value: true,
            max_value: true,
            distinct_count: false,
            null_count: true,
            binary_statistics_truncate_length: None,
        })
        .with_row_group_size(Some(16_384))
        .with_data_page_size(None)
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(&mut frame)
        .unwrap();

    replacement
}

#[test]
fn payload_loader_rejects_a_consumed_nonprivate_table_with_an_unchanged_commitment() {
    // Goal: acceptance must verify consumed files, not only reread the unchanged marker.
    // Method: make ticks nonprivate in the real loader's last process-consumer callback.
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let ticks = fixture
        .root
        .join(&manifest.data_directory)
        .join("ticks.parquet");
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut before = None;
    let mut consumed = Vec::new();

    let result = super::load_minute_visit(
        &fixture.root,
        &found[0],
        &mut before,
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
        |endpoint, sample, _losses| {
            assert!(sample.is_ok());
            consumed.push(endpoint.stamp.publication);
            if endpoint.stamp.publication == 2 {
                std::fs::set_permissions(&ticks, std::fs::Permissions::from_mode(0o644))?;
            }
            Ok(())
        },
    );

    assert_eq!(consumed, [1, 2]);
    assert_eq!(
        persistence::reread_committed(&fixture.root, &found[0].marker).unwrap(),
        manifest
    );
    assert!(matches!(
        result,
        Err(crate::recording::Refused::Commitment(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn payload_loader_refuses_shared_input_budget_without_legacy_retry() {
    // Goal: the actual opt-in loader cannot convert an input refusal into complete coverage.
    // Method: exhaust one shared budget before decoding a real sealed minute.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let budget = Arc::new(ByteBudget::new(1024));
    let charge = budget.reserve(1024).unwrap();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut before = None;

    let result = super::load_minute(
        &fixture.root,
        &found[0],
        &mut before,
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    );
    let mut decoder = Decoder::default();
    let batch = decoder.decode(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    );

    assert!(matches!(
        result,
        Err(crate::recording::Refused::Input(
            persistence::engine::AdmissionRefusal::Budget
        ))
    ));
    assert_eq!(batch.left_out, Some(crate::store::Failure::Refused));
    assert!(batch.seconds.is_empty());
    assert!(decoder.predecessor.is_none());
    assert_eq!(budget.used(), charge.bytes());
}

#[test]
fn a_sealed_boot_mismatch_cannot_seed_machine_rates_under_the_registry_identity() {
    // Goal: a committed minute from another boot cannot be labelled as the registry's run.
    // Method: change only its manifest boot, preserving the hard link, and overwrite its ring tick.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    change_committed_boot(&fixture.root, &found[0].marker);
    for second in 101..109 {
        fixture.publish_with_system_rows(second);
    }
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut accepted = Vec::new();

    let outcome = crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |_, second, _| accepted.push(second.publication),
    )
    .unwrap();
    let terminal_present = outcome.terminal.is_some();
    let mut ring = crate::store::ring::Ring::new(fixture.root.clone());
    ring.accept(outcome.terminal);
    ring.follow().unwrap();
    let mut successors = Vec::new();
    ring.read(|_, second, _| {
        successors.push(second.clone());
        true
    })
    .unwrap();

    assert_eq!(successors[0].publication, 2);
    assert_eq!(successors[0].cpu_percent, None);
    assert!(!successors[0].follows);
    assert!(accepted.is_empty());
    assert_eq!(outcome.left_out, Some(crate::store::Failure::Refused));
    assert!(!terminal_present);
    assert_eq!(successors[1].cpu_percent, Some(50.0));
    assert_eq!(budget.used(), 0);
}

fn change_committed_boot(root: &std::path::Path, marker: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;

    let original = persistence::read_committed(root, marker).unwrap();
    let metadata = root.join(&original.data_directory).join("metadata");
    let inode = std::fs::metadata(marker).unwrap().ino();
    assert_eq!(std::fs::metadata(&metadata).unwrap().ino(), inode);
    let mut bytes = std::fs::read(marker).unwrap();
    // Format 1 stores magic, schema/encoding words, then recording/run/boot identities.
    let boot =
        b"FDSEAL01".len() + 2 * size_of::<u64>() + 2 * size_of::<collector::ring::Identity>();
    assert_eq!(&bytes[..8], b"FDSEAL01");
    assert_eq!(
        &bytes[boot..boot + 8],
        &original.identity.run.boot.0[0].to_le_bytes()
    );
    bytes[boot] ^= 1;
    std::fs::write(marker, bytes).unwrap();

    let changed = persistence::read_committed(root, marker).unwrap();
    assert_eq!(
        changed.identity.run.recording,
        original.identity.run.recording
    );
    assert_eq!(changed.identity.run.run, original.identity.run.run);
    assert_ne!(changed.identity.run.boot, original.identity.run.boot);
    assert_eq!(std::fs::metadata(marker).unwrap().ino(), inode);
    assert_eq!(std::fs::metadata(metadata).unwrap().ino(), inode);
}

#[test]
fn frame_assembly_observes_the_callers_cancel_between_samples() {
    // Goal: frame assembly cannot continue under an unrelated uncancelled operation.
    // Method: cancel from the first of two real samples and require no second sample/baseline.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let sealed = crate::recording::SealedMinute::open(&fixture.root, &found[0].marker).unwrap();
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let mut before = None;
    let mut accepted = Vec::new();

    let result =
        sealed.visit_samples_with_operation(&mut before, &operation, |endpoint, sample| {
            assert!(sample.is_ok());
            accepted.push(endpoint.stamp.publication);
            cancellation.cancel()?;
            Ok(())
        });

    assert_eq!(accepted, [1]);
    assert!(
        matches!(result, Err(crate::recording::Refused::Decode(persistence::Error::Io(error)))
        if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert!(before.is_none());
}

#[test]
fn cancellation_in_one_minute_accepts_only_its_first_observation() {
    // Goal: cancellation within a completion releases its unconsumed observations.
    // Method: cancel the direct adapter's first callback in a real two-observation minute.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut accepted = Vec::new();

    let result = crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |_, second, apps| {
            let crate::store::apps::TickApps::Loaded(apps) = apps else {
                panic!("the real minute has complete payloads");
            };
            assert!(budget.used() > apps.processes.as_ref().unwrap().bytes());
            accepted.push(second.publication);
            cancellation.cancel().unwrap();
        },
    );

    assert_eq!(accepted, [1]);
    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_after_decode_releases_pending_payloads_without_consuming() {
    // Goal: a decoded completion cancelled before consumption cannot become retained state.
    // Method: decode a real minute, cancel its caller operation, then pass it to the consumer.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(160);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    let mut decoder = Decoder::default();
    let first = decoder.decode(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    );
    let mut retained = AppsSecond::with_room();
    load_consume(first, &operation, |_, _, apps| {
        retained.take(apps, None, Some(&budget));
    })
    .unwrap();
    let reader = Arc::new(retained);
    let charge = crate::store::startup_tests::retained_app_charge(&reader);
    let completion = decoder.decode(
        &fixture.root,
        &found[1],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    );
    assert!(budget.used() > charge);
    let mut accepted = 0;
    cancellation.cancel().unwrap();

    let outcome = load_consume(completion, &operation, |_, _, _| accepted += 1);

    assert_eq!(accepted, 0);
    assert!(matches!(outcome, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    drop(decoder);
    assert_eq!(budget.used(), charge);
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    assert_eq!(reader.processes.as_ref().unwrap().len(), 2);
    drop(reader);
    assert_eq!(budget.used(), 0);
}

#[test]
fn terminal_projection_keeps_only_the_last_raw_tick_and_all_its_tables() {
    // Goal: the terminal owner contains one raw tick even when its wall clock is unavailable.
    // Method: add each optional table to a real decoded two-tick minute and project its tail.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let mut decoder = Decoder::default();
    drop(decoder.decode(
        &fixture.root,
        &found[0],
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    ));
    decoder.previous.ticks[1].wall_ns = None;
    for publication in 1..=2 {
        decoder
            .previous
            .gpu_states
            .push(crate::recording::GpuStateRow {
                publication,
                state: 0,
                active_ns: publication,
            });
        decoder
            .previous
            .core_states
            .push(crate::recording::CoreStateRow {
                publication,
                core: 0,
                state: 0,
                active_ns: publication,
                frequency_khz: Some(1),
            });
        decoder.previous.fans.push(crate::recording::FanRow {
            publication,
            fan: 0,
            rpm: 1.0,
        });
    }

    let terminal = decoder.terminal().unwrap();

    assert_eq!(terminal.machine.ticks.len(), 1);
    assert_eq!(terminal.machine.ticks[0].publication, 2);
    assert_eq!(terminal.machine.ticks[0].wall_ns, None);
    assert_eq!(terminal.machine.cpus.len(), 1);
    assert_eq!(terminal.machine.cpus[0].publication, 2);
    assert_eq!(terminal.machine.memory.len(), 1);
    assert_eq!(terminal.machine.memory[0].publication, 2);
    assert_eq!(terminal.machine.interfaces.len(), 1);
    assert_eq!(terminal.machine.interfaces[0].publication, 2);
    assert_eq!(terminal.machine.gpu_states.len(), 1);
    assert_eq!(terminal.machine.gpu_states[0].publication, 2);
    assert_eq!(terminal.machine.core_states.len(), 1);
    assert_eq!(terminal.machine.core_states[0].publication, 2);
    assert_eq!(terminal.machine.fans.len(), 1);
    assert_eq!(terminal.machine.fans[0].publication, 2);
    assert_eq!(budget.used(), 0);
}
#[test]
fn actual_minute_loader_keeps_original_events_after_private_path_replacement() {
    // Goal: a complete load consumes the admitted events, including their original order.
    // Method: replace events after real admission, before the loader constructs its scan.
    let fixture = events_loader_fixture();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    assert_eq!(found.len(), 1);
    let source = crate::recording::SealedSource {
        root: &fixture.root,
        marker: &found[0].marker,
    };
    let manifest = persistence::read_committed(source.root, source.marker).unwrap();
    events_loader_make_ungrouped(&source);
    let replacement = events_loader_prepare_replacement(&source);
    let events = source
        .root
        .join(&manifest.data_directory)
        .join("events.parquet");
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let expected = events_loader_legacy(&source, &budget, &operation);
    let expected_captures = events_loader_consumed_inodes(&source);
    let mut before = None;
    let events_captures = AtomicU64::new(0);
    let captures = Mutex::new(Vec::new());

    let (machine, apps) = super::load_minute(
        source.root,
        &found[0],
        &mut before,
        &budget,
        &operation,
        &|file, budget| {
            use std::os::unix::fs::MetadataExt;
            let inode = file.metadata().unwrap().ino();
            let input = AdmittedTable::from_file_budgeted(file, budget)?;
            captures.lock().unwrap().push(inode);
            if input.contains_column("event") {
                events_captures.fetch_add(1, Ordering::Relaxed);
                std::fs::rename(&replacement, &events).unwrap();
            }

            Ok(input)
        },
    )
    .unwrap();

    events_loader_assert_original(&machine, &apps, &fixture.writer.identity().unwrap());
    assert_eq!(machine, expected.0);
    assert_eq!(apps, expected.1);
    assert_eq!(events_captures.load(Ordering::Relaxed), 1);
    let mut captures = captures.into_inner().unwrap();
    captures.sort_unstable();
    assert_eq!(captures, expected_captures);
    assert!(before.is_some());
    assert_eq!(
        persistence::read_committed(source.root, source.marker).unwrap(),
        manifest
    );
}

fn events_loader_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_events(
        100,
        &[
            collector::tracker::Event::Start {
                pid: 9,
                coalition: Some(3),
            },
            collector::tracker::Event::Exec { pid: 8 },
        ],
    );
    fixture.publish_with_events(100, &[events_loader_exit()]);
    fixture.seal();

    fixture
}

fn events_loader_consumed_inodes(source: &crate::recording::SealedSource<'_>) -> Vec<u64> {
    use std::os::unix::fs::MetadataExt;
    let manifest = persistence::read_committed(source.root, source.marker).unwrap();
    let directory = source.root.join(manifest.data_directory);
    let mut inodes: Vec<_> = [
        "ticks",
        "cpus",
        "memory",
        "interfaces",
        "gpu_states",
        "core_states",
        "fans",
        "processes",
        "events",
    ]
    .iter()
    .map(|name| {
        std::fs::metadata(directory.join(format!("{name}.parquet")))
            .unwrap()
            .ino()
    })
    .collect();
    inodes.sort_unstable();

    inodes
}

fn events_loader_exit() -> collector::tracker::Event {
    collector::tracker::Event::Exit {
        pid: 9,
        last: Ok(collector::processes::Usage {
            awake_ns_at_start: 1,
            user_ns: 2,
            system_ns: 3,
            energy_nj: 4,
            instructions: 5,
            super_instructions: 6,
            cycles: 7,
            idle_wakeups: 8,
            interrupt_wakeups: 9,
            pageins: 10,
            resident_bytes: 11,
            footprint_bytes: 12,
            disk_read_bytes: 13,
            disk_written_bytes: 14,
        }),
        noticed: collector::tracker::Noticed::OnExit,
    }
}

fn events_loader_make_ungrouped(source: &crate::recording::SealedSource<'_>) {
    use polars::prelude::{DataType, Null, col, lit, when};
    let manifest = persistence::read_committed(source.root, source.marker).unwrap();
    let path = source
        .root
        .join(manifest.data_directory)
        .join("processes.parquet");
    let mut frame = persistence::engine::scan(&path)
        .unwrap()
        .with_columns([
            when(col("pid").eq(lit(1_i32)))
                .then(lit(persistence::schema::identity_availability::UNSUPPORTED))
                .otherwise(col("identity_availability"))
                .alias("identity_availability"),
            when(col("pid").eq(lit(1_i32)))
                .then(lit(Null {}).cast(DataType::Binary))
                .otherwise(col("executable_name"))
                .alias("executable_name"),
            when(col("pid").eq(lit(1_i32)))
                .then(lit(Null {}).cast(DataType::Binary))
                .otherwise(col("app_path"))
                .alias("app_path"),
        ])
        .collect()
        .unwrap();
    let replacement = source.root.join("ungrouped-processes.parquet");
    events_loader_write(&replacement, &mut frame);
    std::fs::rename(replacement, path).unwrap();
}

fn events_loader_prepare_replacement(
    source: &crate::recording::SealedSource<'_>,
) -> std::path::PathBuf {
    use polars::prelude::{IntoLazy, UInt64Type, col, lit};
    let manifest = persistence::read_committed(source.root, source.marker).unwrap();
    let path = source
        .root
        .join(manifest.data_directory)
        .join("events.parquet");
    let original = persistence::engine::scan(&path).unwrap().collect().unwrap();
    let numbers = persistence::engine::Cells::<UInt64Type>::named(&original, "event").unwrap();
    let numbers: Vec<_> = (0..original.height())
        .map(|row| numbers.get(row).unwrap())
        .collect();
    assert_eq!(numbers, [2, 3, 1]);
    let mut changed = original
        .lazy()
        .with_columns([(col("pid") + lit(10_i32)).alias("pid")])
        .collect()
        .unwrap();
    let replacement = source.root.join("changed-events.parquet");
    events_loader_write(&replacement, &mut changed);

    replacement
}

fn events_loader_write(path: &std::path::Path, frame: &mut polars::prelude::DataFrame) {
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{ParquetCompression, ParquetWriter, StatisticsOptions};

    let file = collector::recording::private::open_private_file(path, true).unwrap();

    ParquetWriter::new(file)
        .with_compression(ParquetCompression::Zstd(Some(
            ZstdLevel::try_new(3).unwrap(),
        )))
        .with_statistics(StatisticsOptions {
            min_value: true,
            max_value: true,
            distinct_count: false,
            null_count: true,
            binary_statistics_truncate_length: None,
        })
        .with_row_group_size(Some(16_384))
        .with_data_page_size(None)
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(frame)
        .unwrap();
}

fn events_loader_legacy(
    source: &crate::recording::SealedSource<'_>,
    budget: &Arc<ByteBudget>,
    operation: &crate::cancellation::Operation,
) -> (crate::recording::MachineMinute, Vec<AppsSecond>) {
    let sealed = crate::recording::SealedMinute::open(source.root, source.marker).unwrap();
    let mut machine = crate::recording::MachineMinute::default();
    crate::recording::read_machine(&sealed, &mut machine).unwrap();
    let mut before = None;
    let mut apps = Vec::new();
    sealed
        .visit_samples_with_operation(&mut before, operation, |_, sample| {
            let sample = sample.unwrap();
            let mut second = AppsSecond::with_room_for(sample.apps().len());
            second.take(
                crate::store::apps::TickApps::Derived(sample),
                apps.last(),
                Some(budget),
            );
            second.metadata.origin = crate::store::native::Origin::Saved(*sealed.losses());
            apps.push(second);

            Ok(())
        })
        .unwrap();
    sealed.revalidate_full().unwrap();

    (machine, apps)
}

fn events_loader_assert_observations(
    machine: &crate::recording::MachineMinute,
    apps: &[AppsSecond],
    identity: &collector::ring::RunIdentity,
) {
    use crate::apps::Stamp;
    use crate::live_app_response::Observation;
    assert_eq!(apps.len(), 2);
    let observations: Vec<_> = apps
        .iter()
        .map(|second| second.observation.unwrap())
        .collect();
    let expected: Vec<_> = (0..2_u64)
        .map(|run_tick| Observation {
            recording: identity.recording.0,
            stamp: Stamp {
                boot: identity.boot.0,
                run: identity.run.0,
                publication: run_tick + 1,
                continuous_ns: Some(run_tick * 1_000_000_000),
                awake_ns: Some(run_tick * 1_000_000_000),
            },
            run_tick,
            wall_ns: Some(100_000_000_000),
        })
        .collect();
    assert_eq!(observations, expected);
    let clocks: Vec<_> = machine
        .ticks
        .iter()
        .map(|tick| {
            (
                tick.publication,
                tick.run_tick,
                tick.wall_ns,
                tick.continuous_ns,
                tick.awake_ns,
            )
        })
        .collect();
    assert_eq!(
        clocks,
        [
            (1, 0, Some(100_000_000_000), Some(0), Some(0)),
            (
                2,
                1,
                Some(100_000_000_000),
                Some(1_000_000_000),
                Some(1_000_000_000)
            ),
        ]
    );
}

fn events_loader_assert_original(
    machine: &crate::recording::MachineMinute,
    apps: &[AppsSecond],
    identity: &collector::ring::RunIdentity,
) {
    use crate::apps::ProcessState;
    events_loader_assert_observations(machine, apps, identity);
    let first = apps[0].processes.as_ref().unwrap();
    let second = apps[1].processes.as_ref().unwrap();
    assert_eq!(first.iter().map(|row| row.pid).collect::<Vec<_>>(), [1, 7]);
    assert_eq!(
        second.iter().map(|row| row.pid).collect::<Vec<_>>(),
        [1, 7, 9]
    );
    assert_eq!(apps[0].ungrouped_members, 1);
    assert_eq!(apps[1].ungrouped_members, 1);
    assert_eq!(first[0].app_index, None);
    assert_eq!(second[0].app_index, None);
    assert_eq!(
        second[0].program_unavailable,
        Some(collector::ring::IdentityUnavailable::Unsupported)
    );
    assert_eq!(
        second[2].creation,
        Some(collector::ring::ProcessCreation {
            unique_id: 900,
            pid_version: -90,
        })
    );
    assert_eq!(second[2].state, ProcessState::ExitedVerified);
    assert_eq!(second[2].executable_name().unwrap().bytes, b"departed\xfd");
}

#[test]
fn events_capture_budget_denial_remains_typed_without_legacy_retry() {
    // Goal: a later table cannot bypass admission when the shared input budget is full.
    // Method: exhaust remaining capacity just before the actual events constructor.
    use std::os::unix::fs::MetadataExt;
    let fixture = events_loader_fixture();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let events = fixture
        .root
        .join(manifest.data_directory)
        .join("events.parquet");
    let inode = std::fs::metadata(events).unwrap().ino();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let held = Mutex::new(None);
    let attempts = AtomicU64::new(0);
    let mut before = None;

    let result = super::load_minute(
        &fixture.root,
        &found[0],
        &mut before,
        &budget,
        &operation,
        &|file, budget| {
            if file.metadata().unwrap().ino() == inode {
                attempts.fetch_add(1, Ordering::Relaxed);
                *held.lock().unwrap() =
                    Some(budget.reserve(budget.maximum() - budget.used()).unwrap());
            }

            AdmittedTable::from_file_budgeted(file, budget)
        },
    );

    assert!(matches!(
        result,
        Err(crate::recording::Refused::Input(
            persistence::engine::AdmissionRefusal::Budget
        ))
    ));
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    assert!(before.is_none());
    let held = held.into_inner().unwrap().unwrap();
    assert_eq!(budget.used(), held.bytes());
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_after_real_events_capture_discards_the_input_and_all_results() {
    // Goal: cancelled events admission cannot seed typed results or a predecessor.
    // Method: cancel inside the forwarding factory after constructing the real owner.
    let fixture = events_loader_fixture();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let mut before = None;
    let captures = AtomicU64::new(0);

    let result = super::load_minute(
        &fixture.root,
        &found[0],
        &mut before,
        &budget,
        &operation,
        &|file, budget| {
            let input = AdmittedTable::from_file_budgeted(file, budget)?;
            if input.contains_column("event") {
                captures.fetch_add(1, Ordering::Relaxed);
                cancellation.cancel().unwrap();
            }

            Ok(input)
        },
    );

    assert!(matches!(result, Err(crate::recording::Refused::Decode(
        persistence::Error::Io(error))) if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(captures.load(Ordering::Relaxed), 1);
    assert!(before.is_none());
    assert_eq!(budget.used(), 0);
}

#[test]
fn final_validation_rejects_consumed_events_made_nonprivate() {
    // Goal: immutable captured events cannot excuse a later violation of file privacy.
    // Method: make events nonprivate in the final real loader consumer with its marker intact.
    use std::os::unix::fs::PermissionsExt;
    let fixture = events_loader_fixture();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    let events = fixture
        .root
        .join(&manifest.data_directory)
        .join("events.parquet");
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut before = None;
    let mut consumed = Vec::new();

    let result = super::load_minute_visit(
        &fixture.root,
        &found[0],
        &mut before,
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
        |endpoint, sample, _losses| {
            assert!(sample.is_ok());
            consumed.push(endpoint.stamp.publication);
            if endpoint.stamp.publication == 2 {
                std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o644))?;
            }

            Ok(())
        },
    );

    assert_eq!(consumed, [1, 2]);
    assert_eq!(
        persistence::reread_committed(&fixture.root, &found[0].marker).unwrap(),
        manifest
    );
    assert!(matches!(
        result,
        Err(crate::recording::Refused::Commitment(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn catch_up_keeps_only_unseen_minutes_and_their_predecessor_for_decoding() {
    // Goal: a show does not decode the already retained hour again.
    // Method: seal three real minutes and select new work after the second minute's endpoint.
    let fixture = catch_up_fixture();
    let newest = crate::store::Newest {
        run: fixture.writer.identity().unwrap(),
        publication: 3,
        segment: 0,
    };
    let mut found = Vec::new();

    super::super::find(&fixture.root, &mut found, Some(&newest)).unwrap();

    let endpoints: Vec<_> = found.iter().map(|minute| minute.publication_end).collect();
    assert_eq!(endpoints, [5, 4]);
}

#[test]
fn incremental_catch_up_preserves_rates_at_the_new_minute_boundary() {
    // Goal: removing old decode work cannot erase the first new observation's predecessor.
    // Method: load after publication 3 and check publication 4's real machine and process rates.
    let fixture = catch_up_fixture();
    let newest = crate::store::Newest {
        run: fixture.writer.identity().unwrap(),
        publication: 3,
        segment: 0,
    };
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut selected = None;

    super::super::load_payload_since(
        &fixture.root,
        &budget,
        &operation,
        Some(&newest),
        |_, second, apps| {
            if second.publication == 4 {
                let crate::store::apps::TickApps::Loaded(apps) = apps else {
                    panic!("the fixture supplies complete process payloads");
                };
                selected = Some((
                    second.clone(),
                    std::mem::replace(apps, AppsSecond::with_room()),
                ));
            }
        },
    )
    .unwrap();

    let (machine, apps) = selected.unwrap();
    assert!(machine.follows);
    assert_eq!(machine.cpu_percent, Some(50.0));
    assert_eq!(apps.elapsed_ns, Some(1_000_000_000));
    assert_eq!(
        apps.processes
            .as_ref()
            .unwrap()
            .previous
            .unwrap()
            .publication,
        3
    );
}

fn catch_up_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(160);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(219);
    fixture.seal_minute(1);
    fixture.publish_with_system_rows(220);
    fixture.seal_minute(2);
    fixture
}
