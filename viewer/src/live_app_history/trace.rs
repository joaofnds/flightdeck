use super::{App, CompactMeasure, Selection, apps, count, key};
use crate::app_process_groups_history::ProcessGroupReading;
use crate::live_app_response::{self as response, AppPart, Measure, Observation};
use crate::live_apps::Frame;
use persistence::{Error, Result};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub(crate) struct TracePoint {
    pub observation: Observation,
    pub elapsed_ns: Option<u64>,
    totals: Option<Totals>,
    captured: bool,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Totals {
    pub(super) members: u16,
    membership_complete: bool,
    cpu_ns: CompactMeasure,
    energy_nj: CompactMeasure,
    gpu_ns: CompactMeasure,
    disk_read_bytes: CompactMeasure,
    disk_written_bytes: CompactMeasure,
    disk_io_bytes: CompactMeasure,
    resident_bytes: CompactMeasure,
    footprint_bytes: CompactMeasure,
    idle_wakeups: CompactMeasure,
    pageins: CompactMeasure,
    threads: CompactMeasure,
}

const _: () = {
    assert!(size_of::<TracePoint>() <= 312);
};

impl TracePoint {
    pub(super) fn source_available(&self) -> bool {
        self.captured
    }

    pub(crate) fn capture(
        selected: &Frame,
        previous: Option<&Frame>,
        target: &dyn Selection,
    ) -> Result<Self> {
        let mut output = None;

        selected.with_aggregate(previous, |aggregate| {
            let mut totals = None;
            capture_totals(aggregate, target, &mut totals);

            output = Some(Self {
                observation: Observation {
                    recording: selected.identity().recording.0,
                    stamp: *selected.stamp(),
                    run_tick: selected.run_tick(),
                    wall_ns: selected.wall_ns(),
                },
                elapsed_ns: aggregate.elapsed_ns,
                totals,
                captured: true,
            });

            Ok(())
        })?;

        output.ok_or(Error::Bound("App history aggregate output"))
    }

    /// The point of `target` that a window keeps in the part of its App at one second.
    pub(crate) fn from_part(part: &AppPart, target: &dyn Selection) -> Self {
        let header = part.header();
        let mut totals = None;
        if let Some(present) = part.present() {
            if let Some(executable) = target.executable() {
                let mut group = None;
                super::process::part_totals(
                    &present.processes,
                    executable,
                    header.membership_complete,
                    &mut group,
                );
                if let Some(group) = &group {
                    Totals::capture(group, &mut totals);
                }
            } else {
                totals = Some(Totals::of_app(&present.app));
            }
        }

        Self {
            observation: header.selected,
            elapsed_ns: header.elapsed_ns,
            totals,
            captured: true,
        }
    }

    pub(super) fn from_group(
        observation: &Observation,
        elapsed_ns: Option<u64>,
        totals: Option<&Totals>,
        output: &mut Self,
    ) {
        *output = Self {
            observation: *observation,
            elapsed_ns,
            totals: totals.copied(),
            captured: true,
        };
    }

    pub(super) fn from_live(
        observation: &Observation,
        elapsed_ns: Option<u64>,
        app: Option<&App>,
    ) -> Self {
        Self {
            observation: *observation,
            elapsed_ns,
            captured: true,
            totals: app.map(|app| Totals {
                members: app.members,
                membership_complete: app.membership_complete,
                cpu_ns: app.cpu_ns,
                energy_nj: app.energy_nj,
                gpu_ns: app.gpu_ns,
                disk_read_bytes: app.disk_read_bytes,
                disk_written_bytes: app.disk_written_bytes,
                disk_io_bytes: app.disk_io_bytes,
                resident_bytes: app.resident_bytes,
                footprint_bytes: app.footprint_bytes,
                ..Totals::default()
            }),
        }
    }

    pub(super) fn unavailable(observation: &Observation) -> Self {
        Self {
            observation: *observation,
            elapsed_ns: None,
            totals: None,
            captured: false,
        }
    }

    pub(crate) fn json(&self, segment: u64, named_process_group: bool) -> Value {
        let mut point = json!({"observation": self.observation.json(),
            "segment": segment.to_string(), "source_available": self.captured,
            "elapsed_ns": self.elapsed_ns.map(|value| value.to_string()),
            "members": null, "membership_complete": false,
            "cpu_ns": null, "energy_nj": null, "resident_bytes": null, "footprint_bytes": null,
            "cpu_percent_one_core": null, "energy_joules": null, "energy_watts": null,
            "gpu_ns": null, "gpu_percent": null,
            "disk_read_bytes": null, "disk_written_bytes": null, "disk_io_bytes": null,
            "disk_read_bps": null, "disk_written_bps": null, "disk_io_bps": null});

        if let Some(app) = &self.totals {
            point["members"] = json!(app.members);
            point["membership_complete"] = json!(app.membership_complete);
            point["cpu_ns"] = app.cpu_ns.json();
            point["energy_nj"] = app.energy_nj.json();
            point["resident_bytes"] = app.resident_bytes.json();
            point["footprint_bytes"] = app.footprint_bytes.json();
            point["cpu_percent_one_core"] = json!(app.cpu_ns.measured().and_then(|cpu_ns| {
                apps::cpu_percent_one_core(apps::CpuRate {
                    cpu_ns,
                    elapsed_ns: self.elapsed_ns?,
                })
            }));
            point["energy_joules"] = json!(app.energy_nj.measured().map(apps::energy_joules));
            point["energy_watts"] = json!(app.energy_nj.measured().and_then(|energy_nj| {
                apps::energy_watts(apps::EnergyRate {
                    energy_nj,
                    elapsed_ns: self.elapsed_ns?,
                })
            }));
            point["gpu_ns"] = app.gpu_ns.json();
            point["gpu_percent"] = json!(app.gpu_ns.measured().and_then(|gpu_ns| {
                apps::gpu_percent(apps::GpuRate {
                    gpu_ns,
                    elapsed_ns: self.elapsed_ns?,
                })
            }));
            self.json_disk(app, &mut point);
        }

        if named_process_group {
            self.json_process(&mut point);
        }

        point
    }

    fn json_process(&self, point: &mut Value) {
        point["idle_wakeups"] = Value::Null;
        point["pageins"] = Value::Null;
        point["threads"] = Value::Null;
        point["idle_wakeups_per_second"] = Value::Null;
        point["pageins_per_second"] = Value::Null;
        if let Some(app) = &self.totals {
            point["idle_wakeups"] = app.idle_wakeups.json();
            point["pageins"] = app.pageins.json();
            point["threads"] = app.threads.json();
            point["idle_wakeups_per_second"] = json!(self.json_event_rate(app.idle_wakeups));
            point["pageins_per_second"] = json!(self.json_event_rate(app.pageins));
        }
    }

    fn json_event_rate(&self, amount: CompactMeasure) -> Option<f64> {
        apps::events_per_second(apps::EventRate {
            events: amount.measured()?,
            elapsed_ns: self.elapsed_ns?,
        })
    }

    fn json_disk(&self, app: &Totals, point: &mut Value) {
        point["disk_read_bytes"] = app.disk_read_bytes.json();
        point["disk_written_bytes"] = app.disk_written_bytes.json();
        point["disk_io_bytes"] = app.disk_io_bytes.json();
        point["disk_read_bps"] = json!(self.json_disk_rate(app.disk_read_bytes));
        point["disk_written_bps"] = json!(self.json_disk_rate(app.disk_written_bytes));
        point["disk_io_bps"] = json!(self.json_disk_rate(app.disk_io_bytes));
    }

    fn json_disk_rate(&self, amount: CompactMeasure) -> Option<f64> {
        apps::bytes_per_second(apps::BytesRate {
            bytes: amount.measured()?,
            elapsed_ns: self.elapsed_ns?,
        })
    }
}

fn capture_totals(
    aggregate: &apps::Aggregate<'_>,
    target: &dyn Selection,
    output: &mut Option<Totals>,
) {
    let mut group = None;
    let app = if let Some(executable) = target.executable() {
        super::process::aggregate_totals(aggregate, target.app(), executable, &mut group);
        group.as_ref()
    } else {
        aggregate
            .apps
            .get(&key(target.app().kind, target.app().bytes))
    };

    if let Some(app) = app {
        Totals::capture(app, output);
    }
}

impl Totals {
    pub(super) fn process_group_reading(&self, output: &mut ProcessGroupReading) {
        *output = ProcessGroupReading {
            present: true,
            members: u32::from(self.members),
            measures: [
                self.cpu_ns.measure(),
                self.energy_nj.measure(),
                self.disk_read_bytes.measure(),
                self.disk_written_bytes.measure(),
                self.disk_io_bytes.measure(),
                self.resident_bytes.measure(),
                self.footprint_bytes.measure(),
                self.idle_wakeups.measure(),
                self.pageins.measure(),
                self.threads.measure(),
            ],
        };
    }

    /// The totals of an App's row, without the counts only a process group's history shows,
    /// as a live point gives them.
    fn of_app(app: &response::App) -> Self {
        Self {
            members: count(app.members),
            membership_complete: app.membership_complete,
            cpu_ns: CompactMeasure::capture(&app.cpu_ns),
            energy_nj: CompactMeasure::capture(&app.energy_nj),
            gpu_ns: CompactMeasure::capture(&app.gpu_ns),
            disk_read_bytes: CompactMeasure::capture(&app.disk_read_bytes),
            disk_written_bytes: CompactMeasure::capture(&app.disk_written_bytes),
            disk_io_bytes: CompactMeasure::capture(&app.disk_io_bytes),
            resident_bytes: CompactMeasure::capture(&app.resident_bytes),
            footprint_bytes: CompactMeasure::capture(&app.footprint_bytes),
            ..Self::default()
        }
    }

    pub(super) fn capture(app: &apps::AppTotals, output: &mut Option<Self>) {
        *output = Some(Self {
            members: count(app.members),
            membership_complete: app.membership_complete,
            cpu_ns: capture_subtotal(&app.cpu_ns),
            energy_nj: capture_subtotal(&app.energy_nj),
            gpu_ns: capture_subtotal(&app.gpu_ns),
            disk_read_bytes: capture_subtotal(&app.disk_read_bytes),
            disk_written_bytes: capture_subtotal(&app.disk_written_bytes),
            disk_io_bytes: capture_subtotal(&app.disk_io_bytes),
            resident_bytes: capture_subtotal(&app.resident_bytes),
            footprint_bytes: capture_subtotal(&app.footprint_bytes),
            idle_wakeups: capture_subtotal(&app.idle_wakeups),
            pageins: capture_subtotal(&app.pageins),
            threads: capture_subtotal(&app.threads),
        });
    }
}

pub(super) fn capture_subtotal(source: &apps::Subtotal) -> CompactMeasure {
    CompactMeasure::capture(&Measure {
        known: source.known,
        measured_members: source.measured_members,
        unavailable_members: source.unavailable_members,
        overflowed: source.overflowed,
    })
}
