//! Every App's row of each observation a sealed minute's App parts keep, and the parts of that
//! table's format its encoder shares. A row holds an App's totals at a second, so the store
//! holds a sealed minute's Apps without running the App rule on it.

use std::ops::Range;

use polars::prelude::{
    BinaryType, BooleanType, DataFrame, DataType, Expr, LazyFrame, UInt8Type, UInt32Type,
    UInt64Type, col, len, lit,
};

use super::{Refused, SealedMinute, placement};
use crate::apps::AppKey;
use crate::live_app_response::{App, KeyKind, MEMBERS_MAX, Measure, Sample};
use persistence::app_tables::{
    MEASURES, PARTS_ROWS_MAX, PARTS_VERSION, ROW_APP, ROW_HEADER, ROW_PROCESS, measure_column_names,
};
use persistence::engine::Cells;
use persistence::schema::APP_PARTS;
use persistence::{Error, Result};

/// The rows a read takes of a minute, one past what it keeps so a longer table is refused.
pub(crate) const ROWS_TAKEN: u32 = PARTS_ROWS_MAX + 1;
/// The observations a minute holds, at most one a second and the one a stop seals early.
const SAMPLES_MAX: usize = crate::live_apps::committed::ENDPOINTS_MAX;

/// The columns of a row of an observation's facts the read takes, the row's kind first.
const SAMPLE_COLUMNS: [&str; 17] = [
    "row_kind",
    "publication",
    "run_tick",
    "wall_ns",
    "continuous_ns",
    "awake_ns",
    "previous_publication",
    "previous_continuous_ns",
    "previous_awake_ns",
    "elapsed_ns",
    "membership_complete",
    "ungrouped_members",
    "live_ungrouped_members",
    "usage_refused",
    "usage_gone",
    "app_count",
    "process_count",
];
/// The columns of an App row the read takes, besides its measures.
const APP_COLUMNS: [&str; 6] = [
    "app_index",
    "app_kind",
    "app_key",
    "members",
    "live_members",
    "app_membership_complete",
];

/// One minute's Apps at each observation its App parts keep, in publication order. The caller
/// owns it and reuses it from minute to minute.
#[derive(Debug, Default, PartialEq)]
pub struct AppMinute {
    /// The facts of each observation's sample.
    pub samples: Vec<SampleRow>,
    /// Each observation's Apps, in publication order and then the order of its sample.
    pub apps: Vec<AppRow>,
    /// The bytes of every App's key end to end, which each App row names by its place.
    pub keys: Vec<u8>,
}

/// The facts of an observation's sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRow {
    pub publication: u64,
    pub run_tick: u64,
    /// Each clock is `None` where the collector did not read it at the tick.
    pub wall_ns: Option<u64>,
    pub continuous_ns: Option<u64>,
    pub awake_ns: Option<u64>,
    /// The observation the sample took its rates from, `None` where it took none.
    pub previous: Option<Previous>,
    pub elapsed_ns: Option<u64>,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    /// The ungrouped members live at the observation, which an exited process does not join.
    pub live_ungrouped_members: u32,
    pub usage_refused: u32,
    pub usage_gone: u32,
    pub app_count: u64,
    pub process_count: u64,
}

/// The publication and clocks of the observation a sample took its rates from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Previous {
    pub publication: u64,
    pub continuous_ns: Option<u64>,
    pub awake_ns: Option<u64>,
}

/// An App's totals at an observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppRow {
    pub publication: u64,
    pub kind: KeyKind,
    /// Where the App's key starts in its minute's `keys`, and its size in bytes.
    pub key_offset: usize,
    pub key_size: usize,
    pub members: u32,
    pub live_members: u32,
    pub membership_complete: bool,
    pub cpu_ns: Measure,
    pub energy_nj: Measure,
    pub disk_read_bytes: Measure,
    pub disk_written_bytes: Measure,
    pub disk_io_bytes: Measure,
    pub resident_bytes: Measure,
    pub footprint_bytes: Measure,
    pub gpu_ns: Measure,
}

/// Whether a minute's App parts gave its Apps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppParts {
    /// The minute keeps App parts this build reads, and every App row was read.
    Read,
    /// The minute was sealed without App parts, so its Apps come from the App rule.
    Missing,
    /// The minute keeps App parts of this other version, which this build does not read.
    Version(u8),
}

impl AppMinute {
    pub fn clear(&mut self) {
        self.samples.clear();
        self.apps.clear();
        self.keys.clear();
    }

