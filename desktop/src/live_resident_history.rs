use std::collections::BTreeSet;
use std::rc::Rc;

use num_traits::ToPrimitive;
use serde_json::Value;

use crate::data::{Line, Panel};
use crate::live_hover::Target;
use crate::live_projection::{identity, integer};

pub const PANEL_ID: &str = "live-memory-app-resident";
pub const TITLE: &str = "Resident memory by app";
const POINTS_MAX: usize = 4096;
const COHORT_MAX: usize = 8;
const NS: u64 = 1_000_000_000;
pub(crate) const COLORS: [&str; 9] = crate::live_projection::APP_COLORS;
const _: () = {
    assert!(COLORS.len() == COHORT_MAX + 1);
    assert!(POINTS_MAX == crate::data::LIVE_SAMPLE_LIMIT);
    assert!(size_of::<Point>() + COHORT_MAX * size_of::<Option<Measure>>() <= 512);
    assert!(POINTS_MAX * 512 == 2 * 1024 * 1024);
};

pub enum ResidentHistory {
    Ready(History),
    Unavailable(String),
}

pub struct History {
    points: Vec<Point>,
    cohort: Vec<String>,
    positions: Vec<f32>,
    window: crate::chart_axes::TimeWindow,
    selected_index: usize,
    retention_limited: bool,
}

struct Point {
    observation: Observation,
    segment: u64,
    source_available: bool,
    membership_complete: Option<bool>,
    ungrouped_members: Option<u32>,
    usage_refused: Option<u32>,
    usage_gone: Option<u32>,
    apps: Vec<Option<Measure>>,
    other: Option<Other>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Observation {
    pub(crate) wall_ns: Option<u64>,
    pub(crate) recording: [u64; 2],
    pub(crate) boot: [u64; 2],
    pub(crate) run: [u64; 2],
    pub(crate) publication: u64,
    run_tick: u64,
    continuous_ns: Option<u64>,
    awake_ns: Option<u64>,
}

#[derive(Clone, Copy)]
struct Measure {
    known: u64,
    measured_members: u32,
    unavailable_members: u32,
    overflowed: bool,
}

struct Other {
    app_count: u32,
    resident_bytes: Option<Measure>,
}

impl ResidentHistory {
    pub fn prepare(value: &Value, selection: &Value) -> Self {
        match History::decode(value, selection) {
            Ok(history) => Self::Ready(history),
            Err(error) => Self::Unavailable(error),
        }
    }

    pub fn panel(&self) -> Option<Rc<Panel>> {
        match self {
            Self::Ready(history) => Some(Rc::new(history.panel())),
            Self::Unavailable(_) => None,
        }
    }

    pub fn index(&self, target: Option<&Target>) -> Option<usize> {
        let Self::Ready(history) = self else {
            return None;
        };
        let Some(target) = target else {
            return Some(history.selected_index);
        };

        history
            .points
            .iter()
            .position(|point| point.observation.matches(target))
    }

    pub fn reading(&self, line: usize, index: Option<usize>) -> String {
        let Self::Ready(history) = self else {
            return "Unavailable".into();
        };
        let Some(point) = index.and_then(|index| history.points.get(index)) else {
            return "Unavailable".into();
        };
        let measure = point.measure(line);
        let Some(measure) = measure else {
            return "Unavailable".into();
        };
        if measure.overflowed {
            return "Unavailable · byte sum overflow".into();
        }
        if point.value(line).is_none() {
            return format!("Unavailable · {} unavailable", measure.unavailable_members);
        }

        let reading = crate::live_app_views::memory(Some(measure.known));
        if measure.unavailable_members > 0 {
            return format!(
                "{reading} known subtotal · {} measured · {} unavailable",
                measure.measured_members, measure.unavailable_members
            );
        }

        reading
    }

