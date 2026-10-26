use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use persistence::engine::{Engine, InterruptHandle};
use persistence::{Error, Result};

/// The engines whose queries one cancellation interrupts, most recently attached last. A live
/// session reads on its own engine and its session's, and a context on several more at once,
/// so twice as many leaves room for the engines a reopen brings. Interrupting an engine with no
/// query running does nothing, so an engine kept after its session closed costs nothing.
const INTERRUPTS_MAX: usize = 16;

/// Independent from the serial session owner, so a superseding request can interrupt it.
#[derive(Default)]
pub struct Cancellation {
    epoch: AtomicU64,
    interrupts: Mutex<Vec<Arc<InterruptHandle>>>,
    wake: Condvar,
    #[cfg(test)]
    page_row_interrupt: AtomicBool,
}

pub(crate) struct Operation {
    cancellation: Arc<Cancellation>,
    epoch: u64,
}

struct Completion<'a> {
    operation: &'a Operation,
    finished: &'a AtomicBool,
}

impl Drop for Completion<'_> {
    fn drop(&mut self) {
        // Pair completion with the waiter's mutex so notification cannot precede its wait.
        let handle = self.operation.cancellation.interrupts.lock();
        self.finished.store(true, Ordering::Release);
        self.operation.cancellation.wake.notify_all();
        if let Err(error) = &handle {
            eprintln!("flightdeck query completion: {error}");
        }
    }
}

impl Cancellation {
    #[cfg(test)]
    pub(crate) fn interrupt_after_page_row(&self) {
        self.page_row_interrupt.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub fn cancel(&self) -> Result<()> {
        self.epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .map_err(|_| Error::Bound("cancellation epoch"))?;
        let interrupts = self
            .interrupts
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        for interrupt in interrupts.iter() {
            interrupt.interrupt()?;
        }
        self.wake.notify_all();
        Ok(())
    }

    pub(crate) fn operation(self: Arc<Self>, epoch: u64) -> Result<Operation> {
        let operation = Operation {
            cancellation: self,
            epoch,
        };
        operation.check()?;
        Ok(operation)
    }
}

impl Operation {
    #[cfg(test)]
    pub(crate) fn page_row(&self) -> Result<()> {
        if self
            .cancellation
            .page_row_interrupt
            .swap(false, Ordering::AcqRel)
        {
            self.cancellation.cancel()?;
        }
        Ok(())
    }

    pub fn run<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        self.check()?;
        let finished = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let completion = Completion {
                operation: self,
                finished: &finished,
            };
            let watcher = std::thread::Builder::new()
                .name("flightdeck-query-cancel".to_owned())
                .stack_size(128 * 1024)
                .spawn_scoped(scope, || self.watch(&finished))?;
            let result = action();
            drop(completion);
            watcher
                .join()
                .map_err(|_| Error::Bound("query monitor panic"))??;
            self.check()?;
            result
        })
    }

    fn watch(&self, finished: &AtomicBool) -> Result<()> {
        let mut interrupts = self
            .cancellation
            .interrupts
            .lock()
            .map_err(|error| poisoned(&error))?;
        // Keep cancellation effective while a native query is being installed. Polars can
        // finish a kernel after cancellation, so run also checks the epoch before returning.
        for _attempt in 0..10_000 {
            if finished.load(Ordering::Acquire) {
                return Ok(());
            }
            if self.cancellation.epoch() == self.epoch {
                interrupts = self
                    .cancellation
                    .wake
                    .wait(interrupts)
                    .map_err(|error| poisoned(&error))?;
            } else {
                for interrupt in interrupts.iter() {
                    interrupt.interrupt()?;
                }
                let (next, _) = self
                    .cancellation
                    .wake
                    .wait_timeout(interrupts, Duration::from_millis(1))
                    .map_err(|error| poisoned(&error))?;
                interrupts = next;
            }
        }
        Err(Error::Bound("query cancellation retries"))
    }

    pub fn check(&self) -> Result<()> {
        if self.cancellation.epoch() != self.epoch {
            return Err(io::Error::from(io::ErrorKind::Interrupted).into());
        }
        Ok(())
    }

    pub fn begin(&self, epoch: u64) -> Result<Self> {
        Arc::clone(&self.cancellation).operation(epoch)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Adds `connection` to the engines a cancel interrupts, dropping the least recently
    /// attached once `INTERRUPTS_MAX` are.
    pub fn attach(&self, connection: &Engine) -> Result<()> {
        let mut interrupts = self
            .cancellation
            .interrupts
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let attached = connection.interrupt_handle();
        interrupts.retain(|interrupt| !Arc::ptr_eq(interrupt, &attached));
        if interrupts.len() == INTERRUPTS_MAX {
            interrupts.remove(0);
        }
        interrupts.push(attached);
        assert!(interrupts.len() <= INTERRUPTS_MAX);
        drop(interrupts);
        self.check()
    }
}

