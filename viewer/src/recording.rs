//! Format 1's one decoder. It reads a sealed minute's tables back into typed rows, through
//! persistence's admission bounds and the minute's commitment, with Polars' Parquet reader and
//! no SQL, and computes nothing from them, so the recording and the ring hand the store the
//! same readings.

pub(crate) mod apps;
mod machine;
mod minute_ticks;
mod processes;
mod ticks_input;
use ticks_input::TicksInput;

use std::ops::Range;
use std::path::{Path, PathBuf};

use persistence::engine::Cells;
use persistence::schema::Table;
use polars::prelude::{DataFrame, Expr, LazyFrame, SchemaRef, UInt64Type, col};

pub use apps::{AppMinute, AppParts, AppRow, Previous, SampleRow, read_apps};
pub use machine::{
    CoreStateRow, CpuRow, FanRow, GpuStateRow, InterfaceRow, MachineMinute, MemoryRow, TickRow,
    read_machine,
};
pub use minute_ticks::{MinuteTick, MinuteTicks};
pub use processes::{Before, derive_apps};

/// An owned sealed minute whose marker and files `open` verified.
pub type SealedMinute = ScopedSealedMinute<'static>;

/// A sealed minute that may borrow an opt-in input factory for one synchronous load.
pub struct ScopedSealedMinute<'input> {
    root: PathBuf,
    marker: PathBuf,
    manifest: persistence::Manifest,
    ticks: Option<TicksInput>,
    inputs: Option<TableInputs<'input>>,
}

struct TableInputs<'input> {
    budget: &'input std::sync::Arc<persistence::resources::ByteBudget>,
    operation: &'input crate::cancellation::Operation,
    capture: &'input InputCapture<'input>,
}

pub(crate) type InputCapture<'input> = dyn Fn(
        std::fs::File,
        &std::sync::Arc<persistence::resources::ByteBudget>,
    ) -> Result<persistence::engine::AdmittedTable, persistence::engine::AdmissionRefusal>
    + Send
    + Sync
    + 'input;

pub(crate) struct SealedSource<'path> {
    pub(crate) root: &'path Path,
    pub(crate) marker: &'path Path,
}

/// Why a minute was not read, so the store holds it as failed with that reason and never as
/// zeros.
#[derive(Debug)]
pub enum Refused {
    /// The minute's marker or files do not show it committed as `open` found it.
    Commitment(std::io::Error),
    /// A table broke an admission bound or did not decode as format 1 writes it.
    Decode(persistence::Error),
    /// An admitted input or its column storage could not be reserved or admitted.
    Input(persistence::engine::AdmissionRefusal),
}

impl From<persistence::Error> for Refused {
    fn from(error: persistence::Error) -> Self {
        Self::Decode(error)
    }
}

impl ScopedSealedMinute<'_> {
    pub(crate) fn revalidate_full(&self) -> Result<(), Refused> {
        let current =
            persistence::read_committed(&self.root, &self.marker).map_err(Refused::Commitment)?;

        if current != self.manifest {
            return Err(Refused::Commitment(std::io::ErrorKind::InvalidData.into()));
        }

        Ok(())
    }

    /// The full run identity of the manifest whose commitment `open` verified.
    pub(crate) fn identity(&self) -> &collector::ring::RunIdentity {
        &self.manifest.identity.run
    }

    /// Verifies the marker at `marker` and every file the minute it names commits.
    pub fn open(root: &Path, marker: &Path) -> Result<Self, Refused> {
        let manifest = persistence::read_committed(root, marker).map_err(Refused::Commitment)?;
        Ok(Self {
            root: root.to_owned(),
            marker: marker.to_owned(),
            manifest,
            ticks: None,
            inputs: None,
        })
    }

    /// Table `table`, once the marker still links the minute `open` verified.
    fn table(&self, table: Table) -> Result<SealedTable, Refused> {
        self.scan(table.name())
    }

    /// The rows the seal counted of the table `name`, `None` where the minute was sealed
    /// without it.
    fn rows(&self, name: &str) -> Option<u64> {
        let table = self
            .manifest
            .tables
            .iter()
            .find(|table| table.name == name)?;
        Some(table.rows)
    }

    /// The publications the minute's observations hold.
    fn publications(&self) -> Range<u64> {
        self.manifest.publication_first..self.manifest.publication_end
    }

    /// The table `name`, once the marker still links the minute `open` verified. A sealed
    /// minute's files never change, so the marker standing shows them unchanged without
    /// verifying them again.
    fn scan(&self, name: &str) -> Result<SealedTable, Refused> {
        let committed =
            persistence::reread_committed(&self.root, &self.marker).map_err(Refused::Commitment)?;
        if committed != self.manifest {
            return Err(Refused::Commitment(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "changed commitment",
            )));
        }

        let path = self
            .root
            .join(&self.manifest.data_directory)
            .join(format!("{name}.parquet"));
        if let Some(inputs) = &self.inputs {
            inputs.operation.check()?;

            let file = collector::recording::private::open_private_file(&path, false)
                .map_err(Refused::Commitment)?;

            inputs.operation.check()?;
            let input = (inputs.capture)(file, inputs.budget).map_err(Refused::Input)?;
            inputs.operation.check()?;
            let (plan, columns) = input.into_scan()?;

            return Ok(SealedTable {
                plan,
                columns: SealedColumns::Admitted(columns),
                operation: Some(inputs.operation.begin(inputs.operation.epoch())?),
            });
        }

        let mut plan = persistence::engine::scan(&path)?;
        let schema = plan.collect_schema().map_err(persistence::Error::from)?;
        Ok(SealedTable {
            plan,
            columns: SealedColumns::Legacy(schema),
            operation: None,
        })
    }
}

