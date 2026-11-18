//! A store a test takes seconds into one at a time, of the runs and boots it names, and the
//! contexts the store answers of them.
use std::path::PathBuf;
use std::sync::Arc;

use collector::ring::{Identity, RunIdentity};

use super::apps::TickApps;
use super::packing::PackingEnd;
use super::query::{CheckedCoverage, Queries};
use super::{
    ByteBudget, CompleteMachine, CompleteMachineState, Held, MachineSecond, Missing, QueryContext,
    QueryHandle, QueryOutcome, QueryRequest, QuerySelection, SourceGeneration,
};
use crate::Selection;
use crate::session::ContextRange;

const NS: u64 = 1_000_000_000;
const RECORDING: Identity = Identity([1, 1]);
/// The wall second of the oldest point of the chart `charted` holds.
pub const FIRST_SECOND: u64 = 1_000;

pub struct Charted {
    held: Held,
    /// The packing thread's end of the store's queues, which no thread runs, kept so the
    /// store hands its blocks over as it does while the packing thread lives.
    _packing: PackingEnd,
    budget: Arc<ByteBudget>,
    /// The budget the contexts the store answers and their readings are charged to, apart from
    /// the store's own, so a test reads what the readings alone hold.
    responses: Arc<ByteBudget>,
    /// The queues of the store's queries, which no thread answers, kept so a reader's query
    /// waits as it does on a store that has yet to answer it.
    queries: Queries,
    run: RunIdentity,
    publication: u64,
    run_tick: u64,
}

impl Charted {
    #[must_use]
    pub fn new() -> Self {
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let (store_end, packing) = super::packing::pair(
            Arc::clone(&budget),
            super::payload::packed::APP_ROWS_PACKED_SHARE,
        );
        let mut held = Held::new(PathBuf::new(), Arc::clone(&budget), store_end);
        held.ring_complete = true;

        Self {
            held,
            _packing: packing,
            budget,
            responses: Arc::new(ByteBudget::new(256 * 1024 * 1024)),
            queries: Queries::new(),
            run: RunIdentity {
                recording: RECORDING,
                run: Identity([2, 2]),
                boot: Identity([3, 3]),
            },
            publication: 0,
            run_tick: 0,
        }
    }

    /// Takes in the run's next reading, at the wall second `second`, with a complete reading
    /// whose CPU share is the second's last two digits.
    pub fn take(&mut self, second: u64) {
        let machine = self.take_machine(second);
        let complete = CompleteMachine::of_cpu_percent(&machine, cpu_percent(second), &self.budget);

        self.take_reading(&MachineSecond {
            complete: CompleteMachineState::Available(complete),
            ..machine
        });
    }

    /// Takes in the run's next reading, at the wall second `second`, with no complete reading.
    pub fn take_incomplete(&mut self, second: u64) {
        let machine = self.take_machine(second);

        self.take_reading(&machine);
    }

    /// Misses the run's next tick, which leaves the run no reading of it.
    pub fn miss(&mut self) {
        self.run_tick += 1;
    }

    /// Starts the next run of the collector, on the same boot.
    pub fn restart(&mut self) {
        self.run.run.0[0] += 1;
        self.publication = 0;
        self.run_tick = 0;
    }

    /// Starts the next run of the collector on the next boot of the Mac.
    pub fn reboot(&mut self) {
        self.restart();
        self.run.boot.0[0] += 1;
    }

    /// Gives the held second at the wall second `second` a complete reading of another content,
    /// in a buffer of its own, as the store replaces a reading.
    pub fn replace(&mut self, second: u64, cpu_percent: f64) {
        let Some(index) = self
            .held
            .span
            .held()
            .position(|machine| machine.wall_ns == second * NS)
        else {
            panic!("the store holds no reading at the wall second {second}");
        };
        let Some(machine) = self.held.span.get(index) else {
            unreachable!("the store holds the reading it found at {index}");
        };
        let mut machine = machine.clone();
        let complete = CompleteMachine::of_cpu_percent(&machine, cpu_percent, &self.budget);
        machine.complete = CompleteMachineState::Available(complete);

        self.held
            .span
            .replace_held(index, |buffer| *buffer = machine);
    }

