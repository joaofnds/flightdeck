use std::sync::Arc;

use persistence::{Error, Result};

use super::{QueryContext, QuerySecond, Response};
use crate::Cancellation;
use crate::app_process_groups_history::AppProcessGroupsHistory;
use crate::app_process_groups_history::reduce::{Capture, Phase, Reduction};
use crate::cancellation::Operation;
use crate::live_app_history::cohort;
use crate::live_app_response::{KeyKind, Selected};
use crate::store::ProcessRow;

impl QueryContext {
    /// The shared CPU-mean process cohort, reduced directly from retained selected-App members.
    pub fn app_process_groups_history(
        &self,
        kind: KeyKind,
        bytes: &[u8],
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> crate::store::HistoryResult<Response<Box<AppProcessGroupsHistory>>> {
        self.history_ready()?;
        kind.checked_key(bytes)?;
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let mut reservation = self.reserve_response(
            crate::app_process_groups_history::reduce::OPERATION_BYTES_MAX
                + cohort::CAPTURE_WORK_BYTES_MAX,
        )?;

        Ok(operation.run(|| {
            let selected = Selected::new(self.selected().observation, self.context_last_second())?;
            let mut reduction =
                Reduction::with_room(&selected, kind, bytes, &operation, self.seconds().len())?;
            response_groups_visit(
                self,
                kind,
                bytes,
                &mut reduction,
                Phase::Observe,
                &operation,
            )?;
            reduction.choose()?;
            response_groups_visit(
                self,
                kind,
                bytes,
                &mut reduction,
                Phase::Project,
                &operation,
            )?;

            let value = reduction.finish()?;
            reservation
                .shrink_to(size_of::<Response<Box<AppProcessGroupsHistory>>>() + value.bytes());

            Ok(Response::reserved(reservation, value))
        })?)
    }
}

fn response_groups_visit(
    context: &QueryContext,
    kind: KeyKind,
    bytes: &[u8],
    reduction: &mut Reduction<'_>,
    phase: Phase,
    operation: &Operation,
) -> Result<()> {
    for second in context.seconds() {
        operation.check()?;
        let apps = second.apps();
        if apps.missing.is_some() {
            reduction.begin(&response_groups_capture(second, false), phase)?;
            continue;
        }
        if apps.observation != Some(second.observation) {
            return Err(Error::Bound("live App stale detail"));
        }
        let index = apps
            .apps
            .iter()
            .position(|app| app.name.kind.key(&app.name.bytes) == kind.key(bytes));
        let Some(index) = index else {
            reduction.begin(&response_groups_capture(second, true), phase)?;
            continue;
        };
        if apps.processes.is_err() {
            reduction.begin(&response_groups_capture(second, false), phase)?;
            continue;
        }

        let (header, payload) = apps.presentation()?;
        let app_index = u32::try_from(index).map_err(|_| Error::Bound("live App index"))?;
        let processes = payload
            .iter()
            .filter(|process| process.app_index == Some(app_index));
        let capture = Capture {
            observation: &second.observation,
            elapsed_ns: header.elapsed_ns,
            source_available: true,
            membership_complete: header.membership_complete,
            unnamed_members: None,
            live: true,
        };
        cohort::visit_borrowed(
            &capture,
            apps.apps[index].members,
            processes.map(ProcessRow::presentation),
            reduction,
            phase,
            operation,
        )?;
    }

    Ok(())
}

fn response_groups_capture(second: &QuerySecond, available: bool) -> Capture<'_> {
    Capture {
        observation: &second.observation,
        elapsed_ns: second.apps().elapsed_ns,
        source_available: available,
        membership_complete: second.apps().membership_complete,
        unnamed_members: available.then_some(0),
        live: true,
    }
}
