use persistence::{Error, Result};

use crate::app_process_groups_history::ProcessGroupReading;
use crate::app_process_groups_history::reduce::{Capture, Phase, Reduction};
use crate::cancellation::Operation;
use crate::live_app_history::{History, Point, Target, key};
use crate::live_app_response::{Observation, Sample};
use crate::live_apps::Frame;

struct SavedGroup<'name> {
    name: &'name [u8],
    totals: crate::apps::AppTotals,
}

pub(crate) const CAPTURE_WORK_BYTES_MAX: usize = size_of::<Vec<SavedGroup<'static>>>()
    + crate::apps::PROCESS_INTERVALS_MAX * size_of::<SavedGroup<'static>>();

const _: () = assert!(CAPTURE_WORK_BYTES_MAX < 5 * 1024 * 1024);

impl History {
    pub(crate) fn process_groups_visit(
        &self,
        target: &Target<'_>,
        reduction: &mut Reduction<'_>,
        phase: Phase,
    ) -> Result<()> {
        for point in &self.points {
            process_groups_visit_point(point, target, reduction, phase, true)?;
        }
        Ok(())
    }
}

pub(crate) struct Snapshot {
    pub observation: Observation,
    pub elapsed_ns: Option<u64>,
    pub membership_complete: bool,
    pub unnamed_members: Option<u32>,
    pub groups: Vec<NamedReading>,
}

pub(crate) struct NamedReading {
    pub name: Box<[u8]>,
    pub reading: ProcessGroupReading,
}

pub(crate) fn capture(
    frame: &Frame,
    previous: Option<&Frame>,
    target: &Target<'_>,
    operation: &Operation,
) -> Result<Box<Snapshot>> {
    operation.check()?;
    let frame_bytes = frame
        .bytes()
        .checked_add(previous.map_or(0, Frame::bytes))
        .ok_or(Error::Bound("process cohort frame bytes"))?;
    if frame_bytes > 128 * 1024 * 1024 {
        return Err(Error::Bound("process cohort frame bytes"));
    }

    let mut sample = None;
    Sample::capture(frame, previous, &mut sample)?;
    let sample = sample.ok_or(Error::Bound("process cohort saved sample"))?;
    let app_index = sample.apps().iter().position(|app| {
        if app.key.kind != target.kind {
            return false;
        }

        app.key.bytes.as_slice() == target.bytes
    });

    let mut groups = Vec::new();
    capture_groups(&sample, app_index, &mut groups, operation)?;
    let named: u32 = groups.iter().map(|group| group.totals.members).sum();
    let members = app_index.map_or(0, |index| sample.apps()[index].members);
    let unnamed = members
        .checked_sub(named)
        .ok_or(Error::Bound("process cohort saved unnamed members"))?;
    let bytes = size_of::<Snapshot>()
        + groups.len() * size_of::<NamedReading>()
        + groups.iter().map(|group| group.name.len()).sum::<usize>();
    if bytes > 8 * 1024 * 1024 {
        return Err(Error::Bound("process cohort snapshot bytes"));
    }

    let mut snapshot = Box::new(Snapshot {
        observation: sample.selected,
        elapsed_ns: sample.elapsed_ns,
        membership_complete: sample.membership_complete,
        unnamed_members: Some(unnamed),
        groups: Vec::with_capacity(groups.len()),
    });

    capture_readings(&mut snapshot, &mut groups, operation)?;

    Ok(snapshot)
}

