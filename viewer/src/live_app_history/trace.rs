use super::{App, CompactMeasure, Target, apps, count, key};
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
struct Totals {
    members: u16,
    membership_complete: bool,
    cpu_ns: CompactMeasure,
    energy_nj: CompactMeasure,
    resident_bytes: CompactMeasure,
    footprint_bytes: CompactMeasure,
}

const _: () = {
    assert!(size_of::<TracePoint>() <= 224);
};

impl TracePoint {
    pub(crate) fn capture(
        selected: &Frame,
        previous: Option<&Frame>,
        target: &Target<'_>,
    ) -> Result<Self> {
        let mut output = None;

        selected.with_aggregate(previous, |aggregate| {
            let totals = aggregate
                .apps
                .get(&key(target.kind, target.bytes))
                .map(|app| Totals {
                    members: count(app.members),
                    membership_complete: app.membership_complete,
                    cpu_ns: capture_subtotal(&app.cpu_ns),
                    energy_nj: capture_subtotal(&app.energy_nj),
                    resident_bytes: capture_subtotal(&app.resident_bytes),
                    footprint_bytes: capture_subtotal(&app.footprint_bytes),
                });

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
            "cpu_percent_one_core": null, "energy_joules": null, "energy_watts": null});

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
        }

        point
    }
}

fn capture_subtotal(source: &apps::Subtotal) -> CompactMeasure {
    CompactMeasure::capture(&Measure {
        known: source.known,
        measured_members: source.measured_members,
        unavailable_members: source.unavailable_members,
        overflowed: source.overflowed,
    })
}
