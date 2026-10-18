use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use persistence::engine::{Engine, InterruptHandle};
use persistence::{Error, Result};

/// Independent from the serial session owner, so a superseding request can interrupt it.
#[derive(Default)]
pub struct Cancellation {
    epoch: AtomicU64,
    interrupt: Mutex<Option<Arc<InterruptHandle>>>,
    wake: Condvar,
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
        let handle = self.operation.cancellation.interrupt.lock();
        self.finished.store(true, Ordering::Release);
        self.operation.cancellation.wake.notify_all();
        if let Err(error) = &handle {
            eprintln!("flightdeck query completion: {error}");
        }
    }
}

impl Cancellation {
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
        let handle = self
            .interrupt
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        if let Some(handle) = &*handle {
            handle.interrupt()?;
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
        let mut handle = self
            .cancellation
            .interrupt
            .lock()
            .map_err(|error| poisoned(&error))?;
        // Keep cancellation effective while a native query is being installed. Polars can
        // finish a kernel after cancellation, so run also checks the epoch before returning.
        for _attempt in 0..10_000 {
            if finished.load(Ordering::Acquire) {
                return Ok(());
            }
            if self.cancellation.epoch() == self.epoch {
                handle = self
                    .cancellation
                    .wake
                    .wait(handle)
                    .map_err(|error| poisoned(&error))?;
            } else {
                if let Some(interrupt) = &*handle {
                    interrupt.interrupt()?;
                }
                let (next, _) = self
                    .cancellation
                    .wake
                    .wait_timeout(handle, Duration::from_millis(1))
                    .map_err(|error| poisoned(&error))?;
                handle = next;
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

    pub fn attach(&self, connection: &Engine) -> Result<()> {
        let mut handle = self
            .cancellation
            .interrupt
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        *handle = Some(connection.interrupt_handle());
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
    use std::time::Duration;

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
            let permitted = std::sync::Mutex::new(permitted);
            let plan = interruptible_plan(move || {
                started.send(()).unwrap();
                let permitted = permitted.lock().unwrap();
                permitted.recv_timeout(Duration::from_secs(5)).unwrap();
            });

            let outcome = operation.run(|| {
                native = interrupted_natively(&connection.collect(plan));
                Ok(())
            });

            finished.send((superseded(&outcome), native)).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();

        cancellation.cancel().unwrap();
        proceed.send(()).unwrap();

        assert_eq!(
            result.recv_timeout(Duration::from_secs(5)).unwrap(),
            (true, true)
        );
        worker.join().unwrap();
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
