use std::io::{self, Write};

use serde_json::{Value, json};

use super::{Amount, App, Detail, Error, Measure, Page, Process, Result, Sample, Stamp};
use crate::apps;

pub(super) fn apps_page(sample: &Sample, page: Page) -> Result<Value> {
    let range = page_range(page, sample.apps.len())?;
    let mut result = header(sample);
    let values: Vec<_> = sample.apps[range.clone()]
        .iter()
        .enumerate()
        .map(|(index, value)| app(value, range.start + index, sample.elapsed_ns))
        .collect();
    result["apps"] = json!(values);
    result["page"] = page_json(page, range.end, sample.apps.len());

    bounded(result)
}

pub(super) fn app_detail(sample: &Sample, detail: &Detail<'_>) -> Result<Value> {
    let index = usize::try_from(detail.app_index).map_err(|_| Error::Bound("live App index"))?;
    let value = sample
        .apps
        .get(index)
        .ok_or(Error::Bound("live App index"))?;
    let mut result = members_page(sample, Some(detail.app_index), detail.page)?;
    result["app"] = app(value, index, sample.elapsed_ns);

    bounded(result)
}

pub(super) fn members_page(sample: &Sample, app_index: Option<u32>, page: Page) -> Result<Value> {
    let count = sample
        .processes
        .iter()
        .filter(|value| value.app_index == app_index)
        .count();
    let range = page_range(page, count)?;
    let values: Vec<_> = sample
        .processes
        .iter()
        .filter(|value| value.app_index == app_index)
        .skip(range.start)
        .take(range.len())
        .map(|value| process(value, sample.elapsed_ns))
        .collect();
    let mut result = header(sample);
    result["processes"] = json!(values);
    result["page"] = page_json(page, range.end, count);

    bounded(result)
}

fn header(sample: &Sample) -> Value {
    json!({
        "selected": observation(&sample.selected),
        "previous": sample.previous.as_ref().map(stamp),
        "elapsed_ns": sample.elapsed_ns.map(|value| value.to_string()),
        "membership_complete": sample.membership_complete,
        "ungrouped_members": sample.ungrouped_members,
        "usage_refused": sample.usage_refused,
        "usage_gone": sample.usage_gone,
        "app_count": sample.apps.len(),
        "process_count": sample.processes.len(),
    })
}

pub(super) fn observation(value: &super::Observation) -> Value {
    json!({
        "recording": value.recording.map(|value| value.to_string()),
        "stamp": stamp(&value.stamp),
        "run_tick": value.run_tick.to_string(),
        "wall_ns": value.wall_ns.map(|value| value.to_string()),
    })
}

fn stamp(value: &Stamp) -> Value {
    json!({
        "boot": value.boot.map(|value| value.to_string()),
        "run": value.run.map(|value| value.to_string()),
        "publication": value.publication.to_string(),
        "continuous_ns": value.continuous_ns.map(|value| value.to_string()),
        "awake_ns": value.awake_ns.map(|value| value.to_string()),
    })
}

fn app(value: &App, index: usize, elapsed_ns: Option<u64>) -> Value {
    json!({
        "app_index": index, "display_name": value.display_name,
        "key": {
            "kind": match value.key.kind {
                super::KeyKind::Bundle => "bundle", super::KeyKind::Executable => "executable",
            },
            "bytes_hex": hex(&value.key.bytes), "display": value.key.display,
        },
        "members": value.members, "live_members": value.live_members,
        "membership_complete": value.membership_complete,
        "cpu_ns": measure(&value.cpu_ns), "energy_nj": measure(&value.energy_nj),
        "disk_read_bytes": measure(&value.disk_read_bytes),
        "disk_written_bytes": measure(&value.disk_written_bytes),
        "disk_io_bytes": measure(&value.disk_io_bytes),
        "disk_read_bps": disk_rate(value.disk_read_bytes.measured(), elapsed_ns),
        "disk_written_bps": disk_rate(value.disk_written_bytes.measured(), elapsed_ns),
        "disk_io_bps": disk_rate(value.disk_io_bytes.measured(), elapsed_ns),
        "resident_bytes": measure(&value.resident_bytes),
        "footprint_bytes": measure(&value.footprint_bytes),
        "cpu_percent_one_core": value.cpu_ns.measured().and_then(|cpu_ns| {
            apps::cpu_percent_one_core(apps::CpuRate { cpu_ns, elapsed_ns: elapsed_ns? })
        }),
        "energy_joules": value.energy_nj.measured().map(apps::energy_joules),
        "energy_watts": value.energy_nj.measured().and_then(|energy_nj| {
            apps::energy_watts(apps::EnergyRate { energy_nj, elapsed_ns: elapsed_ns? })
        }),
    })
}

