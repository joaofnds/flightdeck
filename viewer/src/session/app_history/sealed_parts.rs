//! Every App's part of each observation of a sealed window: its row, its processes, and the
//! facts of the observation's sample every page shows. The persister derives them as the window
//! seals, so the viewer shows a saved second's App without replaying its minute.
use std::path::Path;

use polars::prelude::{Column, DataFrame, LazyFrame, SortMultipleOptions, col, lit};

use super::sealed::{Kept, columns, kind_code, kind_of, observed_in};
use super::{ContextRange, Session, TICKS_PER_WINDOW_MAX};
use crate::apps::{Amount, ProcessCreation, ProcessState, Stamp, Unavailable};
use crate::live_app_response::{
    App, AppPart, Bytes, Header, Key, KeyKind, MEMBERS_MAX, Measure, Observation, Present, Process,
    Sample,
};
use collector::ring::{IdentityUnavailable, RunIdentity};
use persistence::engine::Row;
use persistence::schema::{APP_PARTS, identity_availability};
use persistence::{Error, Result, TableManifest};

/// The version of the table's columns. A reader takes no parts from a table of another version
/// and leaves its window to the replay, so a later build may change the columns and an older
/// one still reads the recording.
pub(super) const VERSION: u8 = 1;
/// The rows a window keeps. About 1,100 processes in a few hundred Apps a second make some
/// 85,000 rows a window, a sixth of this.
const ROWS_MAX: usize = 1 << 19;
/// What the engine may decode of a window's table, below what it admits of a file. A row
/// decodes to at most 8 bytes a column, 16 more for each of its two columns of bytes and what
/// parquet adds to each, and the bytes of its App's key and its executable's name, so long
/// keys may refuse a window before its rows reach their bound.
const DECODED_BYTES_MAX: u64 = 384 * 1024 * 1024;
const ROW_DECODED_BYTES: usize = COLUMNS * 8 + 2 * 24;

/// A row holds the facts of an observation's sample, one of its Apps, or one of an App's
/// processes.
const ROW_HEADER: u8 = 0;
const ROW_APP: u8 = 1;
const ROW_PROCESS: u8 = 2;

