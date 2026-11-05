//! Cost of the controller and both worker roles, including children reaped between samples.
use std::io::{self, Write};
use std::process::Child;

use collector::clock::Timebase;
use collector::recording::child::{ChildCost, ReapedChild, sample_process};
use collector::recording::control::Health;
use collector::ring::ProcessCreation;

#[derive(Default)]
pub struct WorkerCost {
    exited_ns: u64,
    active_ns: u64,
    footprint: u64,
    peak: u64,
    adopted: Option<ProcessCreation>,
    adopted_baseline: u64,
    incomplete: bool,
    interval_incomplete: bool,
    peak_incomplete: bool,
}

pub struct Accounting {
    timebase: Timebase,
    since: u64,
    since_continuous: u64,
    previous: [u64; 3],
}

struct ResourceClocks {
    awake_ns: u64,
    continuous_ns: u64,
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

impl ResourceClocks {
    fn read(timebase: Timebase) -> io::Result<Self> {
        Ok(Self {
            awake_ns: timebase.nanoseconds(unsafe { mach_absolute_time() })?,
            continuous_ns: timebase.nanoseconds(collector::clock::now())?,
        })
    }
}

impl WorkerCost {
    pub fn started(&mut self) {
        self.active_ns = 0;
        self.adopted = None;
        self.adopted_baseline = 0;
    }

    pub fn finished(&mut self, reaped: &ReapedChild) -> io::Result<()> {
        self.exited_ns = self
            .exited_ns
            .checked_add(reaped.finished.cpu_ns)
            .ok_or_else(invalid)?;
        match &reaped.footprint {
            Ok(sample) => self.peak = self.peak.max(sample.footprint_bytes_max),
            Err(error) => {
                self.peak_incomplete = true;
                collector::diagnostics::error(format_args!(
                    "worker final native footprint unavailable: {error}"
                ));
            }
        }
        self.active_ns = 0;
        self.footprint = 0;
        Ok(())
    }

    fn total(&self) -> io::Result<u64> {
        self.exited_ns
            .checked_add(self.active_ns)
            .ok_or_else(invalid)
    }

    fn sampled(&mut self, sample: &ChildCost) {
        self.active_ns = sample.cpu_ns.saturating_sub(self.adopted_baseline);
        self.footprint = sample.footprint_bytes;
        self.peak = self.peak.max(sample.footprint_bytes_max);
    }
}

impl Accounting {
    pub fn timebase(&self) -> Timebase {
        self.timebase
    }
    pub fn new() -> io::Result<Self> {
        let timebase = Timebase::read()?;
        let clocks = ResourceClocks::read(timebase)?;
        Ok(Self {
            timebase,
            since: clocks.awake_ns,
            since_continuous: clocks.continuous_ns,
            previous: [0; 3],
        })
    }

    pub fn observe(
        &self,
        cost: &mut WorkerCost,
        child: Option<&Child>,
        health: Option<&Health>,
    ) -> io::Result<()> {
        if let Some(child) = child {
            let pid = i32::try_from(child.id()).map_err(io::Error::other)?;
            match sample_process(pid, self.timebase) {
                Ok(sample) => cost.sampled(&sample),
                // A child may exit between this step's wait and sample. Its next wait4
                // supplies the final total, without discarding or double-counting this sample.
                Err(error) if error.raw_os_error() == Some(3) => {}
                Err(error) => return Err(error),
            }
        } else if let Some(health) = health {
            self.observe_adopted(cost, health)?;
        }
        Ok(())
    }

    fn observe_adopted(&self, cost: &mut WorkerCost, health: &Health) -> io::Result<()> {
        if health.alive()? {
            let sample = sample_process(health.pid, self.timebase);
            if !health.alive()? {
                cost.incomplete = true;
                cost.interval_incomplete = true;
                cost.peak_incomplete = true;
                return Ok(());
            }
            let sample = sample?;
            if cost.adopted != Some(health.creation) {
                cost.adopted = Some(health.creation);
                cost.adopted_baseline = sample.cpu_ns;
            }
            cost.sampled(&sample);
        } else if cost.adopted.take().is_some() {
            // Another process owns the final wait4 of an adopted worker. Preserve the
            // measured portion and explicitly refuse a complete total for this tenure.
            cost.exited_ns = cost.total()?;
            cost.active_ns = 0;
            cost.footprint = 0;
            cost.incomplete = true;
            cost.interval_incomplete = true;
            cost.peak_incomplete = true;
        }
        Ok(())
    }