fn capture_readings(
    snapshot: &mut Snapshot,
    groups: &mut [SavedGroup<'_>],
    operation: &Operation,
) -> Result<()> {
    for group in groups {
        operation.check()?;
        group.totals.membership_complete = snapshot.membership_complete;
        let mut totals = None;
        crate::live_app_history::trace::Totals::capture(&group.totals, &mut totals);
        let totals = totals.ok_or(Error::Bound("process cohort saved totals"))?;
        let mut reading = ProcessGroupReading::EMPTY;
        totals.process_group_reading(&mut reading);
        snapshot.groups.push(NamedReading {
            name: group.name.into(),
            reading,
        });
    }
    Ok(())
}

impl Snapshot {
    pub(crate) fn visit(&self, reduction: &mut Reduction<'_>, phase: Phase) -> Result<()> {
        if !reduction.begin(
            &Capture {
                observation: &self.observation,
                elapsed_ns: self.elapsed_ns,
                source_available: true,
                membership_complete: self.membership_complete,
                unnamed_members: self.unnamed_members,
                live: false,
            },
            phase,
        )? {
            return Ok(());
        }

        for group in &self.groups {
            reduction.group(&group.name, &group.reading)?;
        }

        Ok(())
    }
}

fn capture_groups<'sample>(
    sample: &'sample Sample,
    app_index: Option<usize>,
    groups: &mut Vec<SavedGroup<'sample>>,
    operation: &Operation,
) -> Result<()> {
    let members = app_index.map_or(0, |index| sample.apps()[index].members);
    let capacity =
        usize::try_from(members).map_err(|_| Error::Bound("process cohort saved member count"))?;
    *groups = Vec::with_capacity(capacity);
    if groups.capacity() > crate::apps::PROCESS_INTERVALS_MAX {
        return Err(Error::Bound("process cohort saved work bytes"));
    }

    let app_index = app_index
        .map(u32::try_from)
        .transpose()
        .map_err(|_| Error::Bound("process cohort saved App index"))?;
    let Some(app_index) = app_index else {
        return Ok(());
    };

    for process in sample.processes() {
        operation.check()?;
        if process.app_index != Some(app_index) {
            continue;
        }

        let Some(name) = &process.executable_name else {
            continue;
        };

        if name.bytes.is_empty() {
            continue;
        }

        let index = match groups.binary_search_by(|group| group.name.cmp(&name.bytes)) {
            Ok(index) => index,
            Err(index) => {
                if groups.len() == capacity {
                    return Err(Error::Bound("process cohort saved groups"));
                }

                groups.insert(
                    index,
                    SavedGroup {
                        name: &name.bytes,
                        totals: crate::apps::AppTotals::default(),
                    },
                );
                index
            }
        };

        capture_groups_add(&mut groups[index].totals, process);
    }

    Ok(())
}

fn capture_groups_add(
    totals: &mut crate::apps::AppTotals,
    process: &crate::live_app_response::Process,
) {
    super::add(
        totals,
        &super::Member {
            attribution: process.app_attribution,
            live: process.state == crate::apps::ProcessState::Live,
            amounts: [
                process.cpu_ns,
                process.energy_nj,
                process.disk_read_bytes,
                process.disk_written_bytes,
                process.disk_io_bytes,
                process.resident_bytes,
                process.footprint_bytes,
            ],
        },
    );
}

fn process_groups_visit_point(
    point: &Point,
    target: &Target<'_>,
    reduction: &mut Reduction<'_>,
    phase: Phase,
    live: bool,
) -> Result<()> {
    let app = key(target.kind, target.bytes);
    let index = point
        .apps
        .partition_point(|candidate| candidate.key.view() < app);
    let app_index = point
        .apps
        .get(index)
        .filter(|candidate| candidate.key.view() == app)
        .map(|_| index);
    let unnamed_members = process_groups_visit_point_unnamed_members(point, app_index)?;
    if !reduction.begin(
        &Capture {
            observation: &point.observation,
            elapsed_ns: point.elapsed_ns,
            source_available: point.groups.is_some(),
            membership_complete: point.membership_complete,
            unnamed_members,
            live,
        },
        phase,
    )? {
        return Ok(());
    }

    let Some(app_index) = app_index else {
        return Ok(());
    };

    let groups = point
        .groups
        .as_ref()
        .ok_or(Error::Bound("process cohort group source"))?;
    for group in groups {
        if usize::from(group.app_index) != app_index {
            continue;
        }

        let totals = group
            .totals
            .as_ref()
            .ok_or(Error::Bound("process cohort group totals"))?;
        let mut reading = ProcessGroupReading::EMPTY;
        totals.process_group_reading(&mut reading);

        reduction.group(&group.executable, &reading)?;
    }

    Ok(())
}

fn process_groups_visit_point_unnamed_members(
    point: &Point,
    app_index: Option<usize>,
) -> Result<Option<u32>> {
    let Some(groups) = &point.groups else {
        return Ok(None);
    };

    let Some(app_index) = app_index else {
        return Ok(Some(0));
    };

    let mut named = 0_u32;
    for group in groups {
        if usize::from(group.app_index) == app_index {
            if !group.executable.is_empty() {
                let totals = group
                    .totals
                    .as_ref()
                    .ok_or(Error::Bound("process cohort member totals"))?;
                named += u32::from(totals.members);
            }
        }
    }

    let members = u32::from(point.apps[app_index].members);

    Ok(Some(
        members
            .checked_sub(named)
            .ok_or(Error::Bound("process cohort unnamed members"))?,
    ))
}