/// The columns before the measures and amounts, in their order: those of every row, those only
/// a row of an observation's facts fills, those of an App and of a process row, those only an
/// App row fills, and those only a process row fills.
const FIXED_COLUMNS: [&str; 31] = [
    "version",
    "publication",
    "row_kind",
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
    "usage_refused",
    "usage_gone",
    "app_count",
    "process_count",
    "app_index",
    "app_kind",
    "app_key",
    "members",
    "live_members",
    "app_membership_complete",
    "process_index",
    "pid",
    "unique_id",
    "pid_version",
    "executable_name",
    "program_unavailable",
    "app_attribution",
    "state",
];
/// An App row's measures, each in a column for every field, after the fixed columns.
const MEASURES: [MeasureColumn; 7] = [
    ("cpu_ns", |app| app.cpu_ns),
    ("energy_nj", |app| app.energy_nj),
    ("disk_read_bytes", |app| app.disk_read_bytes),
    ("disk_written_bytes", |app| app.disk_written_bytes),
    ("disk_io_bytes", |app| app.disk_io_bytes),
    ("resident_bytes", |app| app.resident_bytes),
    ("footprint_bytes", |app| app.footprint_bytes),
];
const MEASURE_FIELDS: [&str; 4] = [
    "known",
    "measured_members",
    "unavailable_members",
    "overflowed",
];
/// A process row's amounts, each in a column of its value and one of why it has none, after
/// the measures.
const AMOUNTS: [AmountColumn; 10] = [
    ("cpu_ns", |process| process.cpu_ns),
    ("energy_nj", |process| process.energy_nj),
    ("disk_read_bytes", |process| process.disk_read_bytes),
    ("disk_written_bytes", |process| process.disk_written_bytes),
    ("disk_io_bytes", |process| process.disk_io_bytes),
    ("resident_bytes", |process| process.resident_bytes),
    ("footprint_bytes", |process| process.footprint_bytes),
    ("idle_wakeups", |process| process.idle_wakeups),
    ("pageins", |process| process.pageins),
    ("threads", |process| process.threads),
];
/// A measure's name, and where an App row takes it from.
type MeasureColumn = (&'static str, fn(&App) -> Measure);
/// An amount's name, and where a process row takes it from.
type AmountColumn = (&'static str, fn(&Process) -> Amount);
const MEASURE_FIRST: usize = FIXED_COLUMNS.len();
const AMOUNT_FIRST: usize = MEASURE_FIRST + MEASURES.len() * MEASURE_FIELDS.len();
const COLUMNS: usize = AMOUNT_FIRST + AMOUNTS.len() * 2;

const _: () = {
    assert!(COLUMNS <= persistence::engine::PARQUET_COLUMNS_MAX);
    assert!(DECODED_BYTES_MAX < persistence::engine::PARQUET_EXPANDED_BYTES_MAX);
};

/// Writes every App's part of a window's observations into its pending directory, in the table
/// the window seals with.
pub(super) fn encode(samples: &[Box<Sample>], pending: &Path) -> Result<TableManifest> {
    let mut frame = sealed_parts_frame(samples)?;

    persistence::encode_derived(APP_PARTS, &mut frame, pending)
}

/// One row of the table.
enum PartRow<'sample> {
    Header {
        sample: &'sample Sample,
    },
    App {
        sample: &'sample Sample,
        index: u32,
        app: &'sample App,
    },
    Process {
        sample: &'sample Sample,
        app_index: u32,
        app: &'sample App,
        index: u32,
        process: &'sample Process,
    },
}

impl<'sample> PartRow<'sample> {
    fn sample(&self) -> &'sample Sample {
        match self {
            Self::Header { sample } | Self::App { sample, .. } | Self::Process { sample, .. } => {
                sample
            }
        }
    }

    fn kind(&self) -> u8 {
        match self {
            Self::Header { .. } => ROW_HEADER,
            Self::App { .. } => ROW_APP,
            Self::Process { .. } => ROW_PROCESS,
        }
    }

    fn header(&self) -> Option<&'sample Sample> {
        match self {
            Self::Header { sample } => Some(sample),
            Self::App { .. } | Self::Process { .. } => None,
        }
    }

    /// The App of an App row or a process row, and its place among its sample's Apps.
    fn app(&self) -> Option<(u32, &'sample App)> {
        match self {
            Self::Header { .. } => None,
            Self::App { index, app, .. } => Some((*index, app)),
            Self::Process { app_index, app, .. } => Some((*app_index, app)),
        }
    }

    fn app_row(&self) -> Option<&'sample App> {
        match self {
            Self::App { app, .. } => Some(app),
            Self::Header { .. } | Self::Process { .. } => None,
        }
    }

    /// The process of a process row, and its place among its sample's processes.
    fn process(&self) -> Option<(u32, &'sample Process)> {
        match self {
            Self::Process { index, process, .. } => Some((*index, process)),
            Self::Header { .. } | Self::App { .. } => None,
        }
    }

    fn process_row(&self) -> Option<&'sample Process> {
        self.process().map(|(_index, process)| process)
    }
}

