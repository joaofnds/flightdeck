use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::trace::Totals;
use super::{App, BYTES_MAX, History, Point, Target, TracePoint, apps, key};
use crate::live_app_response::Sample;

pub(crate) mod cohort;
mod groups_cache;

pub(crate) trait Selection {
    fn app(&self) -> &Target<'_>;
    fn executable(&self) -> Option<&[u8]>;
}

pub(crate) struct GroupTarget<'a> {
    pub app: Target<'a>,
    pub executable: &'a [u8],
}

pub(super) struct Group {
    app_index: u16,
    executable: Arc<Vec<u8>>,
    totals: Option<Totals>,
}

struct Member {
    attribution: Option<apps::Unavailable>,
    live: bool,
    amounts: [apps::Amount; 10],
}

const _: () = {
    assert!(size_of::<Group>() <= 144);
};

impl Selection for Target<'_> {
    fn app(&self) -> &Target<'_> {
        self
    }

    fn executable(&self) -> Option<&[u8]> {
        None
    }
}

impl Selection for GroupTarget<'_> {
    fn app(&self) -> &Target<'_> {
        &self.app
    }

    fn executable(&self) -> Option<&[u8]> {
        Some(self.executable)
    }
}

impl GroupTarget<'_> {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.app.bytes.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
            return Err(Error::Bound("process history App key"));
        }

        if self.executable.is_empty() {
            return Err(Error::Bound("process history executable key"));
        }

        if self.executable.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
            return Err(Error::Bound("process history executable key"));
        }

        Ok(())
    }

    pub(crate) fn identify(&self, response: &mut Value) -> Result<()> {
        let mut app = identity(self.app.bytes)?;
        app["kind"] = json!(match self.app.kind {
            crate::live_app_response::KeyKind::Bundle => "bundle",
            crate::live_app_response::KeyKind::Executable => "executable",
        });

        response["app_key"] = app;
        response["executable_name"] = identity(self.executable)?;
        response["scope"] = json!("selected_named_process_group");

        Ok(())
    }
}

fn identity(bytes: &[u8]) -> Result<Value> {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut hex, "{byte:02x}").map_err(std::io::Error::other)?;
    }

    let mut display = String::with_capacity(bytes.len() * 4);
    crate::byte_display::append(&mut display, bytes);

    Ok(json!({"bytes_hex":hex,"display":display}))
}

pub(super) fn capture(sample: &Sample, previous: Option<&Point>, apps: &[App]) -> Vec<Group> {
    let mut grouped = BTreeMap::<(u32, &[u8]), apps::AppTotals>::new();
    for process in sample.processes() {
        let (Some(app), Some(name)) = (process.app_index, &process.executable_name) else {
            continue;
        };

        let totals = grouped.entry((app, &name.bytes)).or_default();
        add(
            totals,
            &Member {
                attribution: process.app_attribution,
                live: process.state == apps::ProcessState::Live,
                amounts: [
                    process.cpu_ns,
                    process.energy_nj,
                    process.disk_read_bytes,
                    process.disk_written_bytes,
                    process.disk_io_bytes,
                    process.resident_bytes,
                    process.footprint_bytes,
                    process.idle_wakeups,
                    process.pageins,
                    process.threads,
                ],
            },
        );
    }

    let mut output = Vec::with_capacity(grouped.len());
    for ((app_index, executable), mut totals) in grouped {
        let app_index = u16::try_from(app_index).unwrap_or_else(|_| unreachable!());
        totals.membership_complete = sample.membership_complete;
        let executable = capture_key(previous, apps, app_index, executable);

        output.push(Group {
            app_index,
            executable,
            totals: None,
        });
        let group = output.last_mut().unwrap_or_else(|| unreachable!());
        Totals::capture(&totals, &mut group.totals);
    }

    assert!(output.len() <= apps::PROCESS_INTERVALS_MAX);

    output
}

fn capture_key(
    previous: Option<&Point>,
    apps: &[App],
    app_index: u16,
    executable: &[u8],
) -> Arc<Vec<u8>> {
    if let Some(previous) = previous {
        let Some(groups) = &previous.groups else {
            return Arc::new(executable.to_vec());
        };

        let app = apps[usize::from(app_index)].key.view();
        let index = groups.binary_search_by(|group| {
            previous.apps[usize::from(group.app_index)]
                .key
                .view()
                .cmp(&app)
                .then(group.executable.as_slice().cmp(executable))
        });
        if let Ok(index) = index {
            return Arc::clone(&groups[index].executable);
        }
    }

    Arc::new(executable.to_vec())
}

pub(super) fn key_bytes(groups: &[Group]) -> usize {
    groups
        .iter()
        .filter(|group| Arc::strong_count(&group.executable) == 1)
        .map(|group| 2 * size_of::<usize>() + size_of::<Vec<u8>>() + group.executable.capacity())
        .sum()
}