    /// The bytes of the key of `app`, one of this minute's Apps.
    #[must_use]
    pub fn key(&self, app: &AppRow) -> &[u8] {
        &self.keys[app.key_offset..app.key_offset + app.key_size]
    }

    /// Appends the facts and every App of `sample`, as a sealed minute's App parts keep them.
    pub fn push_sample(&mut self, sample: &Sample) {
        let publication = sample.selected.stamp.publication;
        self.samples.push(SampleRow {
            publication,
            run_tick: sample.selected.run_tick,
            wall_ns: sample.selected.wall_ns,
            continuous_ns: sample.selected.stamp.continuous_ns,
            awake_ns: sample.selected.stamp.awake_ns,
            previous: sample.previous.map(|stamp| Previous {
                publication: stamp.publication,
                continuous_ns: stamp.continuous_ns,
                awake_ns: stamp.awake_ns,
            }),
            elapsed_ns: sample.elapsed_ns,
            membership_complete: sample.membership_complete,
            ungrouped_members: sample.ungrouped_members,
            live_ungrouped_members: sample.live_ungrouped_members,
            usage_refused: sample.usage_refused,
            usage_gone: sample.usage_gone,
            app_count: collector::u64_from_usize(sample.apps().len()),
            process_count: collector::u64_from_usize(sample.processes().len()),
        });
        for app in sample.apps() {
            self.push_sample_app(publication, app);
        }
    }

    fn push_sample_app(&mut self, publication: u64, app: &App) {
        let key_offset = self.keys.len();
        self.keys.extend_from_slice(&app.key.bytes);
        self.apps.push(AppRow {
            publication,
            kind: app.key.kind,
            key_offset,
            key_size: app.key.bytes.len(),
            members: app.members,
            live_members: app.live_members,
            membership_complete: app.membership_complete,
            cpu_ns: app.cpu_ns,
            energy_nj: app.energy_nj,
            disk_read_bytes: app.disk_read_bytes,
            disk_written_bytes: app.disk_written_bytes,
            disk_io_bytes: app.disk_io_bytes,
            resident_bytes: app.resident_bytes,
            footprint_bytes: app.footprint_bytes,
            gpu_ns: app.gpu_ns,
        });
    }
}

/// Every App row of each observation `minute`'s App parts keep, which `into`, empty, takes. A
/// minute sealed without App parts, or with a version this build does not read, gives none.
pub fn read_apps(
    minute: &SealedMinute,
    into: &mut AppMinute,
) -> std::result::Result<AppParts, Refused> {
    let Some(rows) = minute.rows(APP_PARTS) else {
        return Ok(AppParts::Missing);
    };

    let table = minute.scan(APP_PARTS)?;
    Ok(read_app_rows(
        table.plan,
        rows,
        minute.publications(),
        into,
    )?)
}

/// Every App row of the App parts table `plan` of a minute whose seal counted `rows` rows of the
/// observations of `publications`, which `into`, empty, takes, or none where the table is of
/// another version.
pub(crate) fn read_app_rows(
    plan: LazyFrame,
    rows: u64,
    publications: Range<u64>,
    into: &mut AppMinute,
) -> Result<AppParts> {
    assert!(into.samples.is_empty());
    assert!(into.apps.is_empty());
    assert!(into.keys.is_empty());
    let version = read_app_rows_version(plan.clone(), rows)?;
    if version != PARTS_VERSION {
        return Ok(AppParts::Version(version));
    }

    let mut columns: Vec<Expr> = SAMPLE_COLUMNS.iter().map(|name| col(*name)).collect();
    columns.extend(APP_COLUMNS.iter().map(|name| col(*name)));
    for measure in MEASURES {
        columns.extend(measure_column_names(measure).map(col));
    }
    let mut frame = plan
        .filter(col("row_kind").neq(lit(ROW_PROCESS)))
        .select(columns)
        .limit(ROWS_TAKEN)
        .collect()?;
    // A cell of a column in one chunk is read without finding its chunk first, which halves the
    // reading of the rows.
    frame.rechunk_mut();

    read_app_rows_frame(&frame, &publications, into)?;
    Ok(AppParts::Read)
}