fn poisoned<T>(error: &std::sync::PoisonError<T>) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::Cancellation;
    use polars::prelude::{DataFrame, LazyFrame};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn one_cancellation_survives_native_query_startup() {
        // Goal: a cancellation landing before a native query starts still interrupts that query.
        // Method: cancel once before collecting native work lasting 250 ms, then reuse the engine.
        let cancellation = Arc::new(Cancellation::default());
        let control = Arc::clone(&cancellation);
        let (started, ready) = mpsc::sync_channel(1);
        let (proceed, permitted) = mpsc::sync_channel(1);
        let (finished, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let connection = crate::data::connection();
            let operation = control.operation(0).unwrap();
            operation.attach(&connection).unwrap();
            let mut native = false;

            let outcome = operation.run(|| {
                started.send(()).unwrap();
                permitted.recv_timeout(Duration::from_secs(5)).unwrap();
                let plan = interruptible_plan(|| std::thread::sleep(Duration::from_millis(250)));
                native = interrupted_natively(&connection.collect(plan));
                Ok(())
            });

            finished
                .send((superseded(&outcome), native, answer(&connection)))
                .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();

        cancellation.cancel().unwrap();
        proceed.send(()).unwrap();

        let outcome = result.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(outcome, (true, true, 42));
        worker.join().unwrap();
    }

    #[test]
    fn refuses_a_superseded_answer_after_native_execution_finishes() {
        // Goal: a kernel finishing before cancellation cannot bypass the result epoch check.
        // Method: pause after real native execution, cancel once, then release result return.
        let cancellation = Arc::new(Cancellation::default());
        let control = Arc::clone(&cancellation);
        let (started, ready) = mpsc::sync_channel(1);
        let (proceed, permitted) = mpsc::sync_channel(1);
        let (finished, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let connection = crate::data::connection();
            let operation = control.operation(0).unwrap();
            operation.attach(&connection).unwrap();

            let outcome = operation.run(|| {
                connection.query("SELECT 42")?;
                started.send(()).unwrap();
                permitted.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            });

            finished
                .send((superseded(&outcome), answer(&connection)))
                .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();

        cancellation.cancel().unwrap();
        proceed.send(()).unwrap();

        assert_eq!(
            result.recv_timeout(Duration::from_secs(5)).unwrap(),
            (true, 42)
        );
        worker.join().unwrap();
    }

    #[test]
    fn interrupts_a_superseded_native_query_that_has_started() {
        // Goal: cancelling during native execution interrupts the query and returns no answer.
        // Method: gate native execution, cancel once inside it, then release the gate.
        assert_eq!(cancel_inside_native_query(0), (true, true));
    }

    #[test]
    fn interrupts_a_started_query_on_an_engine_attached_before_another() {
        // Goal: a cancellation interrupts the query of every engine its operation attached, since
        // a context reads on several engines at once and each attaches its own.
        // Method: attach a second engine after the querying one, then cancel inside native
        // execution on the first.
        assert_eq!(cancel_inside_native_query(1), (true, true));
    }

    /// Whether the operation was superseded, and whether its query was interrupted natively,
    /// when a cancel lands inside a query on an engine attached before `engines_after` others.
    fn cancel_inside_native_query(engines_after: usize) -> (bool, bool) {
        let cancellation = Arc::new(Cancellation::default());
        let control = Arc::clone(&cancellation);
        let (started, ready) = mpsc::sync_channel(1);
        let (proceed, permitted) = mpsc::sync_channel(1);
        let (finished, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let connection = crate::data::connection();
            let others: Vec<_> = (0..engines_after)
                .map(|_| crate::data::connection())
                .collect();
            let operation = control.operation(0).unwrap();
            operation.attach(&connection).unwrap();
            for other in &others {
                operation.attach(other).unwrap();
            }
            let mut native = false;
            let permitted = std::sync::Mutex::new(permitted);
            let interrupt = connection.interrupt_handle();
            let plan = interruptible_plan(move || {
                started.send(Arc::clone(&interrupt)).unwrap();
                let permitted = permitted.lock().unwrap();
                permitted.recv_timeout(Duration::from_secs(5)).unwrap();
            });

            let outcome = operation.run(|| {
                native = interrupted_natively(&connection.collect(plan));
                Ok(())
            });

            finished.send((superseded(&outcome), native)).unwrap();
        });
        let interrupt = ready.recv_timeout(Duration::from_secs(5)).unwrap();
        cancel_inside_native_query_wait_for_installation(&interrupt);

        cancellation.cancel().unwrap();
        proceed.send(()).unwrap();

        let outcome = result.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();

        outcome
    }

    fn cancel_inside_native_query_wait_for_installation(
        interrupt: &persistence::engine::InterruptHandle,
    ) {
        // Polars starts the gated work before the engine holds the query, and a cancel in that
        // gap reaches the query only if it is still running once the engine holds it.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(1) {
            if interrupt.running().unwrap() {
                return;
            }
            std::thread::yield_now();
        }
    }

    fn superseded(outcome: &persistence::Result<()>) -> bool {
        matches!(outcome,
            Err(persistence::Error::Io(error))
            if error.kind() == std::io::ErrorKind::Interrupted)
    }

    fn interrupted_natively(outcome: &persistence::Result<DataFrame>) -> bool {
        matches!(outcome,
            Err(persistence::Error::Engine(polars::error::PolarsError::ComputeError(message)))
            if message.as_ref() == "query interrupted")
    }

    fn answer(connection: &persistence::engine::Engine) -> i32 {
        let answer = connection.query("SELECT 42").unwrap();
        persistence::engine::Row::new(&answer, 0)
            .unwrap()
            .get(0)
            .unwrap()
    }

    fn interruptible_plan(gate: impl Fn() + Send + Sync + 'static) -> LazyFrame {
        // The gate runs inside native execution. The second part starts a new executor,
        // which observes an interruption delivered while the gate held execution.
        use polars::prelude::{
            AllowedOptimizations, Column, IntoLazy, UnionArgs, col, concat, lit,
        };
        let first = DataFrame::new(1, vec![Column::new("value".into(), [11u64])])
            .unwrap()
            .lazy()
            .map(
                move |frame| {
                    gate();
                    Ok(frame)
                },
                AllowedOptimizations::empty(),
                None,
                Some("cancellation_gate"),
            );
        let second = DataFrame::new(1, vec![Column::new("value".into(), [17u64])])
            .unwrap()
            .lazy()
            .select([(col("value") + lit(1u64)).alias("value")]);

        concat(
            [first, second],
            UnionArgs {
                parallel: false,
                rechunk: false,
                to_supertypes: false,
                diagonal: false,
                strict: true,
                from_partitioned_ds: false,
                maintain_order: true,
            },
        )
        .unwrap()
        .without_optimizations()
    }
}
