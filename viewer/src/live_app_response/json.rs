use std::io::{self, Write};

use serde_json::{Value, json};

use super::{Amount, App, Detail, Error, Measure, Page, Process, Result, Sample, Stamp};
use crate::apps;

/// An App's presentation facts, borrowing its identity from the retained owner.
pub(crate) struct AppView<'row> {
    pub kind: super::KeyKind,
    pub bytes: &'row [u8],
    pub display_name: &'row str,
    pub members: u32,
    pub live_members: u32,
    pub membership_complete: bool,
    pub cpu_ns: Measure,
    pub energy_nj: Measure,
    pub gpu_ns: Measure,
    pub disk_read_bytes: Measure,
    pub disk_written_bytes: Measure,
    pub disk_io_bytes: Measure,
    pub resident_bytes: Measure,
    pub footprint_bytes: Measure,
}

/// A process's presentation facts, borrowing only its captured executable name.
pub(crate) struct ProcessView<'row> {
    pub pid: i32,
    pub creation: Option<apps::ProcessCreation>,
    pub app_index: Option<u32>,
    pub executable_name: Option<(&'row [u8], &'row str)>,
    pub program_unavailable: Option<collector::ring::IdentityUnavailable>,
    pub cpu_ns: Amount,
    pub energy_nj: Amount,
    pub gpu_ns: Amount,
    pub disk_read_bytes: Amount,
    pub disk_written_bytes: Amount,
    pub disk_io_bytes: Amount,
    pub resident_bytes: Amount,
    pub footprint_bytes: Amount,
    pub idle_wakeups: Amount,
    pub pageins: Amount,
    pub threads: Amount,
    pub app_attribution: Option<apps::Unavailable>,
    pub state: apps::ProcessState,
}

impl AppView<'_> {
    pub(crate) fn of(value: &App) -> AppView<'_> {
        AppView {
            kind: value.key.kind,
            bytes: &value.key.bytes,
            display_name: &value.display_name,
            members: value.members,
            live_members: value.live_members,
            membership_complete: value.membership_complete,
            cpu_ns: value.cpu_ns,
            energy_nj: value.energy_nj,
            gpu_ns: value.gpu_ns,
            disk_read_bytes: value.disk_read_bytes,
            disk_written_bytes: value.disk_written_bytes,
            disk_io_bytes: value.disk_io_bytes,
            resident_bytes: value.resident_bytes,
            footprint_bytes: value.footprint_bytes,
        }
    }
}

impl ProcessView<'_> {
    pub(crate) fn of(value: &Process) -> ProcessView<'_> {
        ProcessView {
            pid: value.pid,
            creation: value.creation,
            app_index: value.app_index,
            executable_name: value
                .executable_name
                .as_ref()
                .map(|name| (name.bytes.as_slice(), name.display.as_str())),
            program_unavailable: value.program_unavailable,
            cpu_ns: value.cpu_ns,
            energy_nj: value.energy_nj,
            gpu_ns: value.gpu_ns,
            disk_read_bytes: value.disk_read_bytes,
            disk_written_bytes: value.disk_written_bytes,
            disk_io_bytes: value.disk_io_bytes,
            resident_bytes: value.resident_bytes,
            footprint_bytes: value.footprint_bytes,
            idle_wakeups: value.idle_wakeups,
            pageins: value.pageins,
            threads: value.threads,
            app_attribution: value.app_attribution,
            state: value.state,
        }
    }
}

/// The facts of a sample that every page of it carries.
#[derive(Clone, Copy)]
pub(crate) struct Header {
    pub(crate) selected: super::Observation,
    pub(crate) previous: Option<Stamp>,
    pub(crate) elapsed_ns: Option<u64>,
    pub(crate) membership_complete: bool,
    pub(crate) ungrouped_members: u32,
    pub(crate) live_ungrouped_members: u32,
    pub(crate) usage_refused: u32,
    pub(crate) usage_gone: u32,
    pub(crate) app_count: usize,
    pub(crate) process_count: usize,
}

impl Header {
    pub(super) fn of(sample: &Sample) -> Self {
        Self {
            selected: sample.selected,
            previous: sample.previous,
            elapsed_ns: sample.elapsed_ns,
            membership_complete: sample.membership_complete,
            ungrouped_members: sample.ungrouped_members,
            live_ungrouped_members: sample.live_ungrouped_members,
            usage_refused: sample.usage_refused,
            usage_gone: sample.usage_gone,
            app_count: sample.apps.len(),
            process_count: sample.processes.len(),
        }
    }
}

/// One App and its processes, from a sample or from the App's part of it.
pub(super) struct AppMembers<'a> {
    pub header: &'a Header,
    pub app: &'a App,
    pub app_index: usize,
    pub processes: &'a [&'a Process],
}

pub(super) fn apps_page(sample: &Sample, page: Page) -> Result<Value> {
    let mut result = apps_paging(&Header::of(sample), page)?;
    let range = page_range(page, sample.apps.len())?;
    let values: Vec<_> = sample.apps[range.clone()]
        .iter()
        .enumerate()
        .map(|(index, value)| app(&AppView::of(value), range.start + index, sample.elapsed_ns))
        .collect();
    result["apps"] = Value::Array(values);

    bounded(result)
}

