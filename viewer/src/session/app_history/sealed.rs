//! What the readers of the tables a window derives as it seals share: the windows of a range
//! that hold a table, and the codes their rows give an App's kind.
use std::ops::Range;
use std::path::Path;

use super::{ContextRange, Session};
use crate::live_app_response::{KeyKind, Observation};
use collector::ring::RunIdentity;
use persistence::{Error, Result};

/// A sealed window's derived table, and what its seal recorded of it.
pub(super) struct Kept<'manifest> {
    pub path: &'manifest Path,
    pub run: &'manifest RunIdentity,
    pub publications: Range<u64>,
    pub rows: u64,
}

impl Session {
    /// Gives `read` each window of `range` the persister sealed with the derived table `name`,
    /// and `consume` what `read` takes from it, with the window's index, once the window's
    /// commitment still holds, so a window replaced during the read gives nothing.
    pub(super) fn visit_derived<T>(
        &self,
        range: ContextRange,
        name: &str,
        mut read: impl FnMut(&Kept<'_>) -> Result<Vec<T>>,
        mut consume: impl FnMut(usize, T) -> Result<()>,
    ) -> Result<()> {
        self.operation.attach(&self.connection)?;
        assert!(self.windows.len() <= super::super::WINDOWS_MAX);
        for (window, manifest) in self.windows.iter().enumerate() {
            self.operation.check()?;
            if let Some(bounds) = manifest.wall_ns {
                if bounds.max < range.first_ns {
                    continue;
                }
                if bounds.min >= range.end_ns {
                    continue;
                }
            }
            let Some(table) = manifest.tables.iter().find(|table| table.name == name) else {
                continue;
            };

            self.validate_commitment(window)?;
            let values = read(&Kept {
                path: &self
                    .root
                    .join(&manifest.data_directory)
                    .join(format!("{name}.parquet")),
                run: &manifest.identity.run,
                publications: manifest.publication_first..manifest.publication_end,
                rows: table.rows,
            })?;
            self.validate_commitment(window)?;

            for value in values {
                consume(window, value)?;
            }
        }

        self.operation.check()
    }
}

/// The columns of a derived table, each named and taking its value at every row from `value`.
macro_rules! columns {
    ($rows:ident; $($name:literal => $value:expr),+ $(,)?) => {
        vec![$(
            polars::prelude::Column::new(
                $name.into(),
                $rows.iter().map($value).collect::<Vec<_>>(),
            )
        ),+]
    };
}
pub(super) use columns;

/// Whether the observation's wall clock lies in `range`.
pub(super) fn observed_in(range: ContextRange, observation: &Observation) -> bool {
    observation
        .wall_ns
        .is_some_and(|wall| (range.first_ns..range.end_ns).contains(&wall))
}

pub(super) fn kind_code(kind: KeyKind) -> u8 {
    match kind {
        KeyKind::Bundle => 0,
        KeyKind::Executable => 1,
    }
}

pub(super) fn kind_of(code: u8) -> Result<KeyKind> {
    match code {
        0 => Ok(KeyKind::Bundle),
        1 => Ok(KeyKind::Executable),
        _ => Err(Error::Bound("sealed App kind")),
    }
}

#[cfg(test)]
#[path = "sealed_fixture.rs"]
pub(super) mod fixture;
