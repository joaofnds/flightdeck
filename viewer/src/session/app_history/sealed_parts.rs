//! Every App's part of each observation of a sealed window: its row, its processes, and the
//! facts of the observation's sample every page shows. The persister derives them as the window
//! seals, so the viewer shows a saved second's App without replaying its minute.
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use polars::prelude::{
    BinaryType, BooleanType, DataFrame, LazyFrame, SortMultipleOptions, UInt8Type, UInt32Type,
    UInt64Type, col, lit,
};

use super::sealed::{Kept, kind_code, kind_of, observed_in};
use super::{ContextRange, Session, TICKS_PER_WINDOW_MAX, WINDOWS_MAX};
use crate::apps::{Amount, AppKey, ProcessCreation, ProcessState, Stamp, Unavailable};
use crate::cancellation::Operation;
use crate::live_app_response::{
    App, AppPart, Bytes, Header, Key, KeyKind, Measure, Observation, Present, Process,
};
use crate::recording::apps::{ROWS_TAKEN, kept_measure, kept_members};
use collector::ring::{IdentityUnavailable, RunIdentity};
use persistence::app_tables::{
    AMOUNTS, MEASURE_FIELDS, MEASURES, PARTS_COLUMNS, PARTS_FIXED_COLUMNS, PARTS_ROWS_MAX,
    PARTS_VERSION, ROW_APP, ROW_HEADER, ROW_PROCESS, amount_column_names, measure_column_names,
};
use persistence::engine::{Cells, Row};
use persistence::schema::{APP_PARTS, identity_availability};
use persistence::{Error, Result};

const MEASURE_FIRST: usize = PARTS_FIXED_COLUMNS.len();
const AMOUNT_FIRST: usize = MEASURE_FIRST + MEASURES.len() * MEASURE_FIELDS.len();

/// Every column of the table in its order.
fn column_names() -> Vec<String> {
    let mut names = Vec::with_capacity(PARTS_COLUMNS);
    names.extend(PARTS_FIXED_COLUMNS.map(String::from));
    for measure in MEASURES {
        names.extend(measure_column_names(measure));
    }
    for amount in AMOUNTS {
        names.extend(amount_column_names(amount));
    }

    names
}

/// The place of the fixed column `name`, which fails the build where no fixed column has it.
const fn fixed(name: &str) -> usize {
    column(&PARTS_FIXED_COLUMNS, name)
}

/// The place of the column `name` of a read of the Apps' CPU, energy and disk, which fails the
/// build where the read takes no column of that name.
const fn attribution_column(name: &str) -> usize {
    column(&ATTRIBUTION_COLUMNS, name)
}

/// The place of the column `name` of a read of the Apps' resident memory, which fails the build
/// where the read takes no column of that name.
const fn resident_column(name: &str) -> usize {
    column(&RESIDENT_COLUMNS, name)
}

/// The place of the column `name` among `columns`, which fails the build where none has it.
const fn column(columns: &[&str], name: &str) -> usize {
    let mut index = 0;
    while index < columns.len() {
        if same(columns[index], name) {
            return index;
        }
        index += 1;
    }

    panic!("no column has this name");
}

const fn same(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }

    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }

    true
}

/// What a read keeps of the parts it decodes. A read of the selected App's histories visits
/// the parts of its window five times, and an App of about 250 processes at every second of a
/// full window takes this.
const READ_BYTES_MAX: usize = 64 * 1024 * 1024;

/// The parts of one App that a read decoded from each window, so its later visits of the window
/// take them from memory. The read holds them only while it runs, since what it builds from
/// them keeps what the page shows.
pub(crate) struct PartsRead {
    kind: KeyKind,
    app_bytes: Box<[u8]>,
    windows: Vec<(PathBuf, Rc<Vec<AppPart>>)>,
    windows_bytes: usize,
    windows_bytes_max: usize,
}

impl PartsRead {
    fn new(kind: KeyKind, app_bytes: &[u8], windows_bytes_max: usize) -> Self {
        Self {
            kind,
            app_bytes: app_bytes.into(),
            windows: Vec::new(),
            windows_bytes: 0,
            windows_bytes_max,
        }
    }

    /// The parts the read decoded from the table at `path`, where they are the App's of `key`.
    fn parts(&self, key: &PartKey<'_>, path: &Path) -> Option<Rc<Vec<AppPart>>> {
        if !self.is(key) {
            return None;
        }

        self.windows
            .iter()
            .find(|(kept, _parts)| kept == path)
            .map(|(_kept, parts)| Rc::clone(parts))
    }

    /// Keeps the parts decoded from the table at `path`, where they are the App's of `key` and
    /// fit the read's bound. Parts past the bound are decoded again at each visit.
    fn keep(&mut self, key: &PartKey<'_>, path: &Path, parts: &Rc<Vec<AppPart>>) {
        if !self.is(key) {
            return;
        }

        let parts_bytes: usize = parts.iter().map(AppPart::bytes).sum();
        if parts_bytes > self.windows_bytes_max - self.windows_bytes {
            return;
        }

        assert!(self.windows.len() < WINDOWS_MAX);
        self.windows_bytes += parts_bytes;
        self.windows.push((path.to_owned(), Rc::clone(parts)));
    }

