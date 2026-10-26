use collector::disks::{DISKS_MAX, DiskReading, DiskSnapshot, ScanState};
use persistence::{Error, Result};
use serde_json::{Value, json};

use super::{compatible, get, number, only};
use crate::metric_view::{Sources, View};
use crate::session::Observation;

const RATE_FIELDS: [&str; 8] = [
    "read_bps",
    "write_bps",
    "read_ops_per_second",
    "write_ops_per_second",
    "read_errors_per_second",
    "write_errors_per_second",
    "read_mean_ms",
    "write_mean_ms",
];

pub(super) fn point(
    tables: &Sources<'_>,
    current: &Observation,
    previous: Option<&Observation>,
) -> Result<Value> {
    let mut snapshot = DiskSnapshot::default();
    if !point_source(
        &get(tables, current, "ticks")?,
        current.continuous,
        &mut snapshot,
    )? {
        return Ok(Value::Null);
    }

    let mut previous_snapshot = DiskSnapshot::default();
    let baseline = if let Some(previous) = previous.filter(|before| compatible(before, current)) {
        if point_source(
            &get(tables, previous, "ticks")?,
            previous.continuous,
            &mut previous_snapshot,
        )? {
            Some(&previous_snapshot)
        } else {
            None
        }
    } else {
        None
    };
    let elapsed = baseline
        .and_then(|before| {
            snapshot
                .source_continuous_ns
                .checked_sub(before.source_continuous_ns)
        })
        .filter(|elapsed| *elapsed > 0);
    let mut devices = Vec::with_capacity(snapshot.count);
    let mut deltas = [[None; 8]; DISKS_MAX];
    for (index, record) in snapshot.records[..snapshot.count].iter().enumerate() {
        let before = baseline.and_then(|before| {
            before.records[..before.count]
                .iter()
                .find(|old| identity(old) == identity(record))
        });
        if elapsed.is_some() {
            deltas[index] = point_delta(record, before);
        }
        let mut device = point_rates(&deltas[index], elapsed);
        device["driver_id"] = json!(record.driver_id);
        device["media_id"] = json!(record.media_id);
        device["name"] = json!(point_device_name(record)?);
        devices.push(device);
    }

    let totals = point_totals(&snapshot, baseline, &deltas);
    let mut value = point_rates(&totals, elapsed);
    value["state"] = json!(match snapshot.state {
        ScanState::Complete => "complete",
        ScanState::Failed => "failed",
        ScanState::Exhausted => "exhausted",
        ScanState::Partial => "partial",
    });
    value["devices"] = json!(devices);
    Ok(value)
}

fn point_source(table: &View<'_>, time: Option<u64>, snapshot: &mut DiskSnapshot) -> Result<bool> {
    *snapshot = DiskSnapshot::default();
    if !table.disks(only(table)?, snapshot)? {
        return Ok(false);
    }

    if Some(snapshot.source_continuous_ns) != time {
        *snapshot = DiskSnapshot::default();
        return Ok(false);
    }

    Ok(true)
}

fn point_device_name(record: &DiskReading) -> Result<&str> {
    let end = record
        .name
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::Bound("physical disk name terminator"))?;
    std::str::from_utf8(&record.name[..end])
        .map_err(|_| Error::Bound("physical disk name encoding"))
}

fn identity(record: &DiskReading) -> (u64, u64) {
    (record.driver_id, record.media_id)
}

fn point_delta_counters(record: &DiskReading) -> [Option<u64>; 8] {
    [
        record.read_bytes,
        record.write_bytes,
        record.read_operations,
        record.write_operations,
        record.read_errors,
        record.write_errors,
        record.read_time_ns,
        record.write_time_ns,
    ]
}

fn point_delta(current: &DiskReading, previous: Option<&DiskReading>) -> [Option<u64>; 8] {
    let Some(previous) = previous else {
        return [None; 8];
    };

    if current.failed {
        return [None; 8];
    }

    if previous.failed {
        return [None; 8];
    }

    let mut deltas = [None; 8];
    for (index, (now, before)) in point_delta_counters(current)
        .into_iter()
        .zip(point_delta_counters(previous))
        .enumerate()
    {
        deltas[index] = now
            .zip(before)
            .and_then(|(now, before)| now.checked_sub(before));
    }

    for (duration, operations) in [(6, 2), (7, 3)] {
        if deltas[operations].is_some_and(|operations| operations > 0) {
            if deltas[duration] == Some(0) {
                deltas[duration] = None;
            }
        }
    }

    deltas
}

fn point_totals(
    current: &DiskSnapshot,
    previous: Option<&DiskSnapshot>,
    deltas: &[[Option<u64>; 8]; DISKS_MAX],
) -> [Option<u64>; 8] {
    let Some(previous) = previous else {
        return [None; 8];
    };

    if current.state != ScanState::Complete {
        return [None; 8];
    }

    if previous.state != ScanState::Complete {
        return [None; 8];
    }

    if current.count != previous.count {
        return [None; 8];
    }

    for record in &current.records[..current.count] {
        if !previous.records[..previous.count]
            .iter()
            .any(|old| identity(old) == identity(record))
        {
            return [None; 8];
        }
    }

    let mut totals = [Some(0_u64); 8];
    for delta in &deltas[..current.count] {
        for (total, delta) in totals.iter_mut().zip(delta).skip(2) {
            *total = total
                .zip(*delta)
                .and_then(|(sum, delta)| sum.checked_add(delta));
        }
    }

    let bytes = byte_deltas(
        &current.records[..current.count],
        &previous.records[..previous.count],
    );
    totals[0] = bytes[0];
    totals[1] = bytes[1];
    totals
}

/// The same complete-device totals the detailed Storage summary uses, before presentation.
pub(crate) fn byte_deltas(current: &[DiskReading], previous: &[DiskReading]) -> [Option<u64>; 2] {
    if current.len() != previous.len() {
        return [None; 2];
    }
    let mut totals = [Some(0_u64); 2];
    for record in current {
        let Some(before) = previous
            .iter()
            .find(|before| identity(before) == identity(record))
        else {
            return [None; 2];
        };
        let deltas = point_delta(record, Some(before));
        for (total, delta) in totals.iter_mut().zip(deltas) {
            *total = total
                .zip(delta)
                .and_then(|(total, delta)| total.checked_add(delta));
        }
    }
    totals
}

struct CompletedRequests {
    duration_ns: Option<u64>,
    operations: Option<u64>,
}

fn point_rates(deltas: &[Option<u64>; 8], elapsed: Option<u64>) -> Value {
    let mut value = json!({});
    for (index, name) in RATE_FIELDS[..6].iter().enumerate() {
        value[*name] = json!(
            deltas[index]
                .zip(elapsed)
                .map(|(delta, elapsed)| number(delta) * 1_000_000_000.0 / number(elapsed))
        );
    }
    value[RATE_FIELDS[6]] = json!(point_rates_mean(&CompletedRequests {
        duration_ns: deltas[6],
        operations: deltas[2],
    }));
    value[RATE_FIELDS[7]] = json!(point_rates_mean(&CompletedRequests {
        duration_ns: deltas[7],
        operations: deltas[3],
    }));
    value
}

fn point_rates_mean(completed: &CompletedRequests) -> Option<f64> {
    let (duration, operations) = completed.duration_ns.zip(completed.operations)?;
    if operations == 0 {
        return None;
    }

    if duration == 0 {
        return None;
    }

    Some(number(duration) / number(operations) / 1_000_000.0)
}