    #[cfg(test)]
    pub fn amount(&self, line: usize, index: Option<usize>) -> Option<u64> {
        let Self::Ready(history) = self else {
            return None;
        };
        let point = history.points.get(index?)?;
        point.value(line)?;

        Some(point.measure(line)?.known)
    }

    pub fn coverage(&self, index: Option<usize>) -> String {
        let history = match self {
            Self::Ready(history) => history,
            Self::Unavailable(error) => {
                return format!("Resident App history unavailable: {error}");
            }
        };
        let Some(point) = index.and_then(|index| history.points.get(index)) else {
            return "Resident App history unavailable at this observation".into();
        };
        if !point.source_available {
            return "Resident App source unavailable at this observation".into();
        }

        let membership = if point.complete() {
            "complete"
        } else {
            "partial"
        };
        let other = point.other.as_ref().map_or(0, |other| other.app_count);
        let retention = if history.retention_limited {
            " · retained context is limited"
        } else {
            ""
        };
        format!("Membership {membership} · {other} Other Apps{retention}")
    }

    pub fn headline(&self, index: Option<usize>) -> String {
        let Self::Ready(history) = self else {
            return "Unavailable".into();
        };
        let Some(point) = index.and_then(|index| history.points.get(index)) else {
            return "Unavailable".into();
        };
        if !point.source_available {
            return "Unavailable".into();
        }
        let mut known = 0_u128;
        let mut measured = false;
        let mut complete = point.complete();
        for line in 0..=history.cohort.len() {
            if point.value(line).is_some() {
                if let Some(measure) = point.measure(line) {
                    known += u128::from(measure.known);
                    measured = true;
                    if measure.unavailable_members > 0 {
                        complete = false;
                    }
                }
            } else {
                complete = false;
            }
        }
        if !measured {
            return "Unavailable".into();
        }
        if complete {
            format!("{known} B captured resident")
        } else {
            format!("{known} B known resident subtotal")
        }
    }
}

impl History {
    fn decode(value: &Value, selection: &Value) -> Result<Self, String> {
        if integer(&value["context_seconds"])? != 900 {
            return Err("Resident App context must contain 900 seconds".into());
        }
        let selected = Observation::decode(&value["selected"])?;
        crate::live_source::identity_validate(&value["selected"], &selection["selected"])?;
        let second = integer(&selection["second"])?;
        if selected.wall_ns.map(|wall| wall / NS) != Some(second) {
            return Err("Resident App history has a different selected timestamp".into());
        }
        let last = integer(&selection["context_last_second"])?;
        let cohort = decode_cohort(&value["cohort"])?;
        let values = value["points"]
            .as_array()
            .ok_or("Missing resident App points")?;
        if values.is_empty() {
            return Err("Resident App history is empty".into());
        }
        if values.len() > POINTS_MAX {
            return Err("Resident App history exceeds 4096 points".into());
        }
        let mut history = Self {
            points: Vec::with_capacity(values.len()),
            cohort,
            positions: Vec::with_capacity(values.len()),
            window: crate::chart_axes::TimeWindow::charted(last),
            selected_index: 0,
            retention_limited: boolean(&value["coverage"]["retention_limited"])?,
        };
        let mut selected_index = None;
        let mut identities = BTreeSet::new();
        for value in values {
            let point = Point::decode(value, history.cohort.len())?;
            if !identities.insert((
                point.observation.recording,
                point.observation.boot,
                point.observation.run,
                point.observation.publication,
            )) {
                return Err("Duplicate resident App observation".into());
            }
            if let Some(previous) = history.points.last() {
                if previous.observation >= point.observation {
                    return Err("Resident App observations are not ordered".into());
                }
            }
            let position = position(point.observation.wall_ns, last)?;
            if point.observation == selected {
                selected_index = Some(history.points.len());
            }
            history.positions.push(position);
            history.points.push(point);
        }
        history.selected_index =
            selected_index.ok_or("Selected resident App observation missing")?;
        validate_coverage(&value["coverage"])?;

        Ok(history)
    }

