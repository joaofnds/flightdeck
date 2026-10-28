//! Synchronous replay shared by chronological loading and bounded startup scheduling.
use std::sync::Arc;

use super::Decoder;
use crate::byte_budget::ByteBudget;
use crate::cancellation::Operation;
use crate::live_app_response::Sample;
use crate::live_apps::committed::Endpoint;
use crate::live_apps::{AggregateError, Frame};
use crate::recording::processes::ProcessBatch;
use crate::recording::{Before, Refused, ScopedSealedMinute};

pub(in crate::store::recording) trait Target {
    type Error: From<Refused>;

    fn derives(&self, endpoint: &Endpoint) -> bool;
    fn consume(&mut self, captured: Captured<'_>) -> Result<Progress, Self::Error>;
}

pub(in crate::store::recording) struct Captured<'sample> {
    pub endpoint: Endpoint,
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
        let mut sample = None;
        let mut failure = None;
        Frame::placed_many_from(
            &batch.endpoints,
            &batch.rows,
            &batch.order,
            *input.next,
            input.operation,
            |endpoint, frame| {
                let result = replay_capture(
                    endpoint,
                    frame,
                    self.predecessor.as_ref(),
                    &mut sample,
                    target.derives(endpoint),
                    input.budget,
                )
                .map_err(T::Error::from)
                .and_then(|captured| target.consume(captured));
                match result {
                    Ok(Progress::Accepted(before)) => {
                        self.accept_replay(before, input.next);
                        Ok(true)
                    }
                    Ok(Progress::Pending) => Ok(false),
                    Err(error) => {
                        failure = Some(error);
                        Ok(false)
                    }
                }
            },
        )
        .map_err(Refused::from)?;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
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

    pub(super) fn visit_samples(
        &mut self,
        sealed: &ScopedSealedMinute<'_>,
        operation: &Operation,
        consume: impl FnMut(&Endpoint, persistence::Result<&Sample>) -> persistence::Result<()>,
    ) -> Result<(), Refused> {
        let result = self.visit_samples_replay(sealed, operation, consume);
        if result.is_err() {
            self.predecessor = None;
        }
        result
    }

    fn visit_samples_replay(
        &mut self,
        sealed: &ScopedSealedMinute<'_>,
        operation: &Operation,
        consume: impl FnMut(&Endpoint, persistence::Result<&Sample>) -> persistence::Result<()>,
    ) -> Result<(), Refused> {
        operation.check()?;
        let batch = crate::recording::processes::read_processes_batch(sealed)?;
        operation.check()?;
        let mut next = 0;
        self.replay(
            &batch,
            &mut Input {
                next: &mut next,
                operation,
                budget: None,
            },
            &mut Visitor { consume },
        )
    }
}

fn replay_capture<'sample>(
    endpoint: &Endpoint,
    frame: Box<Frame>,
    before: Option<&Before>,
    sample: &'sample mut Option<Box<Sample>>,
    derives: bool,
    budget: Option<&Arc<ByteBudget>>,
) -> Result<Captured<'sample>, Refused> {
    let previous = before.and_then(|before| {
        let (endpoint, previous) = before.previous();
        frame
            .observation()
            .can_use_predecessor(&endpoint.observation())
            .then_some(previous)
    });
    let captured = if derives {
        Sample::capture_typed(&frame, previous, sample)
    } else {
        frame.validate_input_typed(previous)
    };
    let retains = match &captured {
        Ok(()) => true,
        Err(error) => error.retains_input(),
    };
    let before = if retains {
        Some(match budget {
            Some(budget) => Before::admitted(*endpoint, frame, budget).ok_or(Refused::Input(
                persistence::engine::AdmissionRefusal::Budget,
            ))?,
            None => Before::from_frame(*endpoint, frame),
        })
    } else {
        None
    };
    let sample = if derives {
        Some(captured.map(|()| sample.as_deref().unwrap_or_else(|| unreachable!("sample"))))
    } else {
        None
    };
    Ok(Captured {
        endpoint: *endpoint,
        sample,
        before,
    })
}

struct Visitor<F> {
    consume: F,
}

impl<F> Target for Visitor<F>
where
    F: FnMut(&Endpoint, persistence::Result<&Sample>) -> persistence::Result<()>,
{
    type Error = Refused;

    fn derives(&self, _endpoint: &Endpoint) -> bool {
        true
    }

    fn consume(&mut self, captured: Captured<'_>) -> Result<Progress, Refused> {
        let sample = captured
            .sample
            .unwrap_or_else(|| unreachable!("derived sample"));
        (self.consume)(&captured.endpoint, sample.map_err(AggregateError::legacy))?;
        Ok(Progress::Accepted(captured.before))
    }
}
