//! Bounded query execution over the complete logical event table.
use std::any::Any;
use std::sync::Arc;

use model::catalog::Kind;
use polars::polars_utils::idx_vec::UnitVec;
use polars::prelude::*;

use super::EventTable;
use crate::engine::{AdmissionRefusal, AdmittedReadRefusal, frame_lease};
use crate::resources::ByteBudget;
use crate::{Error, Result};

struct EventScan {
    table: EventTable,
    budget: Arc<ByteBudget>,
}

const PREDICATE_NODES_MAX: usize = 1024;
const QUERY_WORKING_BYTES: usize = 2 * 1024 * 1024;

impl EventTable {
    /// The normalized event schema also applies to a window with no physical event parts.
    pub fn empty_frame() -> Result<DataFrame> {
        Ok(DataFrame::empty_with_schema(&schema()))
    }

    /// A complete relation with bounded per-part validation and a fixed materialized-row limit.
    pub fn into_lazy(
        self,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<LazyFrame, AdmittedReadRefusal> {
        if self.rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX) {
            return Err(Error::Bound("event query source rows").into());
        }
        if self.index.is_none() {
            // Intrinsic native decode/scratch admission happens before Engine::collect.
            // Actual copies still reserve their measured lengths during each callback;
            // later shared-ledger pressure can refuse those without allocating output.
            let _query = budget
                .reserve(QUERY_WORKING_BYTES)
                .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))?;
            super::read::preflight(&self.part(0)?, budget)?;
        }
        Ok(LazyFrame::anonymous_scan(
            Arc::new(EventScan {
                table: self,
                budget: Arc::clone(budget),
            }),
            ScanArgsAnonymous {
                schema: Some(Arc::new(schema())),
                name: "EVENT PARTS",
                ..Default::default()
            },
        )
        .map_err(Error::from)?)
    }

    /// Reduces validated physical slices without materializing the full relation.
    pub fn count_budgeted(
        &self,
        predicate: Option<&Expr>,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<u64, AdmittedReadRefusal> {
        let _reservation = budget
            .reserve(QUERY_WORKING_BYTES)
            .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))?;
        if let Some(predicate) = predicate {
            scan_predicate_validate(predicate, true)?;
        }
        let mut count = 0;
        self.query_visit(None, budget, |frame| {
            let selected = select(frame, predicate, None)?;
            count += collector::u64_from_usize(selected.height());
            Ok(())
        })?;
        Ok(count)
    }

    fn query_visit(
        &self,
        prefix: Option<usize>,
        budget: &Arc<ByteBudget>,
        mut consume: impl FnMut(DataFrame) -> std::result::Result<(), AdmittedReadRefusal>,
    ) -> std::result::Result<(), AdmittedReadRefusal> {
        if self.rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX) {
            return Err(Error::Bound("event query source rows").into());
        }
        if !self.pin.validate()? {
            return Err(Error::Bound("changed event query source").into());
        }
        let mut remaining = prefix.unwrap_or(crate::event_observation::ROWS_MAX);
        for index in 0..self.len() {
            if remaining == 0 {
                break;
            }
            let part = self.part(index)?;
            let slices = part
                .rows()
                .div_ceil(collector::u64_from_usize(
                    crate::event_observation::BATCH_ROWS_MAX,
                ))
                .max(1);
            for slice in 0..slices {
                if remaining == 0 {
                    break;
                }
                let offset =
                    slice * collector::u64_from_usize(crate::event_observation::BATCH_ROWS_MAX);
                let count = remaining.min(crate::event_observation::BATCH_ROWS_MAX);
                let count =
                    u32::try_from(count).map_err(|_| Error::Bound("event query slice rows"))?;
                let frame = part.read_slice_pinned(offset, count, budget, &self.pin, index)?;
                let (frame, reservation) = frame.into_parts();
                remaining -= frame.height();
                consume(frame)?;
                drop(reservation);
            }
        }
        if !self.pin.validate()? {
            return Err(Error::Bound("changed event query source").into());
        }
        Ok(())
    }
}

impl EventScan {
    fn scan_budgeted(
        &self,
        options: &AnonymousScanArgs,
    ) -> std::result::Result<DataFrame, AdmittedReadRefusal> {
        let _reservation = self
            .budget
            .reserve(QUERY_WORKING_BYTES)
            .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))?;
        let predicate = options
            .predicate
            .as_ref()
            .map(scan_predicate)
            .transpose()?
            .flatten();
        let output_schema = options.output_schema.as_ref().unwrap_or(&options.schema);
        let mut output = DataFrame::empty_with_schema(output_schema);
        self.table
            .query_visit(options.n_rows, &self.budget, |frame| {
                let selected = select(frame, predicate.as_ref(), options.with_columns.as_deref())?;
                let rows = collector::u64_from_usize(output.height())
                    + collector::u64_from_usize(selected.height());
                if rows > crate::schema::IDENTITY_ROWS_MAX {
                    return Err(
                        Error::Bound("event query output rows; use event-identity pages").into(),
                    );
                }
                if selected.height() > 0 {
                    let owned = frame_lease::copy_event_frame(&selected, &self.budget)?;
                    output.vstack_mut(&owned).map_err(Error::from)?;
                }
                Ok(())
            })?;
        Ok(output)
    }
}

