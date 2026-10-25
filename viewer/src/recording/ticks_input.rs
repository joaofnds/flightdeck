use std::fs::File;
use std::sync::Arc;

use persistence::engine::{AdmissionRefusal, AdmittedTable};
use persistence::resources::ByteBudget;
use polars::prelude::DataFrame;

use super::{Refused, SealedMinute, SealedSource, SealedTable, holds_columns};
use crate::cancellation::Operation;

pub(super) struct TicksInput {
    table: AdmittedTable,
    operation: Operation,
}

pub(super) struct TicksTable<'minute> {
    legacy: Option<SealedTable>,
    admitted: Option<&'minute TicksInput>,
}

impl SealedMinute {
    #[cfg(test)]
    pub(super) fn open_with_ticks(
        source: &SealedSource<'_>,
        budget: &Arc<ByteBudget>,
        operation: &Operation,
    ) -> Result<Self, Refused> {
        Self::open_with_ticks_using(source, budget, operation, AdmittedTable::from_file_budgeted)
    }

    pub(crate) fn open_with_ticks_using(
        source: &SealedSource<'_>,
        budget: &Arc<ByteBudget>,
        operation: &Operation,
        capture: impl FnOnce(File, &Arc<ByteBudget>) -> Result<AdmittedTable, AdmissionRefusal>,
    ) -> Result<Self, Refused> {
        operation.check()?;
        let mut minute = Self::open(source.root, source.marker)?;
        let path = source
            .root
            .join(&minute.manifest.data_directory)
            .join("ticks.parquet");

        let file = collector::recording::private::open_private_file(&path, false)
            .map_err(Refused::Commitment)?;

        operation.check()?;
        let table = capture(file, budget).map_err(Refused::Input)?;
        operation.check()?;
        minute.ticks = Some(TicksInput {
            table,
            operation: operation.begin(operation.epoch())?,
        });

        Ok(minute)
    }

    pub(super) fn ticks(&self) -> Result<TicksTable<'_>, Refused> {
        if let Some(ticks) = &self.ticks {
            ticks.operation.check()?;

            let current = persistence::reread_committed(&self.root, &self.marker)
                .map_err(Refused::Commitment)?;

            if current != self.manifest {
                return Err(Refused::Commitment(std::io::ErrorKind::InvalidData.into()));
            }

            Ok(TicksTable {
                legacy: None,
                admitted: Some(ticks),
            })
        } else {
            Ok(TicksTable {
                legacy: Some(self.table(persistence::schema::Table::Ticks)?),
                admitted: None,
            })
        }
    }
}

impl TicksTable<'_> {
    pub(super) fn holds(&self, group: &[&str]) -> Result<bool, Refused> {
        match (&self.legacy, self.admitted) {
            (Some(table), None) => table.holds(group),
            (None, Some(input)) => {
                input.operation.check()?;
                holds_columns(group, |name| input.table.contains_column(name))
            }
            _ => unreachable!("ticks adapter must select exactly one input"),
        }
    }

    pub(super) fn collect(
        self,
        columns: &[&str],
        row_limit: Option<u32>,
    ) -> Result<DataFrame, Refused> {
        match (self.legacy, self.admitted) {
            (Some(mut table), None) => {
                if let Some(limit) = row_limit {
                    table.plan = table.plan.limit(limit);
                }
                table.collect(columns)
            }
            (None, Some(input)) => {
                input.operation.check()?;
                let result = input.table.read(columns, row_limit);
                input.operation.check()?;
                Ok(result?)
            }
            _ => unreachable!("ticks adapter must select exactly one input"),
        }
    }
}
