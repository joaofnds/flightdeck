use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use persistence::{Error, Result};

/// Independent from the serial session owner, so a superseding request can interrupt it.
#[derive(Default)]
pub struct Cancellation {
    epoch: AtomicU64,
    interrupt: Mutex<Option<Arc<duckdb::InterruptHandle>>>,
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
            handle.interrupt();
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
        // DuckDB clears an interrupt at query startup. Keep a single cancellation durable
        // across that boundary; no polling thread survives a completed viewing operation.
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
                    interrupt.interrupt();
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

    pub fn attach(&self, connection: &duckdb::Connection) -> Result<()> {
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
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn one_cancellation_survives_engine_startup_for_prepared_and_count_queries() {
        // Goal: a single supersession stays effective when DuckDB clears its startup flag.
        // Method: cancel after preparation/checkpoint, then release real engine execution.
        for prepared in [true, false] {
            let cancellation = Arc::new(Cancellation::default());
            let control = Arc::clone(&cancellation);
            let (started, ready) = mpsc::sync_channel(1);
            let (proceed, permitted) = mpsc::sync_channel(1);
            let (finished, result) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let connection = crate::data::connection().unwrap();
                let operation = control.operation(0).unwrap();
                operation.attach(&connection).unwrap();
                let sql = "SELECT count(*) FROM range(1000000000000000000) t(i) WHERE i % 7 = 0";
                let outcome = operation.run(|| {
                    let mut statement = connection.prepare(sql)?;
                    operation.check()?;
                    started.send(()).unwrap();
                    permitted.recv_timeout(Duration::from_secs(5)).unwrap();
                    if prepared {
                        statement.query_row([], |row| row.get::<_, i64>(0))?;
                    } else {
                        connection.query_row(sql, [], |row| row.get::<_, i64>(0))?;
                    }
                    Ok(())
                });
                let answer: i32 = connection
                    .query_row("SELECT 42", [], |row| row.get(0))
                    .unwrap();
                let error = outcome.unwrap_err().to_string().to_ascii_lowercase();
                finished.send((error, answer)).unwrap();
            });
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            cancellation.cancel().unwrap();
            proceed.send(()).unwrap();
            let (error, answer) = result.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(error.contains("interrupt"), "unexpected error: {error}");
            assert_eq!(answer, 42);
            worker.join().unwrap();
        }
    }
}