/// The version of every row of the App parts table `plan`, which must hold the `rows` rows its
/// seal counted.
fn read_app_rows_version(plan: LazyFrame, rows: u64) -> Result<u8> {
    let summary = plan
        .select([
            col("version").min().alias("first"),
            col("version").max().alias("last"),
            len().cast(DataType::UInt64).alias("rows"),
        ])
        .collect()?;
    let counted = Cells::<UInt64Type>::named(&summary, "rows")?.get(0)?;
    if counted != rows {
        return Err(Error::Bound("sealed parts rows"));
    }
    if counted == 0 {
        return Err(Error::Bound("sealed parts rows"));
    }
    if counted > u64::from(PARTS_ROWS_MAX) {
        return Err(Error::Bound("sealed parts rows"));
    }

    let first = Cells::<UInt8Type>::named(&summary, "first")?.get(0)?;
    if Cells::<UInt8Type>::named(&summary, "last")?.get(0)? != first {
        return Err(Error::Bound("sealed parts version"));
    }

    Ok(first)
}

/// Appends each row of `frame`, the rows of every observation's facts and of its Apps, in
/// publication order with each observation's facts before its Apps and its Apps in their
/// order, of the observations of `publications`. The table keeps each App's rows together,
/// every observation's facts first.
fn read_app_rows_frame(
    frame: &DataFrame,
    publications: &Range<u64>,
    into: &mut AppMinute,
) -> Result<()> {
    let samples = SampleColumns::new(frame)?;
    let apps = AppColumns::new(frame)?;
    let observations = read_app_rows_observations(&samples, frame.height())?;
    let order = placement(
        &observations,
        &samples.publication,
        frame.height(),
        "sealed parts header",
    )?;
    // The first of the Apps of the observation whose facts were read last.
    let mut first_app = 0;
    for row in order {
        let kind = samples.row_kind.get(row)?;
        if kind == ROW_HEADER {
            let sample = samples.sample(row)?;
            if !publications.contains(&sample.publication) {
                return Err(Error::Bound("sealed parts publication"));
            }
            read_app_rows_close(into, first_app)?;
            into.samples.push(sample);
            first_app = into.apps.len();
        } else {
            if kind != ROW_APP {
                return Err(Error::Bound("sealed parts row kind"));
            }
            let Some(sample) = into.samples.last() else {
                return Err(Error::Bound("sealed parts header"));
            };
            if samples.publication.get(row)? != sample.publication {
                return Err(Error::Bound("sealed parts header"));
            }
            let index = collector::usize_from_u32(apps.app_index.get(row)?);
            if index != into.apps.len() - first_app {
                return Err(Error::Bound("sealed parts App"));
            }
            let app = apps.app(row, sample.publication, &mut into.keys)?;
            into.apps.push(app);
        }
    }

    read_app_rows_close(into, first_app)
}

/// Refuses the observation whose facts were read last where its Apps, from `first_app` on, are
/// not as many as its facts count.
fn read_app_rows_close(into: &AppMinute, first_app: usize) -> Result<()> {
    let Some(sample) = into.samples.last() else {
        return Ok(());
    };

    let apps = collector::u64_from_usize(into.apps.len() - first_app);
    if apps != sample.app_count {
        return Err(Error::Bound("sealed parts Apps"));
    }

    Ok(())
}

/// The publication of each observation whose facts the `rows` rows of `samples` hold, in the
/// order the table holds them, refusing more observations than a minute holds or two that do
/// not follow in publication order.
fn read_app_rows_observations(samples: &SampleColumns, rows: usize) -> Result<Vec<u64>> {
    let mut observations: Vec<u64> = Vec::with_capacity(SAMPLES_MAX);
    for row in 0..rows {
        if samples.row_kind.get(row)? == ROW_HEADER {
            let publication = samples.publication.get(row)?;
            if observations.len() == SAMPLES_MAX {
                return Err(Error::Bound("sealed parts observations"));
            }
            if let Some(last) = observations.last() {
                if *last >= publication {
                    return Err(Error::Bound("sealed parts publication"));
                }
            }
            observations.push(publication);
        }
    }

    Ok(observations)
}

/// The columns of a row of an observation's facts.
struct SampleColumns<'frame> {
    row_kind: Cells<'frame, UInt8Type>,
    publication: Cells<'frame, UInt64Type>,
    run_tick: Cells<'frame, UInt64Type>,
    wall_ns: Cells<'frame, UInt64Type>,
    continuous_ns: Cells<'frame, UInt64Type>,
    awake_ns: Cells<'frame, UInt64Type>,
    previous_publication: Cells<'frame, UInt64Type>,
    previous_continuous_ns: Cells<'frame, UInt64Type>,
    previous_awake_ns: Cells<'frame, UInt64Type>,
    elapsed_ns: Cells<'frame, UInt64Type>,
    membership_complete: Cells<'frame, BooleanType>,
    ungrouped_members: Cells<'frame, UInt32Type>,
    live_ungrouped_members: Cells<'frame, UInt32Type>,
    usage_refused: Cells<'frame, UInt32Type>,
    usage_gone: Cells<'frame, UInt32Type>,
    app_count: Cells<'frame, UInt64Type>,
    process_count: Cells<'frame, UInt64Type>,
}