    fn is(&self, key: &PartKey<'_>) -> bool {
        if self.kind != key.kind {
            return false;
        }

        *self.app_bytes == *key.bytes
    }
}

impl Session {
    /// Begins a read of the App with this key, whose visits of a window's parts decode them
    /// once, rather than once a visit.
    pub(crate) fn parts_read_begin(&self, kind: KeyKind, app_bytes: &[u8]) {
        self.parts_read_begin_bounded(kind, app_bytes, READ_BYTES_MAX);
    }

    /// Begins a read that keeps at most `windows_bytes_max` of the parts it decodes.
    fn parts_read_begin_bounded(&self, kind: KeyKind, app_bytes: &[u8], windows_bytes_max: usize) {
        let mut read = self.parts_read.borrow_mut();
        assert!(read.is_none());
        *read = Some(PartsRead::new(kind, app_bytes, windows_bytes_max));
    }

    /// Ends the read, and drops the parts it decoded.
    pub(crate) fn parts_read_end(&self) {
        *self.parts_read.borrow_mut() = None;
    }

    /// Gives `consume` the parts of the App of this key that the windows of `range` keep of
    /// their observations in it, from the table the persister derived as each window sealed. A
    /// window sealed without the table, or with a version of it this build does not read, gives
    /// none.
    pub(crate) fn visit_sealed_parts(
        &self,
        range: ContextRange,
        kind: KeyKind,
        bytes: &[u8],
        mut consume: impl FnMut(&AppPart) -> Result<()>,
    ) -> Result<()> {
        self.visit_sealed_parts_by_window(range, kind, bytes, |_window, part| consume(part))
    }

    /// Gives `consume` each part `visit_sealed_parts` gives, with the index of the window that
    /// keeps it.
    pub(super) fn visit_sealed_parts_by_window(
        &self,
        range: ContextRange,
        kind: KeyKind,
        bytes: &[u8],
        mut consume: impl FnMut(usize, &AppPart) -> Result<()>,
    ) -> Result<()> {
        let key = PartKey { kind, bytes };
        self.visit_derived(
            range,
            APP_PARTS,
            |kept| self.visit_sealed_parts_window(kept, &key),
            |window, parts| {
                for part in parts.iter() {
                    if observed_in(range, part.selected()) {
                        consume(window, part)?;
                    }
                }
                Ok(())
            },
        )
    }

    /// The parts of the window that the read decoded already, or else those its table gives.
    fn visit_sealed_parts_window(
        &self,
        kept: &Kept<'_>,
        key: &PartKey<'_>,
    ) -> Result<Rc<Vec<AppPart>>> {
        if let Some(read) = &*self.parts_read.borrow() {
            if let Some(parts) = read.parts(key, kept.path) {
                return Ok(parts);
            }
        }

        let parts = Rc::new(self.visit_sealed_parts_read(kept, key)?);
        if let Some(read) = &mut *self.parts_read.borrow_mut() {
            read.keep(key, kept.path, &parts);
        }

        Ok(parts)
    }

    fn visit_sealed_parts_read(&self, kept: &Kept<'_>, key: &PartKey<'_>) -> Result<Vec<AppPart>> {
        #[cfg(test)]
        self.parts_decodes.set(self.parts_decodes.get() + 1);
        let source = self.connection.parquet_source(kept.path)?;
        if !self.kept_parts_readable(kept, &source)? {
            return Ok(Vec::new());
        }

        let frame = self.connection.collect(sealed_parts_plan(
            self.connection.plan(&format!(
                "SELECT {} FROM {source}",
                column_names().join(", ")
            ))?,
            key,
        ))?;
        if frame.height() > collector::usize_from_u32(PARTS_ROWS_MAX) {
            return Err(Error::Bound("sealed parts rows"));
        }
        let mut decoder = Decoder {
            kept,
            key,
            output: Vec::with_capacity(TICKS_PER_WINDOW_MAX),
            observation: None,
        };
        for index in 0..frame.height() {
            self.operation.check()?;
            decoder.push(&Row::new(&frame, index)?)?;
        }
        decoder.close()?;

        Ok(decoder.output)
    }
}

impl Session {
    /// Whether this build reads the parts table of `kept`, which the engine holds as `source`,
    /// once the table has the rows its seal counted.
    fn kept_parts_readable(&self, kept: &Kept<'_>, source: &str) -> Result<bool> {
        let summary = self.connection.query(&format!(
            "SELECT MIN(version), MAX(version), COUNT(*) FROM {source}"
        ))?;
        let summary = Row::new(&summary, 0)?;
        let rows: u64 = summary.get(2)?;
        if rows != kept.rows {
            return Err(Error::Bound("sealed parts rows"));
        }
        if rows == 0 {
            return Err(Error::Bound("sealed parts rows"));
        }
        if rows > u64::from(PARTS_ROWS_MAX) {
            return Err(Error::Bound("sealed parts rows"));
        }
        let version: u8 = summary.get(0)?;
        if summary.get::<u8>(1)? != version {
            return Err(Error::Bound("sealed parts version"));
        }

        Ok(version == PARTS_VERSION)
    }