    /// The budget the contexts the store answers and their readings are charged to.
    #[must_use]
    pub fn responses(&self) -> Arc<ByteBudget> {
        Arc::clone(&self.responses)
    }

    /// The context that follows the newest second.
    #[must_use]
    pub fn followed(&self) -> Arc<QueryContext> {
        self.context(QuerySelection::FollowLatest)
    }

    /// The context fixed on the wall second `second`.
    #[must_use]
    pub fn fixed(&self, second: u64) -> Arc<QueryContext> {
        self.context(QuerySelection::Second {
            second,
            alternative: 0,
        })
    }

    /// A handle on the store's queries, which no thread answers, so a reader's query waits.
    #[must_use]
    pub fn handle(&self) -> QueryHandle {
        self.queries.handle(std::thread::current())
    }

    fn take_machine(&mut self, second: u64) -> MachineSecond {
        self.publication += 1;
        let machine = MachineSecond {
            publication: self.publication,
            run_tick: self.run_tick,
            wall_ns: second * NS,
            continuous_ns: Some(second * NS),
            ..MachineSecond::default()
        };
        self.run_tick += 1;

        machine
    }

    fn take_reading(&mut self, machine: &MachineSecond) {
        let unread = Missing::Unread;

        assert!(
            self.held
                .take(&self.run, machine, TickApps::Missing(&unread))
        );
    }

    fn context(&self, selection: QuerySelection) -> Arc<QueryContext> {
        let queries = Queries::new();
        let handle = queries.handle(std::thread::current());
        let submitted = handle.submit(QueryRequest {
            epoch: 0,
            source: None,
            selection,
        });
        if let Err(error) = submitted {
            panic!("the store refused a query of a context: {error:?}");
        }
        let coverage = CheckedCoverage::new(
            SourceGeneration {
                recording: RECORDING,
                generation: 0,
            },
            ContextRange {
                first_ns: 0,
                end_ns: 10_000_000 * NS,
            },
        );

        queries.serve(&self.held, Some(&coverage), &self.responses);

        let Some(reply) = handle.take() else {
            panic!("the store answered no query of a context");
        };
        match reply.outcome {
            QueryOutcome::Ready(context) => context,
            outcome => panic!("expected a context, found {outcome:?}"),
        }
    }
}

impl Default for Charted {
    fn default() -> Self {
        Self::new()
    }
}

/// A store holding one run's readings of a whole followed chart, its lead-in included, the
/// newest at `FIRST_SECOND` plus `CHARTED_SECONDS` less one.
#[must_use]
pub fn charted() -> Charted {
    let mut store = Charted::new();
    for second in FIRST_SECOND..FIRST_SECOND + crate::CHARTED_SECONDS {
        store.take(second);
    }

    store
}

/// The points of the chart `charted` holds, one for each second.
#[must_use]
pub fn charted_point_count() -> usize {
    usize::try_from(crate::CHARTED_SECONDS)
        .unwrap_or_else(|_| unreachable!("the seconds of a chart fit a usize"))
}

/// How many chart points of `later` share their allocation with a chart point of `earlier`.
#[must_use]
pub fn shared_count(earlier: &Selection, later: &Selection) -> usize {
    let (Some(earlier), Some(later)) = (earlier.trends(), later.trends()) else {
        panic!("a reading that shares chart points has a chart");
    };

    later
        .iter()
        .filter(|point| earlier.iter().any(|other| point.shares_point_with(other)))
        .count()
}

/// The CPU share of the complete reading `Charted::take` gives the wall second `second`.
fn cpu_percent(second: u64) -> f64 {
    let Ok(percent) = u32::try_from(second % 100) else {
        unreachable!("a remainder of a division by 100 is below 100");
    };

    f64::from(percent)
}