impl<'frame> SampleColumns<'frame> {
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            row_kind: Cells::named(frame, "row_kind")?,
            publication: Cells::named(frame, "publication")?,
            run_tick: Cells::named(frame, "run_tick")?,
            wall_ns: Cells::named(frame, "wall_ns")?,
            continuous_ns: Cells::named(frame, "continuous_ns")?,
            awake_ns: Cells::named(frame, "awake_ns")?,
            previous_publication: Cells::named(frame, "previous_publication")?,
            previous_continuous_ns: Cells::named(frame, "previous_continuous_ns")?,
            previous_awake_ns: Cells::named(frame, "previous_awake_ns")?,
            elapsed_ns: Cells::named(frame, "elapsed_ns")?,
            membership_complete: Cells::named(frame, "membership_complete")?,
            ungrouped_members: Cells::named(frame, "ungrouped_members")?,
            live_ungrouped_members: Cells::named(frame, "live_ungrouped_members")?,
            usage_refused: Cells::named(frame, "usage_refused")?,
            usage_gone: Cells::named(frame, "usage_gone")?,
            app_count: Cells::named(frame, "app_count")?,
            process_count: Cells::named(frame, "process_count")?,
        })
    }

    /// The facts row `row` holds. A sample that took no rates keeps no clocks of the
    /// observation before.
    fn sample(&self, row: usize) -> Result<SampleRow> {
        let previous = match self.previous_publication.optional(row)? {
            Some(publication) => Some(Previous {
                publication,
                continuous_ns: self.previous_continuous_ns.optional(row)?,
                awake_ns: self.previous_awake_ns.optional(row)?,
            }),
            None => None,
        };

        Ok(SampleRow {
            publication: self.publication.get(row)?,
            run_tick: self.run_tick.get(row)?,
            wall_ns: self.wall_ns.optional(row)?,
            continuous_ns: self.continuous_ns.optional(row)?,
            awake_ns: self.awake_ns.optional(row)?,
            previous,
            elapsed_ns: self.elapsed_ns.optional(row)?,
            membership_complete: self.membership_complete.get(row)?,
            ungrouped_members: self.ungrouped_members.get(row)?,
            live_ungrouped_members: self.live_ungrouped_members.get(row)?,
            usage_refused: self.usage_refused.get(row)?,
            usage_gone: self.usage_gone.get(row)?,
            app_count: self.app_count.get(row)?,
            process_count: self.process_count.get(row)?,
        })
    }
}

/// The columns of an App row.
struct AppColumns<'frame> {
    app_index: Cells<'frame, UInt32Type>,
    app_kind: Cells<'frame, UInt8Type>,
    app_key: Cells<'frame, BinaryType>,
    members: Cells<'frame, UInt32Type>,
    live_members: Cells<'frame, UInt32Type>,
    membership_complete: Cells<'frame, BooleanType>,
    cpu_ns: MeasureColumns<'frame>,
    energy_nj: MeasureColumns<'frame>,
    disk_read_bytes: MeasureColumns<'frame>,
    disk_written_bytes: MeasureColumns<'frame>,
    disk_io_bytes: MeasureColumns<'frame>,
    resident_bytes: MeasureColumns<'frame>,
    footprint_bytes: MeasureColumns<'frame>,
    gpu_ns: MeasureColumns<'frame>,
}