    /// The facts of each observation the parts of `kept`, which the engine holds as `source`,
    /// keep, in publication order, or none where this build does not read them.
    pub(super) fn kept_parts_headers(
        &self,
        kept: &Kept<'_>,
        source: &str,
    ) -> Result<Option<Vec<Header>>> {
        if !self.kept_parts_readable(kept, source)? {
            return Ok(None);
        }

        #[cfg(test)]
        self.kept_header_queries
            .set(self.kept_header_queries.get() + 1);
        let frame = self.connection.collect(kept_parts_sorted(
            self.connection
                .plan(&kept_parts_query(source))?
                .filter(col("row_kind").eq(lit(ROW_HEADER))),
            &["publication"],
        ))?;
        if frame.height() > TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("sealed parts observations"));
        }
        let mut headers: Vec<Header> = Vec::with_capacity(frame.height());
        for index in 0..frame.height() {
            self.operation.check()?;
            let header = kept_header(&Row::new(&frame, index)?, kept.run)?;
            kept_publication_follows(
                kept,
                headers.last().map(|last| last.selected.stamp.publication),
                header.selected.stamp.publication,
            )?;
            headers.push(header);
        }

        Ok(Some(headers))
    }

    /// Gives `consume` each of `headers`, in publication order, with the CPU, energy and disk
    /// of each of its Apps, from the App rows of the parts the engine holds as `source`.
    pub(super) fn visit_kept_parts_apps(
        &self,
        source: &str,
        headers: &[&Header],
        consume: impl FnMut(&Header, &[KeptAttribution<'_>]) -> Result<()>,
    ) -> Result<()> {
        let Some(publications) = kept_parts_publications(headers) else {
            return Ok(());
        };

        let frame = self.kept_parts_app_rows(source, &ATTRIBUTION_COLUMNS, publications)?;
        let columns = KeptAttributionColumns::new(&frame)?;
        self.visit_kept_parts_rows(&columns, headers, consume)
    }

    /// Gives `consume` each of `headers`, in publication order, with the resident memory of
    /// each of its Apps, from the App rows of the parts the engine holds as `source`.
    pub(super) fn visit_kept_parts_residents(
        &self,
        source: &str,
        headers: &[&Header],
        consume: impl FnMut(&Header, &[KeptResident<'_>]) -> Result<()>,
    ) -> Result<()> {
        let Some(publications) = kept_parts_publications(headers) else {
            return Ok(());
        };

        let frame = self.kept_parts_app_rows(source, &RESIDENT_COLUMNS, publications)?;
        let columns = KeptResidentColumns::new(&frame)?;
        self.visit_kept_parts_rows(&columns, headers, consume)
    }

    /// The `columns` of the App rows of the observations of `publications`, in publication and
    /// App order, from the parts the engine holds as `source`.
    fn kept_parts_app_rows(
        &self,
        source: &str,
        columns: &[&str],
        publications: RangeInclusive<u64>,
    ) -> Result<DataFrame> {
        // The engine filters a range of publications far faster than a list of them, so the
        // read takes the range and skips the rows of observations in it that a read leaves out.
        assert!(publications.start() <= publications.end());
        let query = format!("SELECT {} FROM {source}", columns.join(", "));
        let frame = self.connection.collect(kept_parts_sorted(
            self.connection.plan(&query)?.filter(
                col("row_kind")
                    .eq(lit(ROW_APP))
                    .and(col("publication").gt_eq(lit(*publications.start())))
                    .and(col("publication").lt_eq(lit(*publications.end()))),
            ),
            &["publication", "app_index"],
        ))?;
        if frame.height() > collector::usize_from_u32(PARTS_ROWS_MAX) {
            return Err(Error::Bound("sealed parts rows"));
        }

        Ok(frame)
    }

    /// Gives `consume` each of `headers`, in publication order, with what `columns` give of
    /// each of its Apps.
    fn visit_kept_parts_rows<C: KeptColumns>(
        &self,
        columns: &C,
        headers: &[&Header],
        mut consume: impl FnMut(&Header, &[C::App]) -> Result<()>,
    ) -> Result<()> {
        let mut apps = Vec::with_capacity(crate::live_app_response::APPS_MAX);
        let mut row = 0;
        for header in headers {
            self.operation.check()?;
            row = visit_kept_parts_rows_read(&self.operation, columns, row, header, &mut apps)?;
            consume(header, &apps)?;
        }
        if row != columns.app_columns().row_count {
            return Err(Error::Bound("sealed parts publication"));
        }

        Ok(())
    }
}

/// The first and the last publication of `headers`, which a read of their App rows takes, or
/// none for no headers.
fn kept_parts_publications(headers: &[&Header]) -> Option<RangeInclusive<u64>> {
    let (Some(first), Some(last)) = (headers.first(), headers.last()) else {
        return None;
    };

    Some(first.selected.stamp.publication..=last.selected.stamp.publication)
}

/// Gives `output` what `columns` give of the Apps of the observation of `header`, whose rows
/// start at or after row `first`, and returns the row after them.
fn visit_kept_parts_rows_read<C: KeptColumns>(
    operation: &Operation,
    columns: &C,
    first: usize,
    header: &Header,
    output: &mut Vec<C::App>,
) -> Result<usize> {
    let app_columns = columns.app_columns();
    let start = visit_kept_parts_rows_read_start(operation, app_columns, first, header)?;
    output.clear();
    for row in start..app_columns.row_count {
        operation.check()?;
        if app_columns.publication.get(row)? != header.selected.stamp.publication {
            break;
        }
        // A row is read whole before its place is checked, so a row wrong in both ways
        // reports its cell.
        let app = columns.app(row)?;
        if app_columns.app_index(row)? != output.len() {
            return Err(Error::Bound("sealed parts App"));
        }
        if output.len() == header.app_count {
            return Err(Error::Bound("sealed parts Apps"));
        }
        output.push(app);
    }
    if output.len() != header.app_count {
        return Err(Error::Bound("sealed parts Apps"));
    }

    Ok(start + output.len())
}

/// The first row of `app_columns` from row `first` on whose observation is not before
/// `header`'s, which skips the rows of the observations the read leaves out.
fn visit_kept_parts_rows_read_start(
    operation: &Operation,
    app_columns: &KeptAppColumns<'_>,
    first: usize,
    header: &Header,
) -> Result<usize> {
    for row in first..app_columns.row_count {
        operation.check()?;
        if app_columns.publication.get(row)? >= header.selected.stamp.publication {
            return Ok(row);
        }
    }

    Ok(app_columns.row_count)
}

/// The columns of a frame of App rows a read takes, and what the read gives of each App.
trait KeptColumns {
    /// What the read gives of an App.
    type App;

    /// The columns every read of App rows takes.
    fn app_columns(&self) -> &KeptAppColumns<'_>;

    /// What the read gives of the App of row `row`.
    fn app(&self, row: usize) -> Result<Self::App>;
}

/// The columns every read of App rows takes, the first of each read's columns. Each is read in
/// the type the parts table holds it in, since a read of many rows converting each cell through
/// `Row` spends most of its time converting.
struct KeptAppColumns<'frame> {
    row_count: usize,
    publication: Cells<'frame, UInt64Type>,
    app_index: Cells<'frame, UInt32Type>,
    app_kind: Cells<'frame, UInt8Type>,
    app_key: Cells<'frame, BinaryType>,
    members: Cells<'frame, UInt32Type>,
}

/// The columns `KeptAppColumns` reads, in their places among the columns of every read.
const APP_COLUMNS: [&str; 5] = ["publication", "app_index", "app_kind", "app_key", "members"];
const _: () = {
    let mut index = 0;
    while index < APP_COLUMNS.len() {
        assert!(same(ATTRIBUTION_COLUMNS[index], APP_COLUMNS[index]));
        assert!(same(RESIDENT_COLUMNS[index], APP_COLUMNS[index]));
        index += 1;
    }
};

impl<'frame> KeptAppColumns<'frame> {
    /// The columns of `frame`, whose columns open with `APP_COLUMNS`.
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            row_count: frame.height(),
            publication: Cells::new(frame, const { column(&APP_COLUMNS, "publication") })?,
            app_index: Cells::new(frame, const { column(&APP_COLUMNS, "app_index") })?,
            app_kind: Cells::new(frame, const { column(&APP_COLUMNS, "app_kind") })?,
            app_key: Cells::new(frame, const { column(&APP_COLUMNS, "app_key") })?,
            members: Cells::new(frame, const { column(&APP_COLUMNS, "members") })?,
        })
    }

    /// The place of row `row`'s App among its sample's Apps.
    fn app_index(&self, row: usize) -> Result<usize> {
        Ok(collector::usize_from_u32(self.app_index.get(row)?))
    }

    /// The key of row `row`'s App, in the frame's bytes, which the reads of every row of a
    /// window share rather than copying each.
    fn key(&self, row: usize) -> Result<AppKey<'frame>> {
        let kind = kind_of(self.app_kind.get(row)?)?;
        let bytes = self.app_key.get(row)?;

        kind.checked_key(bytes)
    }

    /// The members of row `row`'s App.
    fn members(&self, row: usize) -> Result<u32> {
        kept_members(self.members.get(row)?)
    }
}