    pub fn report_if_due(
        &mut self,
        collector: &mut WorkerCost,
        persister: &mut WorkerCost,
    ) -> io::Result<()> {
        let clocks = ResourceClocks::read(self.timebase)?;
        if self.due(&clocks)? {
            let mut out = collector::diagnostics::stdout_group();
            self.report_if_due_to(&clocks, collector, persister, &mut *out)?;
            out.submit();
        }
        Ok(())
    }

    fn due(&self, clocks: &ResourceClocks) -> io::Result<bool> {
        Ok(clocks
            .awake_ns
            .checked_sub(self.since)
            .ok_or_else(invalid)?
            >= 60_000_000_000)
    }

    fn report_if_due_to(
        &mut self,
        clocks: &ResourceClocks,
        collector: &mut WorkerCost,
        persister: &mut WorkerCost,
        out: &mut impl Write,
    ) -> io::Result<()> {
        if self.due(clocks)? {
            self.report_to_at("minute", clocks, collector, persister, out)?;
        }
        Ok(())
    }

    pub fn report(
        &mut self,
        label: &str,
        collector: &mut WorkerCost,
        persister: &mut WorkerCost,
    ) -> io::Result<()> {
        let mut out = collector::diagnostics::stdout_group();
        self.report_to(label, collector, persister, &mut *out)?;
        out.submit();
        Ok(())
    }

    fn report_to(
        &mut self,
        label: &str,
        collector: &mut WorkerCost,
        persister: &mut WorkerCost,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let clocks = ResourceClocks::read(self.timebase)?;
        self.report_to_at(label, &clocks, collector, persister, out)
    }

