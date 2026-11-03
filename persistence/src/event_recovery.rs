//! Immutable terminal events have run/event identity and no fabricated observation time.
use std::ops::Range;
use std::path::{Path, PathBuf};

use collector::ring::RunIdentity;
use polars::prelude::DataFrame;

use crate::{Losses, Result};

pub(crate) mod batch;
mod discovery;
pub(crate) mod metadata;
pub(crate) mod read;

pub(crate) use batch::write_tail;
pub use discovery::{Discovery, DiscoveryPage};
pub(crate) use metadata::Manifest;
pub use read::read_page;

pub struct EventPage {
    /// Every retained Arrow buffer carries its reservation through native clones and slices.
    pub rows: DataFrame,
    /// The next event number to request, independent of overwritten gaps.
    pub next: Option<u64>,
    pub coverage: Range<u64>,
    pub losses: Losses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recovery {
    pub run: RunIdentity,
    pub events_first: u64,
    pub events_end: u64,
    pub rows: u64,
    pub losses: Losses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    pub first: u64,
    pub end: u64,
    pub rows: u64,
}

pub(crate) fn base(root: &Path) -> PathBuf {
    root.join("event-recovery")
}

pub(crate) fn key(run: &RunIdentity) -> String {
    format!(
        "{}-{}",
        collector::recording::identity_text(run.run),
        collector::recording::identity_text(run.boot)
    )
}

pub(crate) fn directory(root: &Path, run: &RunIdentity) -> PathBuf {
    base(root).join(key(run))
}

pub(crate) fn batch_name(index: usize) -> String {
    format!("{index:04}.parquet")
}

pub(crate) fn validate_run(root: &Path, run: &RunIdentity) -> Result<()> {
    if collector::recording::read_identity(root)? != run.recording {
        return Err(crate::Error::Bound("terminal recording identity"));
    }
    collector::recording::private::verify_directory(root)?;
    Ok(())
}
