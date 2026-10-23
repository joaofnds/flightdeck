//! Format 1's one decoder. It reads a sealed minute's tables back into typed rows, through
//! persistence's admission bounds and the minute's commitment, with Polars' Parquet reader and
//! no SQL, and computes nothing from them, so the recording and the ring hand the store the
//! same readings.

mod machine;

use std::path::{Path, PathBuf};

use polars::prelude::{DataFrame, Expr, SortMultipleOptions, col};

pub use machine::{CpuRow, InterfaceRow, MachineMinute, MemoryRow, TickRow, read_machine};

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

    /// The `columns` of table `table`, rows in the order of the columns `order` names, once the
    /// marker still links the minute `open` verified. A sealed minute's files never change, so
    /// the marker standing shows them unchanged without verifying them again.
    fn collect(&self, table: &str, columns: &[&str], order: &[&str]) -> Result<DataFrame, Refused> {
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
            .join(format!("{table}.parquet"));
        let columns: Vec<Expr> = columns.iter().map(|name| col(*name)).collect();
        let sort = SortMultipleOptions {
            descending: vec![false; order.len()],
            nulls_last: vec![false; order.len()],
            multithreaded: false,
            maintain_order: true,
            limit: None,
        };
        let plan = persistence::engine::scan(&path)
            .map_err(Refused::Decode)?
            .select(columns)
            .sort(order.iter().copied(), sort);
        plan.collect()
            .map_err(|error| Refused::Decode(persistence::Error::from(error)))
    }
}

#[cfg(test)]
#[path = "recording/tests.rs"]
mod tests;