fn scan_predicate(predicate: &Expr) -> Result<Option<Expr>> {
    scan_predicate_validate(predicate, false)?;
    let mut pending = Vec::with_capacity(PREDICATE_NODES_MAX);
    pending.push(predicate);
    let mut retained: Option<Expr> = None;
    for _step in 0..PREDICATE_NODES_MAX {
        let Some(expression) = pending.pop() else {
            return Ok(retained);
        };
        match expression {
            Expr::BinaryExpr {
                left,
                op: Operator::And | Operator::LogicalAnd,
                right,
            } => {
                pending.push(right);
                pending.push(left);
            }
            Expr::Display { inputs, fmt_str } => {
                if !scan_predicate_hint(inputs, fmt_str) {
                    return Err(Error::Bound("unsupported event query runtime predicate"));
                }
            }
            expression => {
                scan_predicate_validate(expression, true)?;
                retained = Some(match retained {
                    Some(previous) => previous.and(expression.clone()),
                    None => expression.clone(),
                });
            }
        }
    }
    if pending.is_empty() {
        Ok(retained)
    } else {
        Err(Error::Bound("event query predicate nodes"))
    }
}

fn scan_predicate_validate(predicate: &Expr, refuse_display: bool) -> Result<()> {
    let mut pending = UnitVec::with_capacity(PREDICATE_NODES_MAX);
    pending.push(predicate);
    let mut admitted = 1;
    for _step in 0..PREDICATE_NODES_MAX {
        let Some(expression) = pending.pop() else {
            return Ok(());
        };
        if refuse_display && matches!(expression, Expr::Display { .. }) {
            return Err(Error::Bound(
                "unsupported event query nested runtime predicate",
            ));
        }
        let children = scan_predicate_children(expression)?;
        if children > PREDICATE_NODES_MAX - admitted {
            return Err(Error::Bound("event query predicate nodes"));
        }
        admitted += children;
        expression.nodes(&mut pending);
        assert!(pending.len() <= PREDICATE_NODES_MAX);
    }
    if pending.is_empty() {
        Ok(())
    } else {
        Err(Error::Bound("event query predicate nodes"))
    }
}

fn scan_predicate_children(expression: &Expr) -> Result<usize> {
    Ok(match expression {
        Expr::Column(_)
        | Expr::Literal(_)
        | Expr::DataTypeFunction(_)
        | Expr::Len
        | Expr::Element
        | Expr::Field(_) => 0,
        Expr::Alias(..)
        | Expr::Cast { .. }
        | Expr::Sort { .. }
        | Expr::Agg(_)
        | Expr::Explode { .. }
        | Expr::KeepName(_)
        | Expr::RenameAlias { .. } => 1,
        Expr::BinaryExpr { .. } | Expr::Gather { .. } | Expr::Filter { .. } | Expr::Eval { .. } => {
            2
        }
        Expr::Ternary { .. } | Expr::Slice { .. } => 3,
        Expr::SortBy { by, .. } => by.len().saturating_add(1),
        Expr::Over {
            partition_by,
            order_by,
            ..
        } => partition_by
            .len()
            .saturating_add(1 + usize::from(order_by.is_some())),
        Expr::StructEval { evaluation, .. } => evaluation.len().saturating_add(1),
        Expr::Function { input, .. }
        | Expr::AnonymousFunction { input, .. }
        | Expr::PipeWithDtype { input, .. } => input.len(),
        Expr::Display { inputs, .. } => inputs.len(),
        Expr::SubPlan(..) | Expr::Selector(_) => {
            return Err(Error::Bound("unsupported event query predicate plan"));
        }
    })
}

fn scan_predicate_hint(inputs: &[Expr], label: &str) -> bool {
    // Pinned Polars 22a147d erases optional top-k/join hints into Display at this callback.
    // Only a complete AND minterm can be omitted; semantic expressions still run unchanged.
    let [Expr::Column(column)] = inputs else {
        return false;
    };
    if !model::catalog::EVENTS
        .columns
        .iter()
        .chain(
            model::catalog::EVENTS
                .groups
                .iter()
                .flat_map(|group| group.iter()),
        )
        .any(|field| field.name == column.as_str())
    {
        return false;
    }
    let Some(identifier) = label.strip_prefix("dynamic_pred: ") else {
        return false;
    };
    if identifier == "dropped" {
        return true;
    }
    if identifier.len() != 36 {
        return false;
    }
    identifier.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
        }
    })
}

impl AnonymousScan for EventScan {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn scan(&self, options: AnonymousScanArgs) -> PolarsResult<DataFrame> {
        self.scan_budgeted(&options)
            .map_err(|error| PolarsError::ComputeError(ordinary_error(error).to_string().into()))
    }

    fn allows_predicate_pushdown(&self) -> bool {
        true
    }

    fn allows_projection_pushdown(&self) -> bool {
        true
    }
}

fn select(
    frame: DataFrame,
    predicate: Option<&Expr>,
    columns: Option<&[PlSmallStr]>,
) -> Result<DataFrame> {
    let mut plan = frame.lazy();
    if let Some(predicate) = predicate {
        plan = plan.filter(predicate.clone());
    }
    if let Some(columns) = columns {
        plan = plan.select(
            columns
                .iter()
                .map(|name| col(name.clone()))
                .collect::<Vec<_>>(),
        );
    }
    Ok(plan.collect()?)
}

fn schema() -> Schema {
    model::catalog::EVENTS
        .columns
        .iter()
        .chain(
            model::catalog::EVENTS
                .groups
                .iter()
                .flat_map(|group| group.iter()),
        )
        .map(|column| {
            let data_type = match column.kind {
                Kind::U8 => DataType::UInt8,
                Kind::U64 => DataType::UInt64,
                Kind::I32 => DataType::Int32,
                Kind::Bytes => DataType::Binary,
                _ => unreachable!("event catalog type"),
            };
            (column.name.into(), data_type)
        })
        .collect()
}

pub(crate) fn ordinary_error(error: AdmittedReadRefusal) -> Error {
    super::read::ordinary_error(error)
}

#[cfg(test)]
mod tests;