    fn panel(&self) -> Panel {
        let mut lines = Vec::with_capacity(self.cohort.len() + 1);
        for (index, name) in std::iter::once("Other")
            .chain(self.cohort.iter().map(String::as_str))
            .enumerate()
        {
            lines.push(Line {
                name: name.strip_suffix(".app").unwrap_or(name).into(),
                color: COLORS[index].into(),
                values: self.points.iter().map(|point| point.value(index)).collect(),
                stack: None,
                negative: Some(false),
                dashed: Some(false),
                axis: None,
                follows: None,
                unit: Some("GiB".into()),
                fill: None,
            });
        }

        Panel {
            id: PANEL_ID.into(),
            title: TITLE.into(),
            unit: "GiB".into(),
            kind: "line".into(),
            lines,
            minimum: Some(0.0),
            maximum: None,
            stack: Some(true),
            fill: Some(1.0),
            caption: None,
            help: Some(
                "Process resident memory counts shared pages for each process. It can \
                exceed machine memory. Eight leaders use the complete interval's mean; Other \
                sums remaining captured Apps. Known subtotals exclude unavailable members. \
                Missing Apps and unavailable observations remain gaps."
                    .into(),
            ),
            source: Some("Captured process resident memory".into()),
            quantities: None,
            states: None,
            table_headings: None,
            table_rows: None,
            positions: Some(self.positions.clone()),
            segments: Some(self.points.iter().map(|point| point.segment).collect()),
            selected_index: Some(self.selected_index),
            window: Some(self.window),
            detail_first: None,
            sliding: false,
            chart_width: std::cell::Cell::new(None),
        }
    }
}

impl Point {
    fn decode(value: &Value, count: usize) -> Result<Self, String> {
        let apps = value["apps"]
            .as_array()
            .ok_or("Missing resident App measures")?;
        if apps.len() != count {
            return Err("Resident App measure count differs from cohort".into());
        }
        let point = Self {
            observation: Observation::decode(&value["observation"])?,
            segment: integer(&value["segment"])?,
            source_available: boolean(&value["source_available"])?,
            membership_complete: optional_boolean(&value["membership_complete"])?,
            ungrouped_members: optional_count(&value["ungrouped_members"])?,
            usage_refused: optional_count(&value["usage_refused"])?,
            usage_gone: optional_count(&value["usage_gone"])?,
            apps: apps
                .iter()
                .map(Measure::decode_optional)
                .collect::<Result<_, _>>()?,
            other: Other::decode_optional(&value["other"])?,
        };
        assert!(point.apps.len() <= COHORT_MAX);
        point.validate()?;

        Ok(point)
    }

    fn validate(&self) -> Result<(), String> {
        let coverage_present = [
            self.membership_complete.is_some(),
            self.ungrouped_members.is_some(),
            self.usage_refused.is_some(),
            self.usage_gone.is_some(),
            self.other.is_some(),
        ];
        if self.source_available {
            if coverage_present.contains(&false) {
                return Err("Available resident App source lacks coverage".into());
            }
        } else {
            if coverage_present.contains(&true) {
                return Err("Unavailable resident App source contains coverage".into());
            }
            if self.apps.iter().any(Option::is_some) {
                return Err("Unavailable resident App source contains readings".into());
            }
        }

        Ok(())
    }

    fn complete(&self) -> bool {
        [
            self.source_available,
            self.membership_complete == Some(true),
            self.ungrouped_members == Some(0),
            self.usage_refused == Some(0),
            self.usage_gone == Some(0),
        ]
        .iter()
        .all(|complete| *complete)
    }

    fn measure(&self, line: usize) -> Option<Measure> {
        if line == 0 {
            return self.other.as_ref()?.resident_bytes;
        }

        self.apps.get(line - 1).copied().flatten()
    }

