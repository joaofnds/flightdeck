use persistence::Result;

use super::{AppReading, Capture, Reduction, Source};
use crate::live_app_response::{Header, Measure, Observation};
use crate::live_apps::Frame;
use crate::session::KeptResident;

/// The resident memory of an observation's Apps as the recording gives it: from a replay of its
/// minute, or from the parts its sealed window keeps.
pub(crate) enum SavedResidents<'saved> {
    Replayed(&'saved Frame),
    Sealed {
        header: &'saved Header,
        apps: &'saved [KeptResident<'saved>],
    },
}

impl SavedResidents<'_> {
    /// The observation whose Apps' resident memory this is.
    pub(crate) fn observation(&self) -> Observation {
        match self {
            Self::Replayed(frame) => Observation {
                recording: frame.identity().recording.0,
                stamp: *frame.stamp(),
                run_tick: frame.run_tick(),
                wall_ns: frame.wall_ns(),
            },
            Self::Sealed { header, .. } => header.selected,
        }
    }
}

impl Reduction<'_> {
    pub(crate) fn observe_saved(&mut self, saved: &SavedResidents<'_>) -> Result<()> {
        match saved {
            SavedResidents::Replayed(frame) => frame.with_aggregate(None, |aggregate| {
                self.observe(&capture(frame, aggregate), apps(aggregate), Source::Saved)
            }),
            SavedResidents::Sealed { header, apps } => {
                self.observe(&capture_header(header), kept_readings(apps), Source::Saved)
            }
        }
    }

    pub(crate) fn project_saved(&mut self, saved: &SavedResidents<'_>) -> Result<()> {
        match saved {
            SavedResidents::Replayed(frame) => frame.with_aggregate(None, |aggregate| {
                self.project(&capture(frame, aggregate), apps(aggregate), Source::Saved)
            }),
            SavedResidents::Sealed { header, apps } => {
                self.project(&capture_header(header), kept_readings(apps), Source::Saved)
            }
        }
    }
}

/// The facts of an observation the parts of its sealed window keep.
pub(crate) fn capture_header(header: &Header) -> Capture {
    Capture {
        observation: header.selected,
        membership_complete: header.membership_complete,
        ungrouped_members: header.live_ungrouped_members,
        usage_refused: header.usage_refused,
        usage_gone: header.usage_gone,
    }
}

/// The Apps with resident memory of an observation the parts of its sealed window keep, which
/// are the Apps a replay of its minute gives.
pub(crate) fn kept_readings<'saved>(
    apps: &'saved [KeptResident<'saved>],
) -> impl Iterator<Item = AppReading<'saved>> {
    apps.iter()
        .map(|app| AppReading {
            key: app.key,
            resident: app.resident,
        })
        .filter(|app| super::has_resident(&app.resident))
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
        ungrouped_members: aggregate.live_ungrouped_members,
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
