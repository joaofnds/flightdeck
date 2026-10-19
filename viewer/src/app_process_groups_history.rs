//! One selected-App cohort, with exact measures and one shared captured timeline.
use persistence::{Error, Result};

use crate::apps;
use crate::live_app_response::{KeyKind, Measure, Observation};

pub(crate) mod cache;
pub(crate) mod reduce;

pub const GROUPS_MAX: usize = 24_000;
pub const LEADERS_MAX: usize = 7;
pub const SERIES_GROUPS_MAX: usize = LEADERS_MAX + 1;
pub const POINTS_MAX: usize = 4096;
pub const KEY_BYTES_MAX: usize = 8 * 1024 * 1024;
pub const NAME_BYTES_MAX: usize = collector::ring::PROGRAM_PATH_BYTES_MAX;
pub const RESULT_BYTES_MAX: usize = size_of::<AppProcessGroupsHistory>()
    + NAME_BYTES_MAX
    + SERIES_GROUPS_MAX * (size_of::<ProcessGroup>() + NAME_BYTES_MAX)
    + POINTS_MAX * size_of::<ProcessGroupsPoint>();

pub struct AppProcessGroupsHistory {
    pub selected: Observation,
    pub app_kind: KeyKind,
    pub app_bytes: Box<[u8]>,
    pub context_seconds: u32,
    pub named_group_count: u32,
    pub context_observations_unavailable: u32,
    pub groups: Vec<ProcessGroup>,
    pub points: Vec<ProcessGroupsPoint>,
}

pub enum ProcessGroup {
    Named(Box<[u8]>),
    Other,
}

pub struct ProcessGroupsPoint {
    pub observation: Observation,
    pub elapsed_ns: Option<u64>,
    pub source_available: bool,
    pub membership_complete: bool,
    /// None means group source facts were unavailable, rather than no unnamed members.
    pub unnamed_members: Option<u32>,
    pub readings: [ProcessGroupReading; SERIES_GROUPS_MAX],
    pub(crate) live: bool,
}

#[derive(Clone, Copy)]
pub struct ProcessGroupReading {
    pub present: bool,
    pub members: u32,
    pub measures: [Measure; 7],
}

#[derive(Clone, Copy)]
pub enum ProcessGroupMetric {
    Cpu,
    Power,
    DiskRead,
    DiskWritten,
    DiskTotal,
    Resident,
    Footprint,
}

const _: () = {
    assert!(SERIES_GROUPS_MAX * 2 == 16);
    assert!(size_of::<ProcessGroupReading>() <= 176);
    assert!(RESULT_BYTES_MAX < 8 * 1024 * 1024);
    assert!(POINTS_MAX == crate::session::OBSERVATIONS_MAX);
};

impl AppProcessGroupsHistory {
    #[must_use]
    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.app_bytes.len()
            + self.groups.capacity() * size_of::<ProcessGroup>()
            + self
                .groups
                .iter()
                .map(|group| match group {
                    ProcessGroup::Named(bytes) => bytes.len(),
                    ProcessGroup::Other => 0,
                })
                .sum::<usize>()
            + self.points.capacity() * size_of::<ProcessGroupsPoint>()
    }

    pub(crate) fn validate_bytes(&self) -> Result<()> {
        if self.bytes() > RESULT_BYTES_MAX {
            return Err(Error::Bound("process cohort result bytes"));
        }

        Ok(())
    }
}

impl ProcessGroupReading {
    pub(crate) const EMPTY: Self = Self {
        present: false,
        members: 0,
        measures: [Measure {
            known: 0,
            measured_members: 0,
            unavailable_members: 0,
            overflowed: false,
        }; 7],
    };

    pub(crate) fn add(&mut self, source: &Self) -> Result<()> {
        self.present = true;
        self.members = self
            .members
            .checked_add(source.members)
            .ok_or(Error::Bound("process cohort members"))?;
        if self.members > 24_000 {
            return Err(Error::Bound("process cohort members"));
        }

        for (target, source) in self.measures.iter_mut().zip(&source.measures) {
            if let Some(known) = target.known.checked_add(source.known) {
                target.known = known;
            } else {
                target.overflowed = true;
            }

            target.overflowed |= source.overflowed;
            target.measured_members = target
                .measured_members
                .checked_add(source.measured_members)
                .ok_or(Error::Bound("process cohort measured members"))?;
            target.unavailable_members = target
                .unavailable_members
                .checked_add(source.unavailable_members)
                .ok_or(Error::Bound("process cohort unavailable members"))?;
        }

        Ok(())
    }
}

impl ProcessGroupMetric {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Cpu => 0,
            Self::Power => 1,
            Self::DiskRead => 2,
            Self::DiskWritten => 3,
            Self::DiskTotal => 4,
            Self::Resident => 5,
            Self::Footprint => 6,
        }
    }

    #[must_use]
    pub fn value(self, reading: &ProcessGroupReading, elapsed_ns: Option<u64>) -> Option<f64> {
        if !reading.present {
            return None;
        }

        let amount = reading.measures[self.index()].measured()?;

        match self {
            Self::Cpu => apps::cpu_percent_one_core(apps::CpuRate {
                cpu_ns: amount,
                elapsed_ns: elapsed_ns?,
            }),
            Self::Power => apps::energy_watts(apps::EnergyRate {
                energy_nj: amount,
                elapsed_ns: elapsed_ns?,
            }),
            Self::DiskRead | Self::DiskWritten | Self::DiskTotal => {
                apps::bytes_per_second(apps::BytesRate {
                    bytes: amount,
                    elapsed_ns: elapsed_ns?,
                })
            }
            Self::Resident | Self::Footprint => Some(value_number(amount)),
        }
    }
}

fn value_number(value: u64) -> f64 {
    let upper = u32::try_from(value >> 32).unwrap_or_else(|_| unreachable!());
    let lower = u32::try_from(value & u64::from(u32::MAX)).unwrap_or_else(|_| unreachable!());

    f64::from(upper) * 4_294_967_296.0 + f64::from(lower)
}

#[cfg(test)]
#[path = "app_process_groups_history/bounds_tests.rs"]
mod bounds_tests;