fn sealed_parts_frame(samples: &[Box<Sample>]) -> Result<DataFrame> {
    let rows = sealed_parts_rows(samples)?;
    if sealed_parts_decoded_bytes(&rows) > DECODED_BYTES_MAX {
        return Err(Error::Bound("sealed parts bytes"));
    }

    let mut columns = sealed_parts_header_columns(&rows);
    columns.extend(sealed_parts_member_columns(&rows));
    columns.extend(sealed_parts_measure_columns(&rows));
    columns.extend(sealed_parts_amount_columns(&rows));
    let names = column_names();
    assert!(
        columns
            .iter()
            .map(|column| column.name().as_str())
            .eq(names.iter().map(String::as_str))
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

/// A row for each observation's facts, one for each of its Apps, and one for each process of
/// an App. A process outside every App belongs to no App's part.
fn sealed_parts_rows(samples: &[Box<Sample>]) -> Result<Vec<PartRow<'_>>> {
    if samples.len() > TICKS_PER_WINDOW_MAX {
        return Err(Error::Bound("sealed parts observations"));
    }
    let mut count = 0;
    for sample in samples {
        count += 1 + sample.apps().len() + sample.processes().len();
    }
    if count > ROWS_MAX {
        return Err(Error::Bound("sealed parts rows"));
    }

    let mut rows = Vec::with_capacity(count);
    for sample in samples {
        // A sample takes its rates only from the adjacent observation of its run, so a row
        // keeps only the publication and clocks of the observation before.
        if let Some(previous) = &sample.previous {
            assert_eq!(previous.boot, sample.selected.stamp.boot);
            assert_eq!(previous.run, sample.selected.stamp.run);
        }

        rows.push(PartRow::Header { sample });
        for (index, app) in (0_u32..).zip(sample.apps()) {
            rows.push(PartRow::App { sample, index, app });
        }
        for (index, process) in (0_u32..).zip(sample.processes()) {
            if let Some(app_index) = process.app_index {
                let app = sample
                    .apps()
                    .get(count_of(app_index)?)
                    .ok_or(Error::Bound("sealed parts process App"))?;
                rows.push(PartRow::Process {
                    sample,
                    app_index,
                    app,
                    index,
                    process,
                });
            }
        }
    }
    assert!(rows.len() <= count);

    Ok(rows)
}

/// What the engine may decode of the rows, by the rule `DECODED_BYTES_MAX` gives.
fn sealed_parts_decoded_bytes(rows: &[PartRow<'_>]) -> u64 {
    rows.iter()
        .map(|row| {
            let key = row.app().map_or(0, |(_index, app)| app.key.bytes.len());
            let name = row
                .process_row()
                .and_then(|process| process.executable_name.as_ref())
                .map_or(0, |name| name.bytes.len());
            collector::u64_from_usize(ROW_DECODED_BYTES + key + name)
        })
        .sum()
}

fn sealed_parts_header_columns(rows: &[PartRow<'_>]) -> Vec<Column> {
    let previous = |row: &PartRow<'_>| row.header().and_then(|sample| sample.previous);

    columns!(rows;
        "version" => |_| VERSION,
        "publication" => |row| row.sample().selected.stamp.publication,
        "row_kind" => PartRow::kind,
        "run_tick" => |row| row.header().map(|sample| sample.selected.run_tick),
        "wall_ns" => |row| row.header().and_then(|sample| sample.selected.wall_ns),
        "continuous_ns" => |row| {
            row.header().and_then(|sample| sample.selected.stamp.continuous_ns)
        },
        "awake_ns" => |row| row.header().and_then(|sample| sample.selected.stamp.awake_ns),
        "previous_publication" => |row| previous(row).map(|stamp| stamp.publication),
        "previous_continuous_ns" => |row| previous(row).and_then(|stamp| stamp.continuous_ns),
        "previous_awake_ns" => |row| previous(row).and_then(|stamp| stamp.awake_ns),
        "elapsed_ns" => |row| row.header().and_then(|sample| sample.elapsed_ns),
        "membership_complete" => |row| row.header().map(|sample| sample.membership_complete),
        "ungrouped_members" => |row| row.header().map(|sample| sample.ungrouped_members),
        "usage_refused" => |row| row.header().map(|sample| sample.usage_refused),
        "usage_gone" => |row| row.header().map(|sample| sample.usage_gone),
        "app_count" => |row| {
            row.header().map(|sample| collector::u64_from_usize(sample.apps().len()))
        },
        "process_count" => |row| {
            row.header().map(|sample| collector::u64_from_usize(sample.processes().len()))
        },
    )
}

fn sealed_parts_member_columns(rows: &[PartRow<'_>]) -> Vec<Column> {
    columns!(rows;
        "app_index" => |row| row.app().map(|(index, _app)| index),
        "app_kind" => |row| row.app().map(|(_index, app)| kind_code(app.key.kind)),
        "app_key" => |row| row.app().map(|(_index, app)| app.key.bytes.as_slice()),
        "members" => |row| row.app_row().map(|app| app.members),
        "live_members" => |row| row.app_row().map(|app| app.live_members),
        "app_membership_complete" => |row| row.app_row().map(|app| app.membership_complete),
        "process_index" => |row| row.process().map(|(index, _process)| index),
        "pid" => |row| row.process_row().map(|process| process.pid),
        "unique_id" => |row| {
            row.process_row()
                .and_then(|process| process.creation)
                .map(|creation| creation.unique_id)
        },
        "pid_version" => |row| {
            row.process_row()
                .and_then(|process| process.creation)
                .map(|creation| creation.pid_version)
        },
        "executable_name" => |row| {
            row.process_row()
                .and_then(|process| process.executable_name.as_ref())
                .map(|name| name.bytes.as_slice())
        },
        "program_unavailable" => |row| {
            row.process_row()
                .and_then(|process| process.program_unavailable)
                .map(identity_code)
        },
        "app_attribution" => |row| {
            row.process_row()
                .and_then(|process| process.app_attribution)
                .map(unavailable_code)
        },
        "state" => |row| row.process_row().map(|process| state_code(process.state)),
    )
}

fn sealed_parts_measure_columns(rows: &[PartRow<'_>]) -> Vec<Column> {
    let mut columns = Vec::with_capacity(MEASURES.len() * MEASURE_FIELDS.len());
    for (measure, value) in MEASURES {
        let values: Vec<Option<Measure>> =
            rows.iter().map(|row| row.app_row().map(value)).collect();
        let [known, measured, unavailable, overflowed] = measure_column_names(measure);
        columns.push(Column::new(
            known.into(),
            values
                .iter()
                .map(|measure| measure.map(|measure| measure.known))
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            measured.into(),
            values
                .iter()
                .map(|measure| measure.map(|measure| measure.measured_members))
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            unavailable.into(),
            values
                .iter()
                .map(|measure| measure.map(|measure| measure.unavailable_members))
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            overflowed.into(),
            values
                .iter()
                .map(|measure| measure.map(|measure| measure.overflowed))
                .collect::<Vec<_>>(),
        ));
    }

    columns
}

fn sealed_parts_amount_columns(rows: &[PartRow<'_>]) -> Vec<Column> {
    let mut columns = Vec::with_capacity(AMOUNTS.len() * 2);
    for (amount, value) in AMOUNTS {
        let values: Vec<Option<Amount>> = rows
            .iter()
            .map(|row| row.process_row().map(value))
            .collect();
        let [known, unavailable] = amount_column_names(amount);
        columns.push(Column::new(
            known.into(),
            values
                .iter()
                .map(|amount| match amount {
                    Some(Amount::Known(value)) => Some(*value),
                    Some(Amount::Unavailable(_)) | None => None,
                })
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            unavailable.into(),
            values
                .iter()
                .map(|amount| match amount {
                    Some(Amount::Unavailable(reason)) => Some(unavailable_code(*reason)),
                    Some(Amount::Known(_)) | None => None,
                })
                .collect::<Vec<_>>(),
        ));
    }

    columns
}

/// Every column of the table in its order.
fn column_names() -> Vec<String> {
    let mut names = Vec::with_capacity(COLUMNS);
    names.extend(FIXED_COLUMNS.map(String::from));
    for (measure, _value) in MEASURES {
        names.extend(measure_column_names(measure));
    }
    for (amount, _value) in AMOUNTS {
        names.extend(amount_column_names(amount));
    }

    names
}

fn measure_column_names(measure: &str) -> [String; 4] {
    MEASURE_FIELDS.map(|field| format!("app_{measure}_{field}"))
}

fn amount_column_names(amount: &str) -> [String; 2] {
    [
        format!("process_{amount}"),
        format!("process_{amount}_unavailable"),
    ]
}

/// The place of the fixed column `name`, which fails the build where no fixed column has it.
const fn fixed(name: &str) -> usize {
    let mut index = 0;
    while index < FIXED_COLUMNS.len() {
        if same(FIXED_COLUMNS[index], name) {
            return index;
        }
        index += 1;
    }

    panic!("no fixed column has this name");
}

/// The place of the entry `name` among `entries`, which fails the build where none has it.
const fn place<T>(entries: &[(&str, T)], name: &str) -> usize {
    let mut index = 0;
    while index < entries.len() {
        if same(entries[index].0, name) {
            return index;
        }
        index += 1;
    }

    panic!("no entry has this name");
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

impl Session {
    /// Gives `consume` the parts of the App of this key that the windows of `range` keep of
    /// their observations in it, from the table the persister derived as each window sealed. A
    /// window sealed without the table, or with a version of it this build does not read, gives
    /// none.
    pub(crate) fn visit_sealed_parts(
        &self,
        range: ContextRange,
        kind: KeyKind,
        bytes: &[u8],
        mut consume: impl FnMut(AppPart) -> Result<()>,
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
        mut consume: impl FnMut(usize, AppPart) -> Result<()>,
    ) -> Result<()> {
        let key = PartKey { kind, bytes };
        self.visit_derived(
            range,
            APP_PARTS,
            |kept| self.visit_sealed_parts_read(kept, &key),
            |window, part| {
                if observed_in(range, part.selected()) {
                    consume(window, part)?;
                }
                Ok(())
            },
        )
    }

    fn visit_sealed_parts_read(&self, kept: &Kept<'_>, key: &PartKey<'_>) -> Result<Vec<AppPart>> {
        let source = self.connection.parquet_source(kept.path)?;
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
        if rows > collector::u64_from_usize(ROWS_MAX) {
            return Err(Error::Bound("sealed parts rows"));
        }
        let version: u8 = summary.get(0)?;
        if summary.get::<u8>(1)? != version {
            return Err(Error::Bound("sealed parts version"));
        }
        if version != VERSION {
            return Ok(Vec::new());
        }

        let frame = self.connection.collect(sealed_parts_plan(
            self.connection.plan(&format!(
                "SELECT {} FROM {source}",
                column_names().join(", ")
            ))?,
            key,
        )?)?;
        if frame.height() > ROWS_MAX {
            return Err(Error::Bound("sealed parts rows"));
        }
        let mut decoder = Decoder {
            kept,
            key,
            output: Vec::with_capacity(TICKS_PER_WINDOW_MAX),
            open: None,
        };
        for index in 0..frame.height() {
            self.operation.check()?;
            decoder.push(&Row::new(&frame, index)?)?;
        }
        decoder.close()?;

        Ok(decoder.output)
    }
}

/// The App whose parts a reader takes.
struct PartKey<'key> {
    kind: KeyKind,
    bytes: &'key [u8],
}

/// The rows of every observation's facts and of the App of this key, in the order the decoder
/// takes them: by publication, then the facts, the App and its processes in their sample's
/// order.
fn sealed_parts_plan(plan: LazyFrame, key: &PartKey<'_>) -> Result<LazyFrame> {
    let app = col("app_kind")
        .eq(lit(kind_code(key.kind)))
        .and(col("app_key").eq(lit(key.bytes)));
    let limit = u32::try_from(ROWS_MAX + 1).map_err(|_| Error::Bound("sealed parts rows"))?;

    Ok(plan
        .filter(col("row_kind").eq(lit(ROW_HEADER)).or(app))
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
        .limit(limit))
}

/// Rebuilds one App's parts of a window's observations from the rows `sealed_parts_plan` gives.
struct Decoder<'read> {
    kept: &'read Kept<'read>,
    key: &'read PartKey<'read>,
    output: Vec<AppPart>,
    /// The facts of the observation the last row of facts opened, and the App's row and
    /// processes there so far.
    open: Option<(Header, Option<Present>)>,
}

impl Decoder<'_> {
    fn push(&mut self, row: &Row<'_>) -> Result<()> {
        let kind: u8 = row.get(const { fixed("row_kind") })?;
        if kind == ROW_HEADER {
            self.close()?;
            return self.open(kept_header(row, self.kept.run)?);
        }

        let Some((header, present)) = &mut self.open else {
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
                processes: Vec::with_capacity(count_of(app.members)?),
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
            if count_of(app_index)? != present.app_index {
                return Err(Error::Bound("sealed parts process App"));
            }
            if present.processes.len() == count_of(present.app.members)? {
                return Err(Error::Bound("sealed parts members"));
            }
            present.processes.push(process);
        }

        Ok(())
    }

    fn open(&mut self, header: Header) -> Result<()> {
        let publication = header.selected.stamp.publication;
        if !self.kept.publications.contains(&publication) {
            return Err(Error::Bound("sealed parts publication"));
        }
        if let Some(last) = self.output.last() {
            if last.selected().stamp.publication >= publication {
                return Err(Error::Bound("sealed parts publication"));
            }
        }
        if self.output.len() == TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("sealed parts observations"));
        }

        self.open = Some((header, None));

        Ok(())
    }

    /// Ends the observation open last, whose App must have all its processes.
    fn close(&mut self) -> Result<()> {
        let Some((header, present)) = self.open.take() else {
            return Ok(());
        };
        if let Some(present) = &present {
            if present.processes.len() != count_of(present.app.members)? {
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
        usage_refused: row.get(const { fixed("usage_refused") })?,
        usage_gone: row.get(const { fixed("usage_gone") })?,
        app_count: size_of_count(row.get(const { fixed("app_count") })?)?,
        process_count: size_of_count(row.get(const { fixed("process_count") })?)?,
    })
}

/// An App row's App and its place among its sample's Apps.
fn kept_app(row: &Row<'_>) -> Result<(usize, App)> {
    let key = Key::new(
        kind_of(row.get(const { fixed("app_kind") })?)?,
        row.get(const { fixed("app_key") })?,
    )?;
    let members: u32 = row.get(const { fixed("members") })?;
    if count_of(members)? > MEMBERS_MAX {
        return Err(Error::Bound("sealed parts members"));
    }
    let measure = |place: usize| -> Result<Measure> {
        let first = MEASURE_FIRST + place * MEASURE_FIELDS.len();
        let measure = Measure {
            known: row.get(first)?,
            measured_members: row.get(first + 1)?,
            unavailable_members: row.get(first + 2)?,
            overflowed: row.get(first + 3)?,
        };
        // Each member adds to one count at most, and a history holds counts up to the members.
        let counted = measure
            .measured_members
            .checked_add(measure.unavailable_members)
            .ok_or(Error::Bound("sealed parts measure"))?;
        if counted > members {
            return Err(Error::Bound("sealed parts measure"));
        }

        Ok(measure)
    };

    let app = App {
        display_name: key.display_name()?,
        key,
        members,
        live_members: row.get(const { fixed("live_members") })?,
        membership_complete: row.get(const { fixed("app_membership_complete") })?,
        cpu_ns: measure(const { place(&MEASURES, "cpu_ns") })?,
        energy_nj: measure(const { place(&MEASURES, "energy_nj") })?,
        disk_read_bytes: measure(const { place(&MEASURES, "disk_read_bytes") })?,
        disk_written_bytes: measure(const { place(&MEASURES, "disk_written_bytes") })?,
        disk_io_bytes: measure(const { place(&MEASURES, "disk_io_bytes") })?,
        resident_bytes: measure(const { place(&MEASURES, "resident_bytes") })?,
        footprint_bytes: measure(const { place(&MEASURES, "footprint_bytes") })?,
    };

    Ok((count_of(row.get(const { fixed("app_index") })?)?, app))
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
        cpu_ns: amount(const { place(&AMOUNTS, "cpu_ns") })?,
        energy_nj: amount(const { place(&AMOUNTS, "energy_nj") })?,
        disk_read_bytes: amount(const { place(&AMOUNTS, "disk_read_bytes") })?,
        disk_written_bytes: amount(const { place(&AMOUNTS, "disk_written_bytes") })?,
        disk_io_bytes: amount(const { place(&AMOUNTS, "disk_io_bytes") })?,
        app_attribution: app_attribution.map(unavailable_of).transpose()?,
        resident_bytes: amount(const { place(&AMOUNTS, "resident_bytes") })?,
        footprint_bytes: amount(const { place(&AMOUNTS, "footprint_bytes") })?,
        idle_wakeups: amount(const { place(&AMOUNTS, "idle_wakeups") })?,
        pageins: amount(const { place(&AMOUNTS, "pageins") })?,
        threads: amount(const { place(&AMOUNTS, "threads") })?,
        state: state_of(row.get(const { fixed("state") })?)?,
    })
}

fn count_of(value: u32) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Bound("sealed parts count"))
}