/// One table of a sealed minute, through admission, and the columns its file holds.
struct SealedTable {
    plan: LazyFrame,
    columns: SealedColumns,
    operation: Option<crate::cancellation::Operation>,
}

enum SealedColumns {
    Legacy(SchemaRef),
    Admitted(persistence::engine::AdmittedColumns),
}

impl SealedTable {
    /// Whether the table holds the optional columns `group`, which a minute holds whole or
    /// not at all.
    fn holds(&self, group: &[&str]) -> Result<bool, Refused> {
        if let Some(operation) = &self.operation {
            operation.check()?;
        }

        match &self.columns {
            SealedColumns::Legacy(schema) => holds_columns(group, |name| schema.contains(name)),
            SealedColumns::Admitted(columns) => holds_columns(group, |name| columns.contains(name)),
        }
    }

    /// The `columns`, rows in the order the file holds them.
    fn collect(mut self, columns: &[&str]) -> Result<DataFrame, Refused> {
        let columns: Vec<Expr> = columns.iter().map(|name| col(*name)).collect();
        self.plan = self.plan.select(columns);

        self.collect_plan()
    }

    fn collect_plan(self) -> Result<DataFrame, Refused> {
        Ok(collect_plan(self.plan, self.operation.as_ref())?)
    }
}

fn collect_plan(
    plan: LazyFrame,
    operation: Option<&crate::cancellation::Operation>,
) -> persistence::Result<DataFrame> {
    if let Some(operation) = operation {
        operation.check()?;
    }

    let result = plan.collect().map_err(persistence::Error::from);
    if let Some(operation) = operation {
        operation.check()?;
    }

    result
}

fn holds_columns(group: &[&str], mut contains: impl FnMut(&str) -> bool) -> Result<bool, Refused> {
    let held = group.iter().filter(|name| contains(name)).count();
    if held == 0 {
        return Ok(false);
    }

    if held < group.len() {
        return Err(persistence::Error::Bound("incomplete column group").into());
    }

    Ok(true)
}

/// The place in its table of each row in publication order, the rows of one publication in the
/// order the table holds them, where `publication` holds the publication of each of the table's
/// `rows` rows and `publications`, strictly increasing, those its rows may name. Refuses a row
/// of any other publication for `refusal`. A sealed minute's tables keep each entity's rows
/// together, which compress far better, and a store reads them a publication at a time, so one
/// pass over the rows replaces a sort that cost more than decoding the table.
fn placement(
    publications: &[u64],
    publication: &Cells<'_, UInt64Type>,
    rows: usize,
    refusal: &'static str,
) -> persistence::Result<Vec<usize>> {
    for pair in publications.windows(2) {
        assert!(pair[0] < pair[1]);
    }

    // The first place of each publication's rows, once the rows of those before it are counted.
    let mut starts = vec![0_usize; publications.len() + 1];
    let mut places = Vec::with_capacity(rows);
    for row in 0..rows {
        let place = publications
            .binary_search(&publication.get(row)?)
            .map_err(|_| persistence::Error::Bound(refusal))?;
        starts[place + 1] += 1;
        places.push(place);
    }
    for place in 1..starts.len() {
        starts[place] += starts[place - 1];
    }
    assert_eq!(starts[publications.len()], rows);

    let mut order = vec![0_usize; rows];
    for (row, place) in places.into_iter().enumerate() {
        order[starts[place]] = row;
        starts[place] += 1;
    }
    Ok(order)
}

/// Refuses `rows` for `refusal` unless the `key` of each row is greater than the key of the row
/// before it.
fn ordered<T, K: Ord>(
    rows: &[T],
    refusal: &'static str,
    key: impl Fn(&T) -> K,
) -> Result<(), Refused> {
    for pair in rows.windows(2) {
        if key(&pair[0]) >= key(&pair[1]) {
            return Err(persistence::Error::Bound(refusal).into());
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "recording/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "recording/ticks_input_tests.rs"]
mod ticks_input_tests;

#[cfg(test)]
mod table_input_tests;