/// The columns of one measure of App rows.
struct KeptMeasureColumns<'frame> {
    known: Cells<'frame, UInt64Type>,
    measured_members: Cells<'frame, UInt32Type>,
    unavailable_members: Cells<'frame, UInt32Type>,
    overflowed: Cells<'frame, BooleanType>,
}

impl<'frame> KeptMeasureColumns<'frame> {
    /// The columns of the measure whose fields `frame` holds from column `first`.
    fn new(frame: &'frame DataFrame, first: usize) -> Result<Self> {
        Ok(Self {
            known: Cells::new(frame, first)?,
            measured_members: Cells::new(frame, first + 1)?,
            unavailable_members: Cells::new(frame, first + 2)?,
            overflowed: Cells::new(frame, first + 3)?,
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

/// The columns of `ATTRIBUTION_COLUMNS`.
struct KeptAttributionColumns<'frame> {
    app_columns: KeptAppColumns<'frame>,
    membership_complete: Cells<'frame, BooleanType>,
    cpu_ns: KeptMeasureColumns<'frame>,
    energy_nj: KeptMeasureColumns<'frame>,
    disk_io_bytes: KeptMeasureColumns<'frame>,
    gpu_ns: KeptMeasureColumns<'frame>,
}

impl<'frame> KeptAttributionColumns<'frame> {
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        let measure = |first: usize| KeptMeasureColumns::new(frame, first);

        Ok(Self {
            app_columns: KeptAppColumns::new(frame)?,
            membership_complete: Cells::new(
                frame,
                const { attribution_column("app_membership_complete") },
            )?,
            cpu_ns: measure(const { attribution_column("app_cpu_ns_known") })?,
            energy_nj: measure(const { attribution_column("app_energy_nj_known") })?,
            disk_io_bytes: measure(const { attribution_column("app_disk_io_bytes_known") })?,
            gpu_ns: measure(const { attribution_column("app_gpu_ns_known") })?,
        })
    }
}

impl<'frame> KeptColumns for KeptAttributionColumns<'frame> {
    type App = KeptAttribution<'frame>;

    fn app_columns(&self) -> &KeptAppColumns<'_> {
        &self.app_columns
    }

    fn app(&self, row: usize) -> Result<KeptAttribution<'frame>> {
        // The key is read before the members, as `kept_app` reads them, so a row wrong in both
        // reports the same cell whichever read takes it.
        let key = self.app_columns.key(row)?;
        let members = self.app_columns.members(row)?;

        Ok(KeptAttribution {
            key,
            members,
            membership_complete: self.membership_complete.get(row)?,
            cpu_ns: self.cpu_ns.measure(row, members)?,
            energy_nj: self.energy_nj.measure(row, members)?,
            disk_io_bytes: self.disk_io_bytes.measure(row, members)?,
            gpu_ns: self.gpu_ns.measure(row, members)?,
        })
    }
}