fn process(value: &Process, elapsed_ns: Option<u64>) -> Value {
    json!({
        "pid": value.pid,
        "creation": value.creation.map(|creation| json!({
            "unique_id": creation.unique_id.to_string(), "pid_version": creation.pid_version,
        })),
        "app_index": value.app_index,
        "executable_name": value.executable_name.as_ref().map(|name| json!({
            "bytes_hex": hex(&name.bytes), "display": name.display,
        })),
        "program_unavailable": value.program_unavailable.map(identity_reason),
        "cpu_ns": amount(value.cpu_ns), "energy_nj": amount(value.energy_nj),
        "disk_read_bytes": amount(value.disk_read_bytes),
        "disk_written_bytes": amount(value.disk_written_bytes),
        "disk_io_bytes": amount(value.disk_io_bytes),
        "disk_read_bps": disk_rate(known(value.disk_read_bytes), elapsed_ns),
        "disk_written_bps": disk_rate(known(value.disk_written_bytes), elapsed_ns),
        "disk_io_bps": disk_rate(known(value.disk_io_bytes), elapsed_ns),
        "resident_bytes": amount(value.resident_bytes),
        "footprint_bytes": amount(value.footprint_bytes),
        "idle_wakeups": amount(value.idle_wakeups),
        "pageins": amount(value.pageins),
        "threads": amount(value.threads),
        "idle_wakeups_per_second": process_event_rate(&OptionalEventRate {
            events: known(value.idle_wakeups),
            elapsed_ns,
        }),
        "pageins_per_second": process_event_rate(&OptionalEventRate {
            events: known(value.pageins),
            elapsed_ns,
        }),
        "app_attribution": value.app_attribution.map(reason),
        "state": match value.state {
            apps::ProcessState::Live => "live",
            apps::ProcessState::ExitedVerified => "exited_verified",
            apps::ProcessState::ExitedUnverified => "exited_unverified",
        },
        "cpu_percent_one_core": known(value.cpu_ns).and_then(|cpu_ns| {
            apps::cpu_percent_one_core(apps::CpuRate { cpu_ns, elapsed_ns: elapsed_ns? })
        }),
        "energy_joules": known(value.energy_nj).map(apps::energy_joules),
        "energy_watts": known(value.energy_nj).and_then(|energy_nj| {
            apps::energy_watts(apps::EnergyRate { energy_nj, elapsed_ns: elapsed_ns? })
        }),
    })
}

struct OptionalEventRate {
    events: Option<u64>,
    elapsed_ns: Option<u64>,
}

fn process_event_rate(rate: &OptionalEventRate) -> Option<f64> {
    apps::events_per_second(apps::EventRate {
        events: rate.events?,
        elapsed_ns: rate.elapsed_ns?,
    })
}

fn disk_rate(bytes: Option<u64>, elapsed_ns: Option<u64>) -> Option<f64> {
    apps::bytes_per_second(apps::BytesRate {
        bytes: bytes?,
        elapsed_ns: elapsed_ns?,
    })
}

pub(super) fn measure(value: &Measure) -> Value {
    json!({ "known": value.known.to_string(), "measured_members": value.measured_members,
        "unavailable_members": value.unavailable_members, "overflowed": value.overflowed })
}

fn amount(value: Amount) -> Value {
    match value {
        Amount::Known(value) => json!({ "known": value.to_string(), "unavailable": null }),
        Amount::Unavailable(value) => json!({ "known": null, "unavailable": reason(value) }),
    }
}

fn known(value: Amount) -> Option<u64> {
    match value {
        Amount::Known(value) => Some(value),
        Amount::Unavailable(_) => None,
    }
}

fn reason(value: apps::Unavailable) -> &'static str {
    match value {
        apps::Unavailable::Refused => "refused",
        apps::Unavailable::Gone => "gone",
        apps::Unavailable::Unknown => "unknown",
        apps::Unavailable::Interval => "interval",
        apps::Unavailable::Creation => "creation",
        apps::Unavailable::Baseline => "baseline",
        apps::Unavailable::CounterReset => "counter_reset",
        apps::Unavailable::Overflow => "overflow",
        apps::Unavailable::Exec => "exec",
        apps::Unavailable::Program => "program",
        apps::Unavailable::ExitUnverified => "exit_unverified",
    }
}

fn identity_reason(value: collector::ring::IdentityUnavailable) -> &'static str {
    use collector::ring::IdentityUnavailable;
    match value {
        IdentityUnavailable::Refused => "refused",
        IdentityUnavailable::Gone => "gone",
        IdentityUnavailable::Unsupported => "unsupported",
        IdentityUnavailable::Changed => "changed",
        IdentityUnavailable::Invalid => "invalid",
        IdentityUnavailable::Exhausted => "exhausted",
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 15)]));
    }

    output
}

fn page_range(page: Page, count: usize) -> Result<std::ops::Range<usize>> {
    if page.limit == 0 {
        return Err(Error::Bound("live App page limit"));
    }
    if page.limit > super::PAGE_ROWS_MAX {
        return Err(Error::Bound("live App page limit"));
    }
    let offset = usize::try_from(page.offset).map_err(|_| Error::Bound("live App page offset"))?;
    if offset > count {
        return Err(Error::Bound("live App page offset"));
    }
    let limit = usize::try_from(page.limit).map_err(|_| Error::Bound("live App page limit"))?;

    Ok(offset..offset.saturating_add(limit).min(count))
}

fn page_json(page: Page, end: usize, count: usize) -> Value {
    json!({ "offset": page.offset, "limit": page.limit, "total": count,
        "next_offset": if end < count { Some(end) } else { None } })
}

pub(crate) fn bounded(value: Value) -> Result<Value> {
    let mut output = Envelope { bytes: 0 };
    serde_json::to_writer(&mut output, &value)
        .map_err(|_| Error::Bound("live App response bytes"))?;
    assert!(output.bytes <= super::RESPONSE_BYTES_MAX);

    Ok(value)
}

struct Envelope {
    bytes: usize,
}

impl Write for Envelope {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > super::RESPONSE_BYTES_MAX - self.bytes {
            return Err(io::Error::other("live App response bytes"));
        }
        self.bytes += bytes.len();

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
