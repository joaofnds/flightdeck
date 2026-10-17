use std::io::{self, Write};

use crate::identity_report::IdentityReport;

use collector::collector::Collector;
use collector::cost::Meter;
use collector::ring::{
    Dropped, IOReportReadings, IdentityObservation, IdentityPublication, IdentityUnavailable,
    ObservationTimes, ProcessIdentity, Readings, SmcReadings, UsageAvailability, Writer,
};
use collector::tracker::{EVENTS_PER_TICK_MAX, Event};

pub(crate) struct Publication {
    identity_enabled: bool,
    events: Vec<IdentityObservation>,
    report: IdentityReport,
}

impl Publication {
    pub(crate) fn new(ring: &Writer) -> io::Result<Self> {
        let identity_enabled = ring.dictionary_usage()?.is_some();

        Ok(Self {
            identity_enabled,
            events: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            report: IdentityReport::default(),
        })
    }

    pub(crate) fn read(
        &mut self,
        collector: &mut Collector,
        meter: &mut Meter,
        ring: &mut Writer,
    ) -> io::Result<()> {
        if self.identity_enabled {
            collector.read_with_identity(meter, ring)?;
            self.report.add(collector.identities.diagnostics());

            Ok(())
        } else {
            collector.read(meter)
        }
    }

    pub(crate) fn publish(
        &mut self,
        ring: &mut Writer,
        collector: &Collector,
        tick: u64,
        times: Option<&ObservationTimes>,
    ) -> io::Result<()> {
        let ioreport = collector
            .ioreport
            .as_ref()
            .map(|ioreport| IOReportReadings {
                gpu_energy_nj: ioreport.gpu_energy_nj(),
                gpu_active_ns: ioreport.gpu().active_ns(),
                cores: ioreport.cores(),
            });
        let smc = collector.smc.as_ref().map(|smc| SmcReadings {
            power_watts: smc.power_watts(),
            soc_celsius: smc.soc_celsius(),
            fan_rpm: smc.fan_rpm(),
        });
        let counts = collector.counts_tick();
        let readings = Readings {
            run_tick: tick,
            cpus: collector.machine.cpus(),
            memory: &collector.machine.memory,
            interfaces: collector.interfaces.readings(),
            ioreport,
            smc,
            processes: &collector.processes.readings,
            processes_refused: collector.processes.refused,
            processes_gone: collector.processes.gone,
            coalitions: &collector.coalitions.readings,
            events: collector.events(),
            dropped: Dropped {
                starts: counts.dropped_starts,
                execs: counts.dropped_execs,
                exits: counts.dropped_exits,
            },
        };

        self.publish_readings(ring, &readings, times, collector.identities.observations())
    }

    fn publish_readings(
        &mut self,
        ring: &mut Writer,
        readings: &Readings,
        times: Option<&ObservationTimes>,
        processes: &[IdentityObservation],
    ) -> io::Result<()> {
        if self.identity_enabled {
            self.fill_events(readings.events);
            ring.publish_with_identity(
                readings,
                times,
                &IdentityPublication {
                    processes,
                    events: &self.events,
                },
            )
        } else {
            if let Some(times) = times {
                ring.publish_observed(readings, times);
            } else {
                ring.publish(readings);
            }

            Ok(())
        }
    }

    pub(crate) fn report(
        &mut self,
        ring: &Writer,
        label: &'static str,
        out: &mut impl Write,
    ) -> io::Result<()> {
        self.report.report(ring, label, out)
    }

    fn fill_events(&mut self, events: &[Event]) {
        assert!(events.len() <= EVENTS_PER_TICK_MAX);
        assert!(self.events.capacity() >= EVENTS_PER_TICK_MAX);

        self.events.clear();

        for event in events {
            let (pid, usage) = match event {
                Event::Start { pid, .. } | Event::Exec { pid } => {
                    (*pid, UsageAvailability::NotApplicable)
                }
                Event::Exit { pid, last, .. } => (
                    *pid,
                    match last {
                        Ok(_) => UsageAvailability::Measured,
                        Err(collector::processes::Unreadable::Refused) => {
                            UsageAvailability::Refused
                        }
                        Err(collector::processes::Unreadable::Gone) => UsageAvailability::Gone,
                    },
                ),
            };

            assert!(self.events.len() < self.events.capacity());
            self.events.push(IdentityObservation {
                pid,
                usage,
                identity: ProcessIdentity::Unavailable {
                    creation: None,
                    reason: IdentityUnavailable::Unsupported,
                },
            });
        }

        assert_eq!(self.events.len(), events.len());
    }
}

#[cfg(test)]
mod tests;