/// The columns of `RESIDENT_COLUMNS`.
struct KeptResidentColumns<'frame> {
    app_columns: KeptAppColumns<'frame>,
    resident: KeptMeasureColumns<'frame>,
}

impl<'frame> KeptResidentColumns<'frame> {
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            app_columns: KeptAppColumns::new(frame)?,
            resident: KeptMeasureColumns::new(
                frame,
                const { resident_column("app_resident_bytes_known") },
            )?,
        })
    }
}

impl<'frame> KeptColumns for KeptResidentColumns<'frame> {
    type App = KeptResident<'frame>;

    fn app_columns(&self) -> &KeptAppColumns<'_> {
        &self.app_columns
    }

    fn app(&self, row: usize) -> Result<KeptResident<'frame>> {
        let key = self.app_columns.key(row)?;
        let members = self.app_columns.members(row)?;

        Ok(KeptResident {
            key,
            resident: self.resident.measure(row, members)?,
        })
    }
}

/// An App's CPU, energy, disk and GPU at an observation of a window that keeps its parts, which
/// the histories of every App plot.
pub(crate) struct KeptAttribution<'frame> {
    pub key: AppKey<'frame>,
    pub members: u32,
    pub membership_complete: bool,
    pub cpu_ns: Measure,
    pub energy_nj: Measure,
    pub disk_io_bytes: Measure,
    pub gpu_ns: Measure,
}

/// The columns a read of the Apps' CPU, energy, disk and GPU takes: those it decodes, then the
/// row's kind its filter reads. The read takes no more, since each column costs the engine its
/// plan and decode on every window.
const ATTRIBUTION_COLUMNS: [&str; 23] = [
    "publication",
    "app_index",
    "app_kind",
    "app_key",
    "members",
    "app_membership_complete",
    "app_cpu_ns_known",
    "app_cpu_ns_measured_members",
    "app_cpu_ns_unavailable_members",
    "app_cpu_ns_overflowed",
    "app_energy_nj_known",
    "app_energy_nj_measured_members",
    "app_energy_nj_unavailable_members",
    "app_energy_nj_overflowed",
    "app_disk_io_bytes_known",
    "app_disk_io_bytes_measured_members",
    "app_disk_io_bytes_unavailable_members",
    "app_disk_io_bytes_overflowed",
    "app_gpu_ns_known",
    "app_gpu_ns_measured_members",
    "app_gpu_ns_unavailable_members",
    "app_gpu_ns_overflowed",
    "row_kind",
];
const _: () = {
    // Each measure's fields follow its known amount in the order a measure keeps them.
    let cpu = attribution_column("app_cpu_ns_known");
    assert!(attribution_column("app_cpu_ns_measured_members") == cpu + 1);
    assert!(attribution_column("app_cpu_ns_unavailable_members") == cpu + 2);
    assert!(attribution_column("app_cpu_ns_overflowed") == cpu + 3);
    let energy = attribution_column("app_energy_nj_known");
    assert!(attribution_column("app_energy_nj_measured_members") == energy + 1);
    assert!(attribution_column("app_energy_nj_unavailable_members") == energy + 2);
    assert!(attribution_column("app_energy_nj_overflowed") == energy + 3);
    let disk = attribution_column("app_disk_io_bytes_known");
    assert!(attribution_column("app_disk_io_bytes_measured_members") == disk + 1);
    assert!(attribution_column("app_disk_io_bytes_unavailable_members") == disk + 2);
    assert!(attribution_column("app_disk_io_bytes_overflowed") == disk + 3);
    let gpu = attribution_column("app_gpu_ns_known");
    assert!(attribution_column("app_gpu_ns_measured_members") == gpu + 1);
    assert!(attribution_column("app_gpu_ns_unavailable_members") == gpu + 2);
    assert!(attribution_column("app_gpu_ns_overflowed") == gpu + 3);
    // Each column the read takes is one the parts table holds, under the name it gives it.
    assert!(fixed("app_membership_complete") < PARTS_FIXED_COLUMNS.len());
    assert!(column(&MEASURES, "cpu_ns") < MEASURES.len());
    assert!(column(&MEASURES, "energy_nj") < MEASURES.len());
    assert!(column(&MEASURES, "disk_io_bytes") < MEASURES.len());
    assert!(column(&MEASURES, "gpu_ns") < MEASURES.len());
};

