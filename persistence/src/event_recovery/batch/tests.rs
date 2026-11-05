use collector::processes::Usage;
use polars::prelude::*;

use super::{BATCHES_MAX, Row, USAGE_COLUMNS, frame, framed, read_ahead, validate_frame};
use crate::Error;
use crate::encoder::usage_columns;
use crate::schema::{availability, event_kind, noticed};
use crate::window::EventRecord;

#[test]
fn read_ahead_writes_each_item_in_the_order_it_was_read() {
    // Goal: the second thread changes nothing about which batch is written after which.
    // Method: read five items, and collect what the write side receives.
    let mut written = Vec::with_capacity(5);

    let result = read_ahead(
        |items| {
            for item in 0..5 {
                items.send(item).unwrap();
            }
            Ok(())
        },
        |item| {
            written.push(item);
            Ok(())
        },
    );

    assert!(result.is_ok());
    assert_eq!(written, [0, 1, 2, 3, 4]);
}

#[test]
fn a_write_error_stops_the_reading_and_is_the_error_returned() {
    // Goal: a refused write neither leaves the reading thread waiting to send nor loses its cause
    // to the error the reading thread ends with.
    // Method: refuse the first item while the reading side sends until a send fails, then returns
    // an error of its own.
    let result = read_ahead(
        |items| {
            for item in 0..=BATCHES_MAX {
                if items.send(item).is_err() {
                    return Err(Error::Bound("read"));
                }
            }
            Ok(())
        },
        |_item| Err(Error::Bound("write")),
    );

    assert!(matches!(result, Err(Error::Bound("write"))));
}

#[test]
fn a_read_error_arrives_after_every_item_sent_before_it_is_written() {
    // Goal: batches read before a failure stay saved, so a retry resumes after them.
    // Method: send two items, end the reading with an error, and collect what was written.
    let mut written = Vec::with_capacity(2);

    let result = read_ahead(
        |items| {
            items.send(0).unwrap();
            items.send(1).unwrap();
            Err(Error::Bound("read"))
        },
        |item| {
            written.push(item);
            Ok(())
        },
    );

    assert!(matches!(result, Err(Error::Bound("read"))));
    assert_eq!(written, [0, 1]);
}

/// Reads this many items, stopping early if a send fails, and counts the ones written.
fn read_ahead_counted(count: usize) -> (crate::Result<()>, usize) {
    let mut written = 0;
    let result = read_ahead(
        |items| {
            for item in 0..count {
                if items.send(item).is_err() {
                    return Ok(());
                }
            }
            Ok(())
        },
        |_item| {
            written += 1;
            Ok(())
        },
    );
    (result, written)
}

#[test]
fn the_most_batches_a_tail_holds_are_all_written() {
    // Goal: the bound on reading ahead refuses no tail the format allows.
    // Method: read exactly the most batches, and count what was written.
    let (result, written) = read_ahead_counted(BATCHES_MAX);

    assert!(result.is_ok());
    assert_eq!(written, BATCHES_MAX);
}

#[test]
fn a_batch_past_the_most_a_tail_holds_is_refused_and_not_written() {
    // Goal: an item past the bound is neither written nor dropped without a word.
    // Method: read one more than the most batches, and read the result and the count.
    let (result, written) = read_ahead_counted(BATCHES_MAX + 1);

    assert!(matches!(
        result,
        Err(Error::Bound("event batches read ahead"))
    ));
    assert_eq!(written, BATCHES_MAX);
}

fn measured_exit() -> Row {
    Row {
        publication: None,
        event: EventRecord {
            number: 1,
            tick: 1,
            pid: 41,
            kind: event_kind::EXIT,
            coalition: None,
            usage: Some(Usage {
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
            availability: availability::MEASURED,
            noticed: Some(noticed::ON_EXIT),
        },
        identity: None,
        name: None,
        app: None,
    }
}

#[test]
fn a_batch_whose_rows_break_the_event_rules_is_refused_before_anything_writes_it() {
    // Goal: the check that protects the recording still runs before a batch leaves the reading
    // thread for its file.
    // Method: frame a measured exit that claims no moment it was noticed.
    let mut row = measured_exit();
    row.event.noticed = None;

    let result = framed(&[row], false);

    assert!(matches!(
        result,
        Err(Error::Bound("event numeric semantics"))
    ));
}

#[test]
fn every_usage_value_missing_from_a_measured_exit_is_refused() {
    // Goal: resolving the usage columns once still checks every usage value of a row.
    // Method: take each column the encoder writes for usage, clear it, and validate the frame.
    let rows = [measured_exit()];
    let usage = usage_columns(&rows, |row| row.event.usage.as_ref());
    assert_eq!(usage.len(), USAGE_COLUMNS.len());

    for column in usage {
        let name = column.name().clone();
        let mut cleared = frame(&rows, false).unwrap();
        let null = Column::full_null(name.clone(), 1, &DataType::UInt64);
        cleared.with_column(null).unwrap();

        let result = validate_frame(&cleared, false);

        assert!(
            matches!(result, Err(Error::Bound("event usage payload"))),
            "{name}"
        );
    }
}