fn size_of_count(value: u64) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Bound("sealed parts count"))
}

fn unavailable_code(reason: Unavailable) -> u8 {
    match reason {
        Unavailable::Refused => 0,
        Unavailable::Gone => 1,
        Unavailable::Unknown => 2,
        Unavailable::Interval => 3,
        Unavailable::Creation => 4,
        Unavailable::Baseline => 5,
        Unavailable::CounterReset => 6,
        Unavailable::Overflow => 7,
        Unavailable::Exec => 8,
        Unavailable::Program => 9,
        Unavailable::ExitUnverified => 10,
    }
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

fn identity_code(reason: IdentityUnavailable) -> u8 {
    match reason {
        IdentityUnavailable::Refused => identity_availability::REFUSED,
        IdentityUnavailable::Gone => identity_availability::GONE,
        IdentityUnavailable::Unsupported => identity_availability::UNSUPPORTED,
        IdentityUnavailable::Changed => identity_availability::CHANGED,
        IdentityUnavailable::Invalid => identity_availability::INVALID,
        IdentityUnavailable::Exhausted => identity_availability::EXHAUSTED,
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

fn state_code(state: ProcessState) -> u8 {
    match state {
        ProcessState::Live => 0,
        ProcessState::ExitedVerified => 1,
        ProcessState::ExitedUnverified => 2,
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
