//! Synchronous replay shared by chronological loading and bounded startup scheduling.
use std::sync::Arc;

use super::Decoder;
use crate::byte_budget::ByteBudget;
use crate::cancellation::Operation;
use crate::live_app_response::Sample;
use crate::live_apps::committed::{Endpoint, PlacedFrame, Placement, PlacementError};
use crate::live_apps::{AggregateError, Frame};
use crate::recording::processes::ProcessBatch;
use crate::recording::{Before, Refused};

pub(in crate::store::recording) trait Target {
    type Error: From<Refused>;

    fn derives(&self, endpoint: &Endpoint) -> bool;
    fn consume(&mut self, captured: Captured<'_>) -> Result<Progress, Self::Error>;
}

pub(in crate::store::recording) struct Captured<'sample> {
    pub endpoint: Endpoint,
    pub names_capacity: usize,
    pub sample: Option<Result<&'sample Sample, AggregateError>>,
    pub before: Option<Before>,
}

pub(in crate::store::recording) enum Progress {
    Accepted(Option<Before>),
    Pending,
}

pub(in crate::store::recording) struct Input<'step> {
    pub next: &'step mut usize,
    pub operation: &'step Operation,
    pub budget: Option<&'step Arc<ByteBudget>>,
}

impl Decoder {
    pub(in crate::store::recording) fn replay<T: Target>(
        &mut self,
        batch: &ProcessBatch,
        input: &mut Input<'_>,
        target: &mut T,
    ) -> Result<(), T::Error> {
        let names_capacity = batch
            .rows
            .height()
            .checked_add(self.predecessor.as_ref().map_or(0, Before::program_count))
            .ok_or(Refused::Decode(persistence::Error::Bound(
                "sealed name count",
            )))?;
        let result = Frame::placed(
            &Placement {
                endpoints: &batch.endpoints,
                rows: &batch.rows,
                order: &batch.order,
                first: *input.next,
                operation: input.operation,
                budget: input.budget,
            },
            |endpoint, frame| {
                let mut sample = None;
                let captured = replay_capture(
                    endpoint,
                    frame,
                    self.predecessor.as_ref(),
                    &mut sample,
                    target.derives(endpoint),
                    names_capacity,
                );
                match target.consume(captured)? {
                    Progress::Accepted(before) => {
                        self.accept_replay(before, input.next);
                        Ok(true)
                    }
                    Progress::Pending => Ok(false),
                }
            },
        );
        match result {
            Ok(()) => Ok(()),
            Err(PlacementError::Source(error)) => Err(T::Error::from(Refused::from(error))),
            Err(PlacementError::Budget) => Err(T::Error::from(Refused::Input(
                persistence::engine::AdmissionRefusal::Budget,
            ))),
            Err(PlacementError::Consumer(error)) => Err(error),
        }
    }

    pub(in crate::store::recording) fn accept_replay(
        &mut self,
        before: Option<Before>,
        next: &mut usize,
    ) {
        assert!(*next < crate::live_apps::committed::ENDPOINTS_MAX);
        self.predecessor = before;
        *next += 1;
    }

    /// Replays `processes`, a minute's process frames, from the frame at `first_frame_index`,
    /// deriving no Apps, so that the last of them gives the first second of the minute after it its
    /// App rates.
    pub(super) fn visit_predecessor(
        &mut self,
        processes: &ProcessBatch,
        first_frame_index: usize,
        operation: &Operation,
    ) -> Result<(), Refused> {
        self.visit(processes, first_frame_index, operation, &mut Predecessor)
    }

    pub(super) fn visit_samples<F>(
        &mut self,
        processes: &ProcessBatch,
        operation: &Operation,
        consume: F,
    ) -> Result<(), Refused>
    where
        F: FnMut(&Endpoint, persistence::Result<&Sample>, usize) -> persistence::Result<()>,
    {
        self.visit(processes, 0, operation, &mut Visitor { consume })
    }

    /// Replays `processes` into `target` from the frame at `first_frame_index`, and forgets the
    /// predecessor where the replay is refused.
    fn visit<T: Target<Error = Refused>>(
        &mut self,
        processes: &ProcessBatch,
        first_frame_index: usize,
        operation: &Operation,
        target: &mut T,
    ) -> Result<(), Refused> {
        let mut next = first_frame_index;
        let result = operation.check().map_err(Refused::from).and_then(|()| {
            self.replay(
                processes,
                &mut Input {
                    next: &mut next,
                    operation,
                    budget: None,
                },
                target,
            )
        });
        if result.is_err() {
            self.predecessor = None;
        }
        result
    }
}

fn replay_capture<'sample>(
    endpoint: &Endpoint,
    frame: PlacedFrame,
    before: Option<&Before>,
    sample: &'sample mut Option<Box<Sample>>,
    derives: bool,
    names_capacity: usize,
) -> Captured<'sample> {
    let previous = before.and_then(|before| {
        let (endpoint, previous) = before.previous();
        frame
            .get()
            .observation()
            .can_use_predecessor(&endpoint.observation())
            .then_some(previous)
    });
    let captured = if derives {
        Sample::capture_typed(frame.get(), previous, sample)
    } else {
        frame.get().validate_input_typed(previous)
    };
    let retains = match &captured {
        Ok(()) => true,
        Err(error) => error.retains_input(),
    };
    let before = if retains {
        Some(Before::from_placed(*endpoint, frame))
    } else {
        None
    };
    let sample = if derives {
        Some(captured.map(|()| sample.as_deref().unwrap_or_else(|| unreachable!("sample"))))
    } else {
        None
    };
    Captured {
        endpoint: *endpoint,
        names_capacity,
        sample,
        before,
    }
}

/// Keeps of each frame only what rates the frame after it, deriving no Apps.
pub(in crate::store::recording) struct Predecessor;

impl Target for Predecessor {
    type Error = Refused;

    fn derives(&self, _endpoint: &Endpoint) -> bool {
        false
    }

    fn consume(&mut self, captured: Captured<'_>) -> Result<Progress, Refused> {
        Ok(Progress::Accepted(captured.before))
    }
}

struct Visitor<F> {
    consume: F,
}

impl<F> Target for Visitor<F>
where
    F: FnMut(&Endpoint, persistence::Result<&Sample>, usize) -> persistence::Result<()>,
{
    type Error = Refused;

    fn derives(&self, _endpoint: &Endpoint) -> bool {
        true
    }

    fn consume(&mut self, captured: Captured<'_>) -> Result<Progress, Refused> {
        let sample = captured
            .sample
            .unwrap_or_else(|| unreachable!("derived sample"));
        (self.consume)(
            &captured.endpoint,
            sample.map_err(AggregateError::legacy),
            captured.names_capacity,
        )?;
        Ok(Progress::Accepted(captured.before))
    }
}