    fn report_to_at(
        &mut self,
        label: &str,
        clocks: &ResourceClocks,
        collector: &mut WorkerCost,
        persister: &mut WorkerCost,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let now = clocks.awake_ns;
        let elapsed = now.checked_sub(self.since).ok_or_else(invalid)?;
        if elapsed == 0 {
            return Err(invalid());
        }
        let continuous_elapsed = clocks
            .continuous_ns
            .checked_sub(self.since_continuous)
            .ok_or_else(invalid)?;
        let own = sample_process(collector::processes::own_pid()?, self.timebase)?;
        let totals = [own.cpu_ns, collector.total()?, persister.total()?];
        let mut deltas = [0; 3];
        for (index, total) in totals.iter().enumerate() {
            deltas[index] = total
                .checked_sub(self.previous[index])
                .ok_or_else(invalid)?;
        }
        report_interval(
            label,
            elapsed,
            continuous_elapsed,
            &deltas,
            [
                !collector.interval_incomplete,
                !persister.interval_incomplete,
            ],
            out,
        )?;
        let acquisition_footprint = own
            .footprint_bytes
            .checked_add(collector.footprint)
            .ok_or_else(invalid)?;
        writeln!(
            out,
            "recording cumulative recorder_cpu_ns={} collector_cpu_ns={} persister_cpu_ns={} \
             recorder_footprint_bytes={} recorder_footprint_bytes_max={} \
             collector_footprint_bytes={} \
             collector_footprint_bytes_max={} acquisition_footprint_bytes={acquisition_footprint} \
             current_footprint_bytes={} lifetime_footprint_bytes_max={} collector_cpu_complete={} \
             persister_cpu_complete={} collector_sampled_footprint_bytes_max={} \
             persister_sampled_footprint_bytes_max={}",
            totals[0],
            totals[1],
            totals[2],
            own.footprint_bytes,
            own.footprint_bytes_max,
            collector.footprint,
            Peak::of(collector),
            persister.footprint,
            Peak::of(persister),
            !collector.incomplete,
            !persister.incomplete,
            collector.peak,
            persister.peak
        )?;
        collector.interval_incomplete = false;
        persister.interval_incomplete = false;
        self.previous = totals;
        self.since = now;
        self.since_continuous = clocks.continuous_ns;
        Ok(())
    }
}

fn report_interval(
    label: &str,
    elapsed: u64,
    continuous_elapsed: u64,
    deltas: &[u64; 3],
    complete: [bool; 2],
    out: &mut impl Write,
) -> io::Result<()> {
    if complete.contains(&false) {
        writeln!(
            out,
            "recording {label} elapsed_ns={elapsed} clock=awake_ns \
             continuous_elapsed_ns={continuous_elapsed} recorder_cpu_ns={} collector_cpu_ns={} \
             persister_cpu_ns={} combined_cpu_ns=unavailable combined_percent=unavailable \
             reason=adopted_exit_unmeasured collector_cpu_complete={} persister_cpu_complete={}",
            deltas[0], deltas[1], deltas[2], complete[0], complete[1]
        )?;
    } else {
        let combined = deltas[0]
            .checked_add(deltas[1])
            .and_then(|value| value.checked_add(deltas[2]))
            .ok_or_else(invalid)?;
        let percent = std::time::Duration::from_nanos(combined)
            .div_duration_f64(std::time::Duration::from_nanos(elapsed))
            * 100.0;
        writeln!(
            out,
            "recording {label} elapsed_ns={elapsed} clock=awake_ns \
             continuous_elapsed_ns={continuous_elapsed} recorder_cpu_ns={} collector_cpu_ns={} \
             persister_cpu_ns={} combined_cpu_ns={combined} combined_percent={percent:.6} \
             collector_cpu_complete=true persister_cpu_complete=true",
            deltas[0], deltas[1], deltas[2]
        )?;
    }
    Ok(())
}

struct Peak(Option<u64>);

impl Peak {
    fn of(cost: &WorkerCost) -> Self {
        Self((!cost.peak_incomplete).then_some(cost.peak))
    }
}

impl std::fmt::Display for Peak {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(value) => value.fmt(formatter),
            None => formatter.write_str("unavailable"),
        }
    }
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_charges_controller_and_both_workers_over_awake_time() {
        // Goal: production reporting charges all three roles against the awake interval.
        // Method: render known role costs with a continuous-clock sleep gap.
        let mut output = Vec::new();

        report_interval(
            "minute",
            60_000_000_000,
            660_000_000_000,
            &[100_000_000, 600_000_000, 300_000_000],
            [true, true],
            &mut output,
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            concat!(
                "recording minute elapsed_ns=60000000000 clock=awake_ns ",
                "continuous_elapsed_ns=660000000000 recorder_cpu_ns=100000000 ",
                "collector_cpu_ns=600000000 persister_cpu_ns=300000000 ",
                "combined_cpu_ns=1000000000 combined_percent=1.666667 ",
                "collector_cpu_complete=true persister_cpu_complete=true\n"
            )
        );
    }

    #[test]
    fn interval_preserves_measured_parts_without_inventing_an_unmeasured_total() {
        // Goal: incomplete worker CPU cannot produce a plausible total or erase measured parts.
        // Method: render the actual recorder format with the persister interval incomplete.
        let mut output = Vec::new();

        report_interval(
            "minute",
            60_000_000_000,
            60_000_000_000,
            &[100_000_000, 600_000_000, 17],
            [true, false],
            &mut output,
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            concat!(
                "recording minute elapsed_ns=60000000000 clock=awake_ns ",
                "continuous_elapsed_ns=60000000000 recorder_cpu_ns=100000000 ",
                "collector_cpu_ns=600000000 persister_cpu_ns=17 ",
                "combined_cpu_ns=unavailable combined_percent=unavailable ",
                "reason=adopted_exit_unmeasured collector_cpu_complete=true ",
                "persister_cpu_complete=false\n"
            )
        );
    }

    #[test]
    fn an_unreadable_final_peak_remains_unavailable_with_its_sampled_lower_bound() {
        // Goal: a native final-sample refusal cannot become a measured zero lifetime peak.
        // Method: finish through production accounting, then inspect its actual output fields.
        let mut accounting = Accounting::new().unwrap();
        let mut collector = WorkerCost::default();
        let mut persister = WorkerCost {
            peak: 1234,
            ..WorkerCost::default()
        };
        persister
            .finished(&ReapedChild {
                finished: collector::recording::child::Finished {
                    cpu_ns: 17,
                    status: 0,
                    resident_bytes_max: 5678,
                },
                footprint: Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            })
            .unwrap();
        let mut output = Vec::new();
        accounting
            .report_to("tail", &mut collector, &mut persister, &mut output)
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("lifetime_footprint_bytes_max=unavailable "));
        assert!(output.contains("persister_sampled_footprint_bytes_max=1234"));
        assert!(output.contains("persister_cpu_complete=true"));
        assert!(output.contains("persister_cpu_ns=17"));
    }

    #[test]
    fn complete_later_intervals_recover_after_an_unmeasured_adopted_exit() {
        // Goal: one unavailable final wait4 cannot erase all later measured minute coverage.
        // Method: run the production reporter across the affected and next complete intervals.
        let mut accounting = Accounting::new().unwrap();
        let mut collector = WorkerCost {
            incomplete: true,
            interval_incomplete: true,
            exited_ns: 100,
            ..WorkerCost::default()
        };
        let mut persister = WorkerCost::default();
        let mut clocks = ResourceClocks {
            awake_ns: accounting.since + 60_000_000_000,
            continuous_ns: accounting.since_continuous + 60_000_000_000,
        };
        let mut affected = Vec::new();
        accounting
            .report_to_at(
                "minute",
                &clocks,
                &mut collector,
                &mut persister,
                &mut affected,
            )
            .unwrap();
        let affected = String::from_utf8(affected).unwrap();
        assert!(affected.contains("combined_percent=unavailable"));
        assert!(affected.contains("collector_cpu_complete=false"));
        clocks.awake_ns += 60_000_000_000;
        clocks.continuous_ns += 60_000_000_000;
        let mut recovered = Vec::new();
        accounting
            .report_to_at(
                "minute",
                &clocks,
                &mut collector,
                &mut persister,
                &mut recovered,
            )
            .unwrap();
        let recovered = String::from_utf8(recovered).unwrap();
        assert!(!recovered.contains("combined_percent=unavailable"));
        assert!(recovered.contains("collector_cpu_complete=false"));
    }
    #[test]
    fn sleeping_time_neither_emits_a_minute_nor_dilutes_its_cpu_denominator() {
        // Goal: a sleep gap cannot certify a full awake minute or lower its CPU share.
        // Method: advance continuous time by ten minutes while awake time advances ten seconds.
        let mut accounting = Accounting::new().unwrap();
        accounting.since = 10_000_000_000;
        accounting.since_continuous = 10_000_000_000;
        let mut collector = WorkerCost::default();
        let mut persister = WorkerCost::default();
        let mut out = Vec::new();
        let mut clocks = ResourceClocks {
            continuous_ns: 620_000_000_000,
            awake_ns: 20_000_000_000,
        };
        accounting
            .report_if_due_to(&clocks, &mut collector, &mut persister, &mut out)
            .unwrap();
        assert!(out.is_empty());
        clocks.awake_ns = 70_000_000_000;
        clocks.continuous_ns += 50_000_000_000;
        accounting
            .report_if_due_to(&clocks, &mut collector, &mut persister, &mut out)
            .unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("elapsed_ns=60000000000 clock=awake_ns")
        );
        clocks.awake_ns = 0;
        assert_eq!(
            accounting.due(&clocks).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn unavailable_adopted_health_preserves_cumulative_sample_and_baseline() {
        // Goal: a torn health read cannot masquerade as death or erase CPU between samples.
        // Method: sample this process, withhold one health observation, and sample it again.
        let accounting = Accounting::new().unwrap();
        let health = Health::own().unwrap();
        let sample = sample_process(health.pid, accounting.timebase).unwrap();
        let baseline = sample.cpu_ns.saturating_sub(5);
        let mut cost = WorkerCost {
            active_ns: 5,
            adopted: Some(health.creation),
            adopted_baseline: baseline,
            ..WorkerCost::default()
        };
        accounting.observe(&mut cost, None, None).unwrap();
        assert_eq!(cost.total().unwrap(), 5);
        assert_eq!(cost.adopted_baseline, baseline);
        assert!(!cost.incomplete);
        accounting.observe(&mut cost, None, Some(&health)).unwrap();
        assert_eq!(cost.adopted_baseline, baseline);
        assert!(cost.active_ns >= 5);
        assert!(!cost.interval_incomplete);
    }
}
