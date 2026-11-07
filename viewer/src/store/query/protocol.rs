//! One replaceable request and one replaceable answer, separate from the UI View mailbox.
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::Thread;

use collector::ring::RingWaker;

use super::{ByteBudget, CheckedCoverage, Held, QueryOutcome, QueryUnavailable, SourceGeneration};

#[derive(Clone)]
pub struct QueryHandle {
    shared: Arc<Shared>,
    thread: Thread,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryRequest {
    pub epoch: u64,
    pub source: Option<SourceGeneration>,
    pub selection: QuerySelection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuerySelection {
    FollowLatest,
    Second {
        second: u64,
        alternative: u32,
    },
    Exact {
        observation: crate::live_app_response::Observation,
        context_last_second: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryToken {
    pub epoch: u64,
    pub sequence: u64,
}

#[derive(Debug)]
pub struct QueryReply {
    pub token: QueryToken,
    pub outcome: QueryOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuerySubmitError {
    SequenceExhausted,
    Closed,
}

pub(crate) struct Queries {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    sequence: AtomicU64,
    closed: AtomicBool,
    contexts: Arc<AtomicUsize>,
}

#[derive(Default)]
struct State {
    request: Option<(QueryToken, QueryRequest)>,
    in_flight: Option<QueryToken>,
    reply: Option<QueryReply>,
    replied: Option<Arc<dyn Fn() + Send + Sync>>,
    ring_waker: Option<Arc<RingWaker>>,
}

impl Queries {
    /// Leaves where a consumer can reach it what ends the Store's wait on its ring.
    pub(crate) fn set_ring_waker(&self, ring_waker: Option<Arc<RingWaker>>) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ring_waker = ring_waker;
    }

    pub(crate) fn requested(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .request
            .is_some()
    }

    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                sequence: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                contexts: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    pub(crate) fn handle(&self, thread: Thread) -> QueryHandle {
        QueryHandle {
            shared: Arc::clone(&self.shared),
            thread,
        }
    }

    pub(crate) fn serve(
        &self,
        held: &Held,
        coverage: Option<&CheckedCoverage>,
        budget: &Arc<ByteBudget>,
    ) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let request = state.request.take();
        let Some((token, request)) = request else {
            return;
        };
        assert!(state.in_flight.is_none());
        state.in_flight = Some(token);
        drop(state);
        let outcome = super::build(
            held,
            coverage,
            &request,
            budget,
            &self.shared.contexts,
            || {
                if self.shared.closed.load(Ordering::Acquire) {
                    return false;
                }

                self.shared.sequence.load(Ordering::Acquire) == token.sequence
            },
        );
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.in_flight.take(), Some(token));
        if !self.shared.closed.load(Ordering::Acquire) {
            if self.shared.sequence.load(Ordering::Acquire) == token.sequence {
                assert!(state.reply.is_none());
                let Some(outcome) = outcome else {
                    unreachable!("only supersession or producer closure cancels a build");
                };
                state.reply = Some(QueryReply { token, outcome });
                announce_reply(state);
            }
        }
    }

    pub(crate) fn close(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.shared.closed.swap(true, Ordering::AcqRel) {
            return;
        }

        let token = state
            .request
            .take()
            .map(|(token, _request)| token)
            .or(state.in_flight)
            .or_else(|| state.reply.as_ref().map(|reply| reply.token));
        state.reply = None;
        if let Some(token) = token {
            if self.shared.sequence.load(Ordering::Acquire) == token.sequence {
                state.reply = Some(QueryReply {
                    token,
                    outcome: QueryOutcome::Unavailable(QueryUnavailable::Closed),
                });
                announce_reply(state);
            }
        }
    }
}

/// Tells the consumer an answer landed, with the lock let go, since the consumer's call takes a
/// lock of its own under which it reads whether an answer waits.
fn announce_reply(state: MutexGuard<'_, State>) {
    let replied = state.replied.clone();
    drop(state);
    if let Some(replied) = replied {
        replied();
    }
}

impl QueryHandle {
    /// Replaces work not yet started and drops any answer the consumer did not take.
    pub fn submit(&self, request: QueryRequest) -> Result<QueryToken, QuerySubmitError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(QuerySubmitError::Closed);
        }
        let sequence = self
            .shared
            .sequence
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(QuerySubmitError::SequenceExhausted)?;
        self.shared.sequence.store(sequence, Ordering::Release);
        let token = QueryToken {
            epoch: request.epoch,
            sequence,
        };
        state.reply = None;
        state.request = Some((token, request));
        drop(state);
        self.thread.unpark();

        Ok(token)
    }

    /// Ends the Store's wait on its ring, for a request whose page shows nothing until answered.
    /// A refusal leaves the request to the end of that wait. The wake reaches every reader of the
    /// ring, so a request that only refreshes a page leaves the Store to its own pace.
    pub fn wake_store(&self) {
        let ring_waker = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ring_waker
            .clone();
        if let Some(ring_waker) = ring_waker {
            if let Err(errno) = ring_waker.wake() {
                eprintln!("flightdeck Store wake: errno {errno}");
            }
        }
    }

    /// Cancels pending and in-flight work while leaving contexts already displayed usable.
    pub fn cancel(&self) -> Result<(), QuerySubmitError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(QuerySubmitError::Closed);
        }
        let sequence = self
            .shared
            .sequence
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(QuerySubmitError::SequenceExhausted)?;
        self.shared.sequence.store(sequence, Ordering::Release);
        state.request = None;
        state.reply = None;

        Ok(())
    }

    #[must_use]
    pub fn pending(&self) -> bool {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.request.is_some() {
            return true;
        }
        if state.in_flight.is_some() {
            return true;
        }

        state.reply.is_some()
    }

    /// Calls `replied` on the Store's thread each time an answer lands, in place of any earlier
    /// call, so a consumer waiting on a condition of its own can end that wait.
    pub fn on_reply(&self, replied: impl Fn() + Send + Sync + 'static) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replied = Some(Arc::new(replied));
    }

    /// Whether an answer waits to be taken.
    #[must_use]
    pub fn answered(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reply
            .is_some()
    }

    #[must_use]
    pub fn take(&self) -> Option<QueryReply> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reply
            .take()
    }

    #[must_use]
    pub fn contexts_held(&self) -> usize {
        self.shared.contexts.load(Ordering::Acquire)
    }
}

impl Drop for Queries {
    fn drop(&mut self) {
        self.close();
    }
}
