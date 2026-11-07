use std::fmt::Write;

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::{Target, apps, key};
use crate::live_app_response::Process;
use crate::live_app_response::json::ProcessView;

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

struct Member {
    attribution: Option<apps::Unavailable>,
    live: bool,
    amounts: [apps::Amount; 11],
}

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

/// The totals of the processes of a part's App that run the executable named `executable`,
/// or none where none does, as `aggregate_totals` gives them from the part's sample.
pub(super) fn part_totals(
    processes: &[Process],
    executable: &[u8],
    membership_complete: bool,
    output: &mut Option<apps::AppTotals>,
) {
    let totals = output.insert(apps::AppTotals::default());
    for process in processes {
        if let Some(name) = &process.executable_name {
            if name.bytes == executable {
                add_process(totals, process);
            }
        }
    }

    totals.membership_complete = membership_complete;
    if totals.members == 0 {
        *output = None;
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
                    process.gpu_ns,
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

/// Adds a process of a sample to the totals of its group.
fn add_process(totals: &mut apps::AppTotals, process: &Process) {
    add_process_view(totals, &ProcessView::of(process));
}

pub(crate) fn add_process_view(totals: &mut apps::AppTotals, process: &ProcessView<'_>) {
    add(
        totals,
        &Member {
            attribution: process.app_attribution,
            live: process.state == apps::ProcessState::Live,
            amounts: [
                process.cpu_ns,
                process.energy_nj,
                process.gpu_ns,
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

fn add(totals: &mut apps::AppTotals, member: &Member) {
    totals.members += 1;
    assert!(totals.members <= 24_000);
    let [
        cpu,
        energy,
        gpu,
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
    totals.gpu_ns.add(attributed(gpu));
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
