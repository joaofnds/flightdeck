use persistence::Result;

use super::{AppReading, Capture, Reduction, Source};
use crate::live_app_response::{Measure, Observation};
use crate::live_apps::Frame;

impl Reduction<'_> {
    pub(crate) fn observe_frame(&mut self, frame: &Frame) -> Result<()> {
        frame.with_aggregate(None, |aggregate| {
            self.observe(&capture(frame, aggregate), apps(aggregate), Source::Saved)
        })
    }

    pub(crate) fn project_frame(&mut self, frame: &Frame) -> Result<()> {
        frame.with_aggregate(None, |aggregate| {
            self.project(&capture(frame, aggregate), apps(aggregate), Source::Saved)
        })
    }
}

pub(crate) fn capture(frame: &Frame, aggregate: &crate::apps::Aggregate<'_>) -> Capture {
    Capture {
        observation: Observation {
            recording: frame.identity().recording.0,
            stamp: *frame.stamp(),
            run_tick: frame.run_tick(),
            wall_ns: frame.wall_ns(),
        },
        membership_complete: aggregate.membership_complete,
        ungrouped_members: u32::try_from(
            aggregate
                .processes
                .iter()
                .filter(|process| {
                    process.state == crate::apps::ProcessState::Live && process.app.is_none()
                })
                .count(),
        )
        .unwrap_or_else(|_| unreachable!("bounded resident process count fits u32")),
        usage_refused: frame.usage_refused,
        usage_gone: frame.usage_gone,
    }
}

fn apps<'key>(
    aggregate: &'key crate::apps::Aggregate<'key>,
) -> impl Iterator<Item = AppReading<'key>> {
    aggregate
        .apps
        .iter()
        .map(|(key, app)| AppReading {
            key: *key,
            resident: Measure {
                known: app.resident_bytes.known,
                measured_members: app.resident_bytes.measured_members,
                unavailable_members: app.resident_bytes.unavailable_members,
                overflowed: app.resident_bytes.overflowed,
            },
        })
        .filter(|app| super::has_resident(&app.resident))
}
