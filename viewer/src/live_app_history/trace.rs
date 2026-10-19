use super::{App, CompactMeasure, Selection, apps, count, key};
use crate::live_app_response::{Measure, Observation};
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

#[derive(Clone, Copy)]
pub(super) struct Totals {
    members: u16,
    membership_complete: bool,
    cpu_ns: CompactMeasure,
    energy_nj: CompactMeasure,
    disk_read_bytes: CompactMeasure,
    disk_written_bytes: CompactMeasure,
    disk_io_bytes: CompactMeasure,
    resident_bytes: CompactMeasure,
    footprint_bytes: CompactMeasure,
}

const _: () = {
    assert!(size_of::<TracePoint>() <= 272);
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
                disk_read_bytes: app.disk_read_bytes,
                disk_written_bytes: app.disk_written_bytes,
                disk_io_bytes: app.disk_io_bytes,
                resident_bytes: app.resident_bytes,
                footprint_bytes: app.footprint_bytes,
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

    pub(crate) fn json(&self, segment: u64) -> Value {
        let mut point = json!({"observation": self.observation.json(),
            "segment": segment.to_string(), "source_available": self.captured,
            "elapsed_ns": self.elapsed_ns.map(|value| value.to_string()),
            "members": null, "membership_complete": false,
            "cpu_ns": null, "energy_nj": null, "resident_bytes": null, "footprint_bytes": null,
            "cpu_percent_one_core": null, "energy_joules": null, "energy_watts": null,
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
            self.json_disk(app, &mut point);
        }

        point
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
    pub(super) fn capture(app: &apps::AppTotals, output: &mut Option<Self>) {
        *output = Some(Self {
            members: count(app.members),
            membership_complete: app.membership_complete,
            cpu_ns: capture_subtotal(&app.cpu_ns),
            energy_nj: capture_subtotal(&app.energy_nj),
            disk_read_bytes: capture_subtotal(&app.disk_read_bytes),
            disk_written_bytes: capture_subtotal(&app.disk_written_bytes),
            disk_io_bytes: capture_subtotal(&app.disk_io_bytes),
            resident_bytes: capture_subtotal(&app.resident_bytes),
            footprint_bytes: capture_subtotal(&app.footprint_bytes),
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