pub(super) fn trace(
    point: &Point,
    target: &Target<'_>,
    executable: &[u8],
    output: &mut TracePoint,
) {
    let Some(groups) = &point.groups else {
        return;
    };

    let app = key(target.kind, target.bytes);
    let index = groups.binary_search_by(|group| {
        point.apps[usize::from(group.app_index)]
            .key
            .view()
            .cmp(&app)
            .then(group.executable.as_slice().cmp(executable))
    });
    let totals = match index {
        Ok(index) => groups[index].totals.as_ref(),
        Err(_) => None,
    };

    TracePoint::from_group(&point.observation, point.elapsed_ns, totals, output);
}

pub(super) fn payload_bytes(groups: Option<&Vec<Group>>) -> usize {
    groups.map_or(0, |groups| groups.capacity() * size_of::<Group>())
}

pub(super) fn charged_bytes(groups: Option<&Vec<Group>>) -> usize {
    payload_bytes(groups) + groups.map_or(0, |groups| key_bytes(groups))
}

impl History {
    pub(crate) fn process_coverage(&self, response: &mut Value) {
        response["coverage"]["live_group_cache_bytes"] = json!(self.group_bytes);
        response["coverage"]["live_group_observations_retained"] = json!(
            self.points
                .iter()
                .filter(|point| point.groups.is_some())
                .count()
        );
    }

    pub(super) fn trim_process_groups(&mut self, incoming: &mut Point) {
        // Optional groups get half the room left by App facts; saved metric caches keep room too.
        let limit = BYTES_MAX.saturating_sub(self.bytes - self.group_bytes) / 2;
        if self.process_groups_fit(limit) {
            return;
        }

        for point in &mut self.points {
            if self.group_bytes <= limit {
                if self.bytes + self.saved_residents.get_mut().bytes() <= BYTES_MAX {
                    return;
                }
            }

            let released = charged_bytes(point.groups.as_ref());
            point.groups = None;
            self.bytes -= released;
            self.group_bytes -= released;
        }

        if self.process_groups_fit(limit) {
            return;
        }

        let released = charged_bytes(incoming.groups.as_ref());
        incoming.groups = None;
        self.bytes -= released;
        self.group_bytes -= released;
    }

    fn process_groups_fit(&mut self, limit: usize) -> bool {
        if self.group_bytes > limit {
            return false;
        }

        self.bytes + self.saved_residents.get_mut().bytes() <= BYTES_MAX
    }
}

pub(super) fn aggregate_totals(
    aggregate: &apps::Aggregate<'_>,
    target: &Target<'_>,
    executable: &[u8],
    output: &mut Option<apps::AppTotals>,
) {
    *output = Some(apps::AppTotals::default());
    let totals = output.as_mut().unwrap_or_else(|| unreachable!());
    let app = key(target.kind, target.bytes);

    for process in &aggregate.processes {
        if process.app != Some(app) {
            continue;
        }

        if process.executable_name != Some(executable) {
            continue;
        }

        add(
            totals,
            &Member {
                attribution: process.app_attribution,
                live: process.state == apps::ProcessState::Live,
                amounts: [
                    process.cpu_ns,
                    process.energy_nj,
                    process.disk_read_bytes,
                    process.disk_written_bytes,
                    process.disk_io_bytes,
                    process.resident_bytes_last.map_or(
                        apps::Amount::Unavailable(apps::Unavailable::Unknown),
                        apps::Amount::Known,
                    ),
                    process.footprint_bytes_last.map_or(
                        apps::Amount::Unavailable(apps::Unavailable::Unknown),
                        apps::Amount::Known,
                    ),
                    process.idle_wakeups,
                    process.pageins,
                    process.threads.map_or(
                        apps::Amount::Unavailable(apps::Unavailable::Unknown),
                        apps::Amount::Known,
                    ),
                ],
            },
        );
    }

    totals.membership_complete = aggregate.membership_complete;
    if totals.members == 0 {
        *output = None;
    }
}

fn add(totals: &mut apps::AppTotals, member: &Member) {
    totals.members += 1;
    assert!(totals.members <= 24_000);
    let [
        cpu,
        energy,
        read,
        written,
        disk,
        resident,
        footprint,
        wakeups,
        pageins,
        threads,
    ] = member.amounts;
    let attributed = |amount| member.attribution.map_or(amount, apps::Amount::Unavailable);

    totals.cpu_ns.add(attributed(cpu));
    totals.energy_nj.add(attributed(energy));
    totals.disk_read_bytes.add(attributed(read));
    totals.disk_written_bytes.add(attributed(written));
    totals.disk_io_bytes.add(attributed(disk));
    totals.idle_wakeups.add(attributed(wakeups));
    totals.pageins.add(attributed(pageins));
    if member.live {
        totals.live_members += 1;
        totals.resident_bytes.add(resident);
        totals.footprint_bytes.add(footprint);
        totals.threads.add(threads);
    }
}