/// An Apps page without the Apps it lists, the same whether its sample or a part gives it.
pub(crate) fn apps_paging(header: &Header, page: Page) -> Result<Value> {
    let range = page_range(page, header.app_count)?;
    let mut result = header_json(header);
    result["page"] = page_json(page, range.end, header.app_count);

    bounded(result)
}

pub(super) fn app_detail(sample: &Sample, detail: &Detail<'_>) -> Result<Value> {
    let index = usize::try_from(detail.app_index).map_err(|_| Error::Bound("live App index"))?;
    let value = sample
        .apps
        .get(index)
        .ok_or(Error::Bound("live App index"))?;
    let processes = members(sample, Some(detail.app_index));

    app_members_page(
        &AppMembers {
            header: &Header::of(sample),
            app: value,
            app_index: index,
            processes: &processes,
        },
        detail.page,
    )
}

/// The detail page of one App, the same whether its sample or its part gives it.
pub(super) fn app_members_page(members: &AppMembers<'_>, page: Page) -> Result<Value> {
    let mut result = processes_page(
        members.header,
        members.processes.iter().map(|value| ProcessView::of(value)),
        members.processes.len(),
        page,
    )?;
    result["app"] = app(
        &AppView::of(members.app),
        members.app_index,
        members.header.elapsed_ns,
    );

    bounded(result)
}

pub(super) fn members_page(sample: &Sample, app_index: Option<u32>, page: Page) -> Result<Value> {
    let processes = members(sample, app_index);

    processes_page(
        &Header::of(sample),
        processes.iter().map(|value| ProcessView::of(value)),
        processes.len(),
        page,
    )
}

fn members(sample: &Sample, app_index: Option<u32>) -> Vec<&Process> {
    sample
        .processes
        .iter()
        .filter(|value| value.app_index == app_index)
        .collect()
}

pub(crate) fn processes_page<'row>(
    header: &Header,
    processes: impl Iterator<Item = ProcessView<'row>>,
    count: usize,
    page: Page,
) -> Result<Value> {
    let range = page_range(page, count)?;
    let values: Vec<_> = processes
        .skip(range.start)
        .take(range.len())
        .map(|value| process(&value, header.elapsed_ns))
        .collect();
    let mut result = header_json(header);
    result["processes"] = Value::Array(values);
    result["page"] = page_json(page, range.end, count);

    bounded(result)
}

fn header_json(header: &Header) -> Value {
    json!({
        "selected": observation(&header.selected),
        "previous": header.previous.as_ref().map(stamp),
        "elapsed_ns": header.elapsed_ns.map(|value| value.to_string()),
        "membership_complete": header.membership_complete,
        "ungrouped_members": header.ungrouped_members,
        "usage_refused": header.usage_refused,
        "usage_gone": header.usage_gone,
        "app_count": header.app_count,
        "process_count": header.process_count,
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

pub(crate) fn app(value: &AppView<'_>, index: usize, elapsed_ns: Option<u64>) -> Value {
    let mut key_display = String::with_capacity(value.bytes.len() * 4);
    crate::byte_display::append(&mut key_display, value.bytes);

    json!({
        "app_index": index, "display_name": value.display_name,
        "key": {
            "kind": match value.kind {
                super::KeyKind::Bundle => "bundle", super::KeyKind::Executable => "executable",
            },
            "bytes_hex": hex(value.bytes), "display": key_display,
        },
        "members": value.members, "live_members": value.live_members,
        "membership_complete": value.membership_complete,
        "cpu_ns": measure(&value.cpu_ns), "energy_nj": measure(&value.energy_nj),
        "gpu_ns": measure(&value.gpu_ns),
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
        "gpu_percent": value.gpu_ns.measured().and_then(|gpu_ns| {
            apps::gpu_percent(apps::GpuRate { gpu_ns, elapsed_ns: elapsed_ns? })
        }),
    })
}

fn process(value: &ProcessView<'_>, elapsed_ns: Option<u64>) -> Value {
    json!({
        "pid": value.pid,
        "creation": value.creation.map(|creation| json!({
            "unique_id": creation.unique_id.to_string(), "pid_version": creation.pid_version,
        })),
        "app_index": value.app_index,
        "executable_name": value.executable_name.map(|(bytes, display)| json!({
            "bytes_hex": hex(bytes), "display": display,
        })),
        "program_unavailable": value.program_unavailable.map(identity_reason),
        "cpu_ns": amount(value.cpu_ns), "energy_nj": amount(value.energy_nj),
        "gpu_ns": amount(value.gpu_ns),
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
        "gpu_percent": known(value.gpu_ns).and_then(|gpu_ns| {
            apps::gpu_percent(apps::GpuRate { gpu_ns, elapsed_ns: elapsed_ns? })
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

pub(crate) fn page_range(page: Page, count: usize) -> Result<std::ops::Range<usize>> {
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
    bounded_bytes(&value, super::RESPONSE_BYTES_MAX)?;

    Ok(value)
}

pub(crate) fn bounded_bytes(value: &Value, maximum: usize) -> Result<()> {
    let mut output = Envelope { bytes: 0, maximum };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| Error::Bound("live App response bytes"))?;
    assert!(output.bytes <= maximum);

    Ok(())
}

struct Envelope {
    bytes: usize,
    maximum: usize,
}

impl Write for Envelope {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum - self.bytes {
            return Err(io::Error::other("live App response bytes"));
        }
        self.bytes += bytes.len();

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
