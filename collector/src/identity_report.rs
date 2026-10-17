use std::io::{self, Write};

use collector::programs::{
    Acquisition,
    sweep::{Diagnostics, Failure},
};
use collector::ring::Writer;

#[derive(Default)]
pub(crate) struct IdentityReport {
    counts: [u64; 9],
    last_failure: Option<Failure>,
}

impl IdentityReport {
    pub(crate) fn add(&mut self, diagnostics: &Diagnostics) {
        let added = [
            diagnostics.measured,
            diagnostics.refused,
            diagnostics.gone,
            diagnostics.unsupported,
            diagnostics.changed,
            diagnostics.invalid,
            diagnostics.exhausted,
            diagnostics.readiness_unavailable,
            diagnostics.unexpected_syscalls,
        ];

        for (count, added) in self.counts.iter_mut().zip(added) {
            *count += added;
        }

        if let Some(failure) = &diagnostics.last_failure {
            self.last_failure = Some(Failure {
                pid: failure.pid,
                dictionary: failure.dictionary,
                acquired: Acquisition {
                    creation_before: failure.acquired.creation_before,
                    creation_after: failure.acquired.creation_after,
                    readiness: failure.acquired.readiness,
                    path: failure.acquired.path,
                    candidate: failure.acquired.candidate,
                },
            });
        }
    }

    pub(crate) fn report(
        &mut self,
        ring: &Writer,
        label: &'static str,
        out: &mut impl Write,
    ) -> io::Result<()> {
        if let Some(usage) = ring.dictionary_usage()? {
            let observations = self.counts[..7].iter().sum::<u64>();

            writeln!(
                out,
                "identity {label} observations={observations} measured={} refused={} \
                gone={} unsupported_source_or_gate={} changed={} invalid={} exhausted={} \
                gate_unavailable={} unexpected_syscalls={} \
                dictionary_entries={} dictionary_bytes={}",
                self.counts[0],
                self.counts[1],
                self.counts[2],
                self.counts[3],
                self.counts[4],
                self.counts[5],
                self.counts[6],
                self.counts[7],
                self.counts[8],
                usage.entries,
                usage.arena_bytes
            )?;
            self.report_failure(label, out)?;
        } else {
            writeln!(
                out,
                "identity {label} unavailable source=unregistered-legacy"
            )?;
        }

        self.counts.fill(0);
        self.last_failure = None;

        Ok(())
    }

    fn report_failure(&self, label: &'static str, out: &mut impl Write) -> io::Result<()> {
        if let Some(failure) = &self.last_failure {
            writeln!(
                out,
                "identity {label} last_failure_pid={} creation_before={:?} \
                readiness={:?} path={:?} creation_after={:?} candidate={} dictionary={:?}",
                failure.pid,
                failure.acquired.creation_before,
                failure.acquired.readiness,
                failure.acquired.path,
                failure.acquired.creation_after,
                failure.acquired.candidate,
                failure.dictionary
            )?;
        }

        Ok(())
    }
}