/// An App's resident memory at an observation of a window that keeps its parts.
pub(crate) struct KeptResident<'frame> {
    pub key: AppKey<'frame>,
    pub resident: Measure,
}

/// The columns a read of the Apps' resident memory takes: those it decodes, then the row's
/// kind its filter reads.
const RESIDENT_COLUMNS: [&str; 10] = [
    "publication",
    "app_index",
    "app_kind",
    "app_key",
    "members",
    "app_resident_bytes_known",
    "app_resident_bytes_measured_members",
    "app_resident_bytes_unavailable_members",
    "app_resident_bytes_overflowed",
    "row_kind",
];
const _: () = {
    let known = resident_column("app_resident_bytes_known");
    assert!(resident_column("app_resident_bytes_measured_members") == known + 1);
    assert!(resident_column("app_resident_bytes_unavailable_members") == known + 2);
    assert!(resident_column("app_resident_bytes_overflowed") == known + 3);
    // Each column the read takes is one the parts table holds, under the name it gives it.
    assert!(fixed("publication") < PARTS_FIXED_COLUMNS.len());
    assert!(fixed("app_index") < PARTS_FIXED_COLUMNS.len());
    assert!(fixed("app_kind") < PARTS_FIXED_COLUMNS.len());
    assert!(fixed("app_key") < PARTS_FIXED_COLUMNS.len());
    assert!(fixed("members") < PARTS_FIXED_COLUMNS.len());
    assert!(fixed("row_kind") < PARTS_FIXED_COLUMNS.len());
    assert!(column(&MEASURES, "resident_bytes") < MEASURES.len());
    assert!(same(MEASURE_FIELDS[0], "known"));
    assert!(same(MEASURE_FIELDS[1], "measured_members"));
    assert!(same(MEASURE_FIELDS[2], "unavailable_members"));
    assert!(same(MEASURE_FIELDS[3], "overflowed"));
};

/// Every column of a parts table but the processes' amounts, which an App row and a row of an
/// observation's facts leave empty.
fn kept_parts_query(source: &str) -> String {
    format!(
        "SELECT {} FROM {source}",
        column_names()[..AMOUNT_FIRST].join(", ")
    )
}

/// The rows of `plan` sorted by `columns`, of which a table keeps at most `PARTS_ROWS_MAX`.
fn kept_parts_sorted(plan: LazyFrame, columns: &[&str]) -> LazyFrame {
    plan.sort(
        columns.to_vec(),
        SortMultipleOptions {
            descending: vec![false; columns.len()],
            nulls_last: vec![false; columns.len()],
            multithreaded: false,
            maintain_order: true,
            limit: None,
        },
    )
    .limit(ROWS_TAKEN)
}

/// Refuses an observation of `publication` that `kept` does not count, or that does not
/// follow the observation of `last` read before it.
fn kept_publication_follows(kept: &Kept<'_>, last: Option<u64>, publication: u64) -> Result<()> {
    if !kept.publications.contains(&publication) {
        return Err(Error::Bound("sealed parts publication"));
    }
    if let Some(last) = last {
        if last >= publication {
            return Err(Error::Bound("sealed parts publication"));
        }
    }

    Ok(())
}

/// The App whose parts a reader takes.
struct PartKey<'key> {
    kind: KeyKind,
    bytes: &'key [u8],
}

/// The rows of every observation's facts and of the App of this key, in the order the decoder
/// takes them: by publication, then the facts, the App and its processes in their sample's
/// order.
fn sealed_parts_plan(plan: LazyFrame, key: &PartKey<'_>) -> LazyFrame {
    let app = col("app_kind")
        .eq(lit(kind_code(key.kind)))
        .and(col("app_key").eq(lit(key.bytes)));

    plan.filter(col("row_kind").eq(lit(ROW_HEADER)).or(app))
        .sort(
            ["publication", "row_kind", "process_index"],
            SortMultipleOptions {
                descending: vec![false; 3],
                nulls_last: vec![false; 3],
                multithreaded: false,
                maintain_order: true,
                limit: None,
            },
        )
        .limit(ROWS_TAKEN)
}

/// Rebuilds one App's parts of a window's observations from the rows `sealed_parts_plan` gives.
struct Decoder<'read> {
    kept: &'read Kept<'read>,
    key: &'read PartKey<'read>,
    output: Vec<AppPart>,
    /// The facts of the observation the last row of facts opened, and the App's row and
    /// processes there so far.
    observation: Option<(Header, Option<Present>)>,
}

