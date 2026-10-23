//! Format 1's one decoder. It reads a sealed minute's tables back into typed rows, through
//! persistence's admission bounds and the minute's commitment, with Polars' Parquet reader and
//! no SQL, and computes nothing from them, so the recording and the ring hand the store the
//! same readings.

pub(crate) mod apps;
mod machine;

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

/// A sealed minute whose marker and files `open` verified.
pub struct SealedMinute {
    root: PathBuf,
    marker: PathBuf,
    manifest: persistence::Manifest,
}

/// Why a minute was not read, so the store holds it as failed with that reason and never as
/// zeros.
#[derive(Debug)]
pub enum Refused {
    /// The minute's marker or files do not show it committed as `open` found it.
    Commitment(std::io::Error),
    /// A table broke an admission bound or did not decode as format 1 writes it.
    Decode(persistence::Error),
}

impl From<persistence::Error> for Refused {
    fn from(error: persistence::Error) -> Self {
        Self::Decode(error)
    }
}

impl SealedMinute {
    /// Verifies the marker at `marker` and every file the minute it names commits.
    pub fn open(root: &Path, marker: &Path) -> Result<Self, Refused> {
        let manifest = persistence::read_committed(root, marker).map_err(Refused::Commitment)?;
        Ok(Self {
            root: root.to_owned(),
            marker: marker.to_owned(),
            manifest,
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
        let mut plan = persistence::engine::scan(&path)?;
        let schema = plan.collect_schema().map_err(persistence::Error::from)?;
        Ok(SealedTable { plan, schema })
    }
}

/// One table of a sealed minute, through admission, and the columns its file holds.
struct SealedTable {
    plan: LazyFrame,
    schema: SchemaRef,
}

impl SealedTable {
    /// Whether the table holds the optional columns `group`, which a minute holds whole or
    /// not at all.
    fn holds(&self, group: &[&str]) -> Result<bool, Refused> {
        let held = group
            .iter()
            .filter(|name| self.schema.contains(name))
            .count();
        if held == 0 {
            return Ok(false);
        }

        if held < group.len() {
            return Err(persistence::Error::Bound("incomplete column group").into());
        }

        Ok(true)
    }

    /// The `columns`, rows in the order the file holds them.
    fn collect(self, columns: &[&str]) -> Result<DataFrame, Refused> {
        let columns: Vec<Expr> = columns.iter().map(|name| col(*name)).collect();
        self.plan
            .select(columns)
            .collect()
            .map_err(|error| Refused::Decode(persistence::Error::from(error)))
    }
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

#[cfg(test)]
#[path = "recording/tests.rs"]
mod tests;