    fn value(&self, line: usize) -> Option<f32> {
        self.observation.wall_ns?;
        if !self.source_available {
            return None;
        }
        let measure = self.measure(line)?;
        if measure.overflowed {
            return None;
        }
        if measure.measured_members == 0 {
            if line != 0 {
                return None;
            }
            if !self.complete() {
                return None;
            }
            if self.other.as_ref()?.app_count != 0 {
                return None;
            }
            if measure.known != 0 {
                return None;
            }
            if measure.unavailable_members != 0 {
                return None;
            }
        }

        Some(measure.known.to_f32()? / 1_073_741_824.0)
    }
}

impl Observation {
    pub(crate) fn decode(value: &Value) -> Result<Self, String> {
        let stamp = &value["stamp"];
        Ok(Self {
            recording: identity(&value["recording"])?,
            boot: identity(&stamp["boot"])?,
            run: identity(&stamp["run"])?,
            publication: integer(&stamp["publication"])?,
            run_tick: integer(&value["run_tick"])?,
            wall_ns: optional_integer(&value["wall_ns"])?,
            continuous_ns: optional_integer(&stamp["continuous_ns"])?,
            awake_ns: optional_integer(&stamp["awake_ns"])?,
        })
    }

    pub(crate) fn matches(&self, target: &Target) -> bool {
        let Some(identity) = &target.identity else {
            return false;
        };
        let Ok(run) = crate::live_projection::identity(&identity["run"]) else {
            return false;
        };
        let Ok(boot) = crate::live_projection::identity(&identity["boot"]) else {
            return false;
        };
        let Ok(publication) = integer(&identity["publication"]) else {
            return false;
        };

        [
            self.publication == publication,
            self.run == run,
            self.boot == boot,
            self.wall_ns.map(|wall| wall / NS) == Some(target.second),
        ]
        .iter()
        .all(|matching| *matching)
    }
}

impl Measure {
    fn decode_optional(value: &Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }

        Ok(Some(Self {
            known: integer(&value["known"])?,
            measured_members: count(&value["measured_members"])?,
            unavailable_members: count(&value["unavailable_members"])?,
            overflowed: boolean(&value["overflowed"])?,
        }))
    }
}

impl Other {
    fn decode_optional(value: &Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }

        Ok(Some(Self {
            app_count: count(&value["app_count"])?,
            resident_bytes: Measure::decode_optional(&value["resident_bytes"])?,
        }))
    }
}

fn decode_cohort(value: &Value) -> Result<Vec<String>, String> {
    let entries = value.as_array().ok_or("Missing resident App cohort")?;
    if entries.len() > COHORT_MAX {
        return Err("Resident App cohort exceeds eight leaders".into());
    }
    let mut names = Vec::with_capacity(entries.len());
    let mut keys = BTreeSet::new();
    for entry in entries {
        let key = crate::live_source::app_key_from_json(entry)?;
        let bundle = key.kind == "bundle";
        if !keys.insert((key.kind, key.bytes_hex)) {
            return Err("Duplicate resident App cohort key".into());
        }
        let name = entry["key"]["display"]
            .as_str()
            .ok_or("Missing captured resident App name")?;
        if name.len() > 16_384 {
            return Err("Resident App display exceeds its bound".into());
        }
        let sum = entry["sum_bytes"]
            .as_str()
            .ok_or("Missing resident App mean sum")?;
        if sum.len() > 39 {
            return Err("Resident App mean sum exceeds u128".into());
        }
        sum.parse::<u128>().map_err(|error| error.to_string())?;
        let samples = count(&entry["samples"])?;
        if !(1..=4096).contains(&samples) {
            return Err("Resident App mean sample count exceeds its context".into());
        }
        let name = if bundle {
            name.rsplit('/')
                .next()
                .ok_or("Missing resident App basename")?
        } else {
            name
        };
        names.push(name.into());
    }
    decode_cohort_labels(entries, &mut names)?;

    Ok(names)
}

