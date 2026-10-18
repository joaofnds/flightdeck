use std::io;

use crate::clock::Timebase;
use crate::processes::{self, Process, Unreadable};
use crate::push_without_growing;
use crate::ring::{
    IdentityAvailability, IdentityObservation, IdentityUnavailable, ProcessIdentity,
    UsageAvailability, Writer,
};

use super::{Acquisition, acquire, split_program};

mod availability;
use availability::{failure_reason, stable_creation};

pub struct Sweep {
    observations: Vec<IdentityObservation>,
    scratch: [u8; 4096],
    diagnostics: Diagnostics,
}

#[derive(Default)]
/// Counts describe the last sweep; raw failures use fixed caller-owned storage.
pub struct Diagnostics {
    pub measured: u64,
    pub refused: u64,
    pub gone: u64,
    pub unsupported: u64,
    pub changed: u64,
    pub invalid: u64,
    pub exhausted: u64,
    pub readiness_unavailable: u64,
    /// Each unexpected creation or path syscall, rather than the number of affected PIDs.
    pub unexpected_syscalls: u64,
    pub last_failure: Option<Failure>,
}

pub struct Failure {
    pub pid: i32,
    pub acquired: Acquisition,
    pub dictionary: Option<IdentityAvailability>,
}

impl Sweep {
    #[must_use]
    pub fn new(process_limit: usize) -> Self {
        assert!(process_limit > 0);

        Self {
            observations: Vec::with_capacity(process_limit),
            scratch: [0; 4096],
            diagnostics: Diagnostics::default(),
        }
    }

    pub fn run(
        &mut self,
        timebase: Timebase,
        numeric: &mut processes::Sweep,
        pids: &mut Vec<i32>,
        writer: &mut Writer,
    ) -> io::Result<()> {
        self.run_with(numeric, pids, writer, |pid, scratch, output| {
            let mut reading = None;
            acquire(pid, false, scratch, output, || {
                reading = Some(processes::read(timebase, pid));
            });

            match reading {
                Some(reading) => reading,
                None => unreachable!("acquire always reads numeric data once"),
            }
        })
    }

    /// Acquires at discovery, before the caller loses the readable process path.
    pub fn capture(
        &mut self,
        observation: &mut IdentityObservation,
        writer: &mut Writer,
        reading: impl FnOnce(),
    ) {
        let mut acquired = Acquisition::EMPTY;
        acquire(
            observation.pid,
            false,
            &mut self.scratch,
            &mut acquired,
            reading,
        );

        self.diagnostics.readiness_unavailable += u64::from(acquired.readiness.is_err());
        self.observe(observation, &mut acquired, writer);
    }

    fn run_with(
        &mut self,
        numeric: &mut processes::Sweep,
        pids: &mut Vec<i32>,
        writer: &mut Writer,
        mut read: impl FnMut(
            i32,
            &mut [u8; 4096],
            &mut Acquisition,
        ) -> io::Result<Result<Process, Unreadable>>,
    ) -> io::Result<()> {
        assert!(pids.len() <= self.observations.capacity());
        assert!(pids.len() <= numeric.readings.capacity());

        pids.sort_unstable();
        pids.dedup();

        self.clear();
        numeric.clear();

        for &pid in pids.iter() {
            let mut acquired = Acquisition::EMPTY;
            let reading = read(pid, &mut self.scratch, &mut acquired)?;
            self.diagnostics.readiness_unavailable += u64::from(acquired.readiness.is_err());
            let usage = match &reading {
                Ok(_) => UsageAvailability::Measured,
                Err(Unreadable::Refused) => UsageAvailability::Refused,
                Err(Unreadable::Gone) => UsageAvailability::Gone,
            };

            numeric.push_reading(pid, reading);

            let mut observation = IdentityObservation {
                pid,
                usage,
                ..IdentityObservation::EMPTY
            };
            self.observe(&mut observation, &mut acquired, writer);
            push_without_growing(&mut self.observations, observation);
        }

        assert_eq!(self.observations.len(), pids.len());
        assert_eq!(self.diagnostics.count(), crate::u64_from_usize(pids.len()));
        assert_eq!(
            numeric.readings.len() + crate::usize_from_u32(numeric.refused + numeric.gone),
            pids.len()
        );

        Ok(())
    }

    fn observe(
        &mut self,
        observation: &mut IdentityObservation,
        acquired: &mut Acquisition,
        writer: &mut Writer,
    ) {
        let creation = stable_creation(acquired);
        let mut dictionary = None;
        if acquired.candidate {
            assert!(creation.is_some());
            let Some(Ok(length)) = acquired.path else {
                unreachable!("a candidate has a validated path");
            };

            let mut program = None;
            match split_program(&self.scratch[..length], &mut program) {
                Ok(()) => {
                    let Some(bytes) = program else {
                        unreachable!("a split path supplies its program");
                    };

                    let mut reference = None;
                    match writer.intern_program(&bytes, &mut reference) {
                        Ok(()) => {
                            self.diagnostics.measured += 1;
                            observation.identity = ProcessIdentity::Measured {
                                creation: match creation {
                                    Some(creation) => creation,
                                    None => unreachable!("a candidate has a stable creation"),
                                },
                                program: match reference {
                                    Some(reference) => reference,
                                    None => unreachable!("intern supplies its reference"),
                                },
                            };
                            return;
                        }
                        Err(reason) => dictionary = Some(reason),
                    }
                }
                Err(error) => acquired.path = Some(Err(error)),
            }
        }

        let reason = match dictionary {
            Some(reason) => availability::dictionary_reason(reason),
            None => failure_reason(acquired),
        };
        self.diagnostics
            .record(observation.pid, acquired, dictionary, reason);

        observation.identity = ProcessIdentity::Unavailable { creation, reason };
    }

    pub(crate) fn clear(&mut self) {
        self.observations.clear();
        self.diagnostics = Diagnostics::default();
    }

    #[must_use]
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    #[must_use]
    pub fn observations(&self) -> &[IdentityObservation] {
        &self.observations
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.observations.capacity()
    }
}

#[cfg(test)]
mod tests;

impl Diagnostics {
    fn count(&self) -> u64 {
        self.measured
            + self.refused
            + self.gone
            + self.unsupported
            + self.changed
            + self.invalid
            + self.exhausted
    }

    fn record(
        &mut self,
        pid: i32,
        acquired: &mut Acquisition,
        dictionary: Option<IdentityAvailability>,
        reason: IdentityUnavailable,
    ) {
        match reason {
            IdentityUnavailable::Refused => self.refused += 1,
            IdentityUnavailable::Gone => self.gone += 1,
            IdentityUnavailable::Unsupported => self.unsupported += 1,
            IdentityUnavailable::Changed => self.changed += 1,
            IdentityUnavailable::Invalid => self.invalid += 1,
            IdentityUnavailable::Exhausted => self.exhausted += 1,
        }

        self.unexpected_syscalls += availability::unexpected_syscalls(acquired);

        self.last_failure = Some(Failure {
            pid,
            acquired: std::mem::replace(acquired, Acquisition::EMPTY),
            dictionary,
        });
    }
}