impl<'frame> AppColumns<'frame> {
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            app_index: Cells::named(frame, "app_index")?,
            app_kind: Cells::named(frame, "app_kind")?,
            app_key: Cells::named(frame, "app_key")?,
            members: Cells::named(frame, "members")?,
            live_members: Cells::named(frame, "live_members")?,
            membership_complete: Cells::named(frame, "app_membership_complete")?,
            cpu_ns: MeasureColumns::named(frame, "cpu_ns")?,
            energy_nj: MeasureColumns::named(frame, "energy_nj")?,
            disk_read_bytes: MeasureColumns::named(frame, "disk_read_bytes")?,
            disk_written_bytes: MeasureColumns::named(frame, "disk_written_bytes")?,
            disk_io_bytes: MeasureColumns::named(frame, "disk_io_bytes")?,
            resident_bytes: MeasureColumns::named(frame, "resident_bytes")?,
            footprint_bytes: MeasureColumns::named(frame, "footprint_bytes")?,
            gpu_ns: MeasureColumns::named(frame, "gpu_ns")?,
        })
    }

    /// The App row `row` of the observation of `publication` holds, whose key it appends to
    /// `keys`.
    fn app(&self, row: usize, publication: u64, keys: &mut Vec<u8>) -> Result<AppRow> {
        let kind = kind_of(self.app_kind.get(row)?)?;
        let (AppKey::Bundle(key) | AppKey::Executable(key)) =
            kind.checked_key(self.app_key.get(row)?)?;
        let members = kept_members(self.members.get(row)?)?;
        let app = AppRow {
            publication,
            kind,
            key_offset: keys.len(),
            key_size: key.len(),
            members,
            live_members: self.live_members.get(row)?,
            membership_complete: self.membership_complete.get(row)?,
            cpu_ns: self.cpu_ns.measure(row, members)?,
            energy_nj: self.energy_nj.measure(row, members)?,
            disk_read_bytes: self.disk_read_bytes.measure(row, members)?,
            disk_written_bytes: self.disk_written_bytes.measure(row, members)?,
            disk_io_bytes: self.disk_io_bytes.measure(row, members)?,
            resident_bytes: self.resident_bytes.measure(row, members)?,
            footprint_bytes: self.footprint_bytes.measure(row, members)?,
            gpu_ns: self.gpu_ns.measure(row, members)?,
        };

        keys.extend_from_slice(key);
        Ok(app)
    }
}

/// The columns of one measure of App rows.
struct MeasureColumns<'frame> {
    known: Cells<'frame, UInt64Type>,
    measured_members: Cells<'frame, UInt32Type>,
    unavailable_members: Cells<'frame, UInt32Type>,
    overflowed: Cells<'frame, BooleanType>,
}

impl<'frame> MeasureColumns<'frame> {
    /// The columns of the measure `measure`, one of `MEASURES`.
    fn named(frame: &'frame DataFrame, measure: &str) -> Result<Self> {
        let [known, measured, unavailable, overflowed] = measure_column_names(measure);

        Ok(Self {
            known: Cells::named(frame, &known)?,
            measured_members: Cells::named(frame, &measured)?,
            unavailable_members: Cells::named(frame, &unavailable)?,
            overflowed: Cells::named(frame, &overflowed)?,
        })
    }

    /// The measure of row `row`'s App, of `members` members.
    fn measure(&self, row: usize, members: u32) -> Result<Measure> {
        let measure = Measure {
            known: self.known.get(row)?,
            measured_members: self.measured_members.get(row)?,
            unavailable_members: self.unavailable_members.get(row)?,
            overflowed: self.overflowed.get(row)?,
        };

        kept_measure(measure, members)
    }
}

/// A table keeps an App's kind by these codes, so a build that changed one would misread the
/// tables earlier builds wrote at the same version.
const _: () = {
    assert!(kind_code(KeyKind::Bundle) == 0);
    assert!(kind_code(KeyKind::Executable) == 1);
};

pub(crate) const fn kind_code(kind: KeyKind) -> u8 {
    match kind {
        KeyKind::Bundle => 0,
        KeyKind::Executable => 1,
    }
}

pub(crate) fn kind_of(code: u8) -> Result<KeyKind> {
    match code {
        0 => Ok(KeyKind::Bundle),
        1 => Ok(KeyKind::Executable),
        _ => Err(Error::Bound("sealed App kind")),
    }
}

/// An App's `members`, which a history bounds.
pub(crate) fn kept_members(members: u32) -> Result<u32> {
    if collector::usize_from_u32(members) > MEMBERS_MAX {
        return Err(Error::Bound("sealed parts members"));
    }

    Ok(members)
}

/// `measure`, of an App of `members` members.
pub(crate) fn kept_measure(measure: Measure, members: u32) -> Result<Measure> {
    // Each member adds to one count at most, and a history holds counts up to the members.
    let counted = measure
        .measured_members
        .checked_add(measure.unavailable_members)
        .ok_or(Error::Bound("sealed parts measure"))?;
    if counted > members {
        return Err(Error::Bound("sealed parts measure"));
    }

    Ok(measure)
}