pub(crate) fn decode_cohort_labels(entries: &[Value], names: &mut [String]) -> Result<(), String> {
    assert!(names.len() <= COHORT_MAX);
    assert_eq!(entries.len(), names.len());
    let collisions = decode_cohort_label_collisions(names);
    for (index, name) in names.iter_mut().enumerate() {
        if collisions[index] {
            *name = entries[index]["key"]["display"]
                .as_str()
                .ok_or("Missing captured resident App display")?
                .into();
        }
    }
    let collisions = decode_cohort_label_collisions(names);
    for (index, name) in names.iter_mut().enumerate() {
        if collisions[index] {
            let kind = entries[index]["key"]["kind"]
                .as_str()
                .ok_or("Missing captured resident App kind")?;
            *name = format!("{name} ({kind})");
        }
    }
    let mut used = BTreeSet::from(["Other".to_owned()]);
    for name in names {
        let base = name.clone();
        for suffix in 0..=COHORT_MAX {
            if !used.contains(name) {
                break;
            }
            *name = format!("{base} [{suffix}]");
        }
        assert!(used.insert(name.clone()));
    }

    Ok(())
}

fn decode_cohort_label_collisions(names: &[String]) -> [bool; COHORT_MAX] {
    assert!(names.len() <= COHORT_MAX);
    let mut collisions = [false; COHORT_MAX];
    for (index, name) in names.iter().enumerate() {
        collisions[index] = name == "Other";
        for (other_index, other) in names.iter().enumerate() {
            if index != other_index {
                if name == other {
                    collisions[index] = true;
                }
            }
        }
    }

    collisions
}

pub(crate) fn position(wall_ns: Option<u64>, second: u64) -> Result<f32, String> {
    let Some(wall_ns) = wall_ns else {
        return Ok(0.0);
    };
    let window = crate::chart_axes::TimeWindow::charted(second);
    let end = second
        .checked_add(1)
        .ok_or("Resident App context clock overflow")?;
    if !(window.first..end).contains(&(wall_ns / NS)) {
        return Err("Resident App observation lies outside its charted seconds".into());
    }
    let seconds = u16::try_from(wall_ns / NS - window.first).map_err(|error| error.to_string())?;
    let nanos = u32::try_from(wall_ns % NS).map_err(|error| error.to_string())?;
    let span = u16::try_from(window.span).map_err(|error| error.to_string())?;

    Ok(
        (f32::from(seconds)
            + nanos.to_f32().ok_or("Resident clock exceeds f32")? / 1_000_000_000.0)
            / f32::from(span),
    )
}

pub(crate) fn validate_coverage(value: &Value) -> Result<(), String> {
    for (name, bound) in [
        ("context_app_count", 24_000),
        ("context_key_bytes", 8_388_608),
        ("cache_bytes", 67_108_864),
    ] {
        if integer(&value[name])? > bound {
            return Err(format!("Resident App {name} exceeds its bound"));
        }
    }

    Ok(())
}

pub(crate) fn count(value: &Value) -> Result<u32, String> {
    u32::try_from(integer(value)?).map_err(|error| error.to_string())
}

pub(crate) fn boolean(value: &Value) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| "Missing resident App coverage flag".into())
}

pub(crate) fn optional_integer(value: &Value) -> Result<Option<u64>, String> {
    if value.is_null() {
        return Ok(None);
    }

    integer(value).map(Some)
}

pub(crate) fn optional_count(value: &Value) -> Result<Option<u32>, String> {
    if value.is_null() {
        return Ok(None);
    }

    count(value).map(Some)
}

pub(crate) fn optional_boolean(value: &Value) -> Result<Option<bool>, String> {
    if value.is_null() {
        return Ok(None);
    }

    boolean(value).map(Some)
}

#[cfg(test)]
#[path = "live_resident_history_tests.rs"]
mod tests;

#[path = "live_resident_history_view.rs"]
mod view;