impl Decoder<'_> {
    fn push(&mut self, row: &Row<'_>) -> Result<()> {
        let kind: u8 = row.get(const { fixed("row_kind") })?;
        if kind == ROW_HEADER {
            self.close()?;
            return self.open(kept_header(row, self.kept.run)?);
        }

        let Some((header, present)) = &mut self.observation else {
            return Err(Error::Bound("sealed parts header"));
        };
        let publication: u64 = row.get(const { fixed("publication") })?;
        if publication != header.selected.stamp.publication {
            return Err(Error::Bound("sealed parts header"));
        }
        if kind == ROW_APP {
            if present.is_some() {
                return Err(Error::Bound("sealed parts App"));
            }
            let (app_index, app) = kept_app(row)?;
            if app.key.kind != self.key.kind {
                return Err(Error::Bound("sealed parts App"));
            }
            if *app.key.bytes != *self.key.bytes {
                return Err(Error::Bound("sealed parts App"));
            }
            *present = Some(Present {
                app_index,
                processes: Vec::with_capacity(collector::usize_from_u32(app.members)),
                app,
            });
        } else {
            if kind != ROW_PROCESS {
                return Err(Error::Bound("sealed parts row kind"));
            }
            let present = present.as_mut().ok_or(Error::Bound("sealed parts App"))?;
            let process = kept_process(row)?;
            let app_index = process
                .app_index
                .ok_or(Error::Bound("sealed parts process App"))?;
            if collector::usize_from_u32(app_index) != present.app_index {
                return Err(Error::Bound("sealed parts process App"));
            }
            if present.processes.len() == collector::usize_from_u32(present.app.members) {
                return Err(Error::Bound("sealed parts members"));
            }
            present.processes.push(process);
        }

        Ok(())
    }

    fn open(&mut self, header: Header) -> Result<()> {
        kept_publication_follows(
            self.kept,
            self.output
                .last()
                .map(|last| last.selected().stamp.publication),
            header.selected.stamp.publication,
        )?;
        if self.output.len() == TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("sealed parts observations"));
        }

        self.observation = Some((header, None));

        Ok(())
    }

    /// Ends the observation open last, whose App must have all its processes.
    fn close(&mut self) -> Result<()> {
        let Some((header, present)) = self.observation.take() else {
            return Ok(());
        };
        if let Some(present) = &present {
            if present.processes.len() != collector::usize_from_u32(present.app.members) {
                return Err(Error::Bound("sealed parts members"));
            }
        }

        self.output.push(AppPart::kept(
            header,
            self.key.kind,
            self.key.bytes.into(),
            present,
        ));

        Ok(())
    }
}

fn kept_header(row: &Row<'_>, run: &RunIdentity) -> Result<Header> {
    let stamp = |publication, continuous_ns, awake_ns| Stamp {
        boot: run.boot.0,
        run: run.run.0,
        publication,
        continuous_ns,
        awake_ns,
    };
    let previous = match row.get::<Option<u64>>(const { fixed("previous_publication") })? {
        Some(publication) => Some(stamp(
            publication,
            row.get(const { fixed("previous_continuous_ns") })?,
            row.get(const { fixed("previous_awake_ns") })?,
        )),
        None => None,
    };

    Ok(Header {
        selected: Observation {
            recording: run.recording.0,
            stamp: stamp(
                row.get(const { fixed("publication") })?,
                row.get(const { fixed("continuous_ns") })?,
                row.get(const { fixed("awake_ns") })?,
            ),
            run_tick: row.get(const { fixed("run_tick") })?,
            wall_ns: row.get(const { fixed("wall_ns") })?,
        },
        previous,
        elapsed_ns: row.get(const { fixed("elapsed_ns") })?,
        membership_complete: row.get(const { fixed("membership_complete") })?,
        ungrouped_members: row.get(const { fixed("ungrouped_members") })?,
        live_ungrouped_members: row.get(const { fixed("live_ungrouped_members") })?,
        usage_refused: row.get(const { fixed("usage_refused") })?,
        usage_gone: row.get(const { fixed("usage_gone") })?,
        app_count: count_of(row.get(const { fixed("app_count") })?)?,
        process_count: count_of(row.get(const { fixed("process_count") })?)?,
    })
}

/// An App row's App and its place among its sample's Apps.
fn kept_app(row: &Row<'_>) -> Result<(usize, App)> {
    let key = Key::new(
        kind_of(row.get(const { fixed("app_kind") })?)?,
        row.get(const { fixed("app_key") })?,
    )?;
    let members = kept_members(row.get(const { fixed("members") })?)?;
    let measure = |place: usize| -> Result<Measure> {
        kept_app_measure(row, MEASURE_FIRST + place * MEASURE_FIELDS.len(), members)
    };

    let app = App {
        display_name: key.display_name()?,
        key,
        members,
        live_members: row.get(const { fixed("live_members") })?,
        membership_complete: row.get(const { fixed("app_membership_complete") })?,
        cpu_ns: measure(const { column(&MEASURES, "cpu_ns") })?,
        energy_nj: measure(const { column(&MEASURES, "energy_nj") })?,
        disk_read_bytes: measure(const { column(&MEASURES, "disk_read_bytes") })?,
        disk_written_bytes: measure(const { column(&MEASURES, "disk_written_bytes") })?,
        disk_io_bytes: measure(const { column(&MEASURES, "disk_io_bytes") })?,
        resident_bytes: measure(const { column(&MEASURES, "resident_bytes") })?,
        footprint_bytes: measure(const { column(&MEASURES, "footprint_bytes") })?,
        gpu_ns: measure(const { column(&MEASURES, "gpu_ns") })?,
    };

    Ok((
        collector::usize_from_u32(row.get(const { fixed("app_index") })?),
        app,
    ))
}

/// The measure of an App of `members` members whose fields a row holds from column `first`.
fn kept_app_measure(row: &Row<'_>, first: usize, members: u32) -> Result<Measure> {
    let measure = Measure {
        known: row.get(first)?,
        measured_members: row.get(first + 1)?,
        unavailable_members: row.get(first + 2)?,
        overflowed: row.get(first + 3)?,
    };

    kept_measure(measure, members)
}

fn kept_process(row: &Row<'_>) -> Result<Process> {
    let amount = |place: usize| -> Result<Amount> {
        let first = AMOUNT_FIRST + place * 2;
        match (row.get(first)?, row.get(first + 1)?) {
            (Some(value), None) => Ok(Amount::Known(value)),
            (None, Some(code)) => Ok(Amount::Unavailable(unavailable_of(code)?)),
            _ => Err(Error::Bound("sealed parts amount")),
        }
    };
    let creation = match (
        row.get(const { fixed("unique_id") })?,
        row.get(const { fixed("pid_version") })?,
    ) {
        (Some(unique_id), Some(pid_version)) => Some(ProcessCreation {
            unique_id,
            pid_version,
        }),
        (None, None) => None,
        _ => return Err(Error::Bound("sealed parts creation")),
    };
    let executable_name: Option<Vec<u8>> = row.get(const { fixed("executable_name") })?;
    let program_unavailable: Option<u8> = row.get(const { fixed("program_unavailable") })?;
    let app_attribution: Option<u8> = row.get(const { fixed("app_attribution") })?;

    Ok(Process {
        pid: row.get(const { fixed("pid") })?,
        creation,
        app_index: Some(row.get(const { fixed("app_index") })?),
        executable_name: executable_name.map(Bytes::new).transpose()?,
        program_unavailable: program_unavailable.map(identity_of).transpose()?,
        cpu_ns: amount(const { column(&AMOUNTS, "cpu_ns") })?,
        energy_nj: amount(const { column(&AMOUNTS, "energy_nj") })?,
        gpu_ns: amount(const { column(&AMOUNTS, "gpu_ns") })?,
        disk_read_bytes: amount(const { column(&AMOUNTS, "disk_read_bytes") })?,
        disk_written_bytes: amount(const { column(&AMOUNTS, "disk_written_bytes") })?,
        disk_io_bytes: amount(const { column(&AMOUNTS, "disk_io_bytes") })?,
        app_attribution: app_attribution.map(unavailable_of).transpose()?,
        resident_bytes: amount(const { column(&AMOUNTS, "resident_bytes") })?,
        footprint_bytes: amount(const { column(&AMOUNTS, "footprint_bytes") })?,
        idle_wakeups: amount(const { column(&AMOUNTS, "idle_wakeups") })?,
        pageins: amount(const { column(&AMOUNTS, "pageins") })?,
        threads: amount(const { column(&AMOUNTS, "threads") })?,
        state: state_of(row.get(const { fixed("state") })?)?,
    })
}

fn count_of(value: u64) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Bound("sealed parts count"))
}

fn unavailable_of(code: u8) -> Result<Unavailable> {
    match code {
        0 => Ok(Unavailable::Refused),
        1 => Ok(Unavailable::Gone),
        2 => Ok(Unavailable::Unknown),
        3 => Ok(Unavailable::Interval),
        4 => Ok(Unavailable::Creation),
        5 => Ok(Unavailable::Baseline),
        6 => Ok(Unavailable::CounterReset),
        7 => Ok(Unavailable::Overflow),
        8 => Ok(Unavailable::Exec),
        9 => Ok(Unavailable::Program),
        10 => Ok(Unavailable::ExitUnverified),
        _ => Err(Error::Bound("sealed parts unavailable")),
    }
}

fn identity_of(code: u8) -> Result<IdentityUnavailable> {
    match code {
        identity_availability::REFUSED => Ok(IdentityUnavailable::Refused),
        identity_availability::GONE => Ok(IdentityUnavailable::Gone),
        identity_availability::UNSUPPORTED => Ok(IdentityUnavailable::Unsupported),
        identity_availability::CHANGED => Ok(IdentityUnavailable::Changed),
        identity_availability::INVALID => Ok(IdentityUnavailable::Invalid),
        identity_availability::EXHAUSTED => Ok(IdentityUnavailable::Exhausted),
        _ => Err(Error::Bound("sealed parts program")),
    }
}

fn state_of(code: u8) -> Result<ProcessState> {
    match code {
        0 => Ok(ProcessState::Live),
        1 => Ok(ProcessState::ExitedVerified),
        2 => Ok(ProcessState::ExitedUnverified),
        _ => Err(Error::Bound("sealed parts state")),
    }
}

#[cfg(test)]
#[path = "sealed_parts_tests.rs"]
mod tests;
