use std::io::{self, Write};

use crate::identity_report::IdentityReport;

use collector::collector::Collector;
use collector::cost::Meter;
use collector::ring::{
    Dropped, IOReportReadings, IdentityObservation, IdentityPublication, IdentityUnavailable,
    ObservationTimes, ProcessIdentity, Readings, SmcReadings, SupplementalReadings,
    UsageAvailability, Writer,
};
use collector::tracker::{EVENTS_PER_TICK_MAX, Event, EventCreation, EventIdentity};

pub(crate) struct Publication {
    identity_enabled: bool,
    battery_enabled: bool,
    disks_enabled: bool,
    events: Vec<IdentityObservation>,
    report: IdentityReport,
}

impl Publication {
    pub(crate) fn new(ring: &Writer) -> io::Result<Self> {
        let identity_enabled = ring.dictionary_usage()?.is_some();

        Ok(Self {
            identity_enabled,
            battery_enabled: ring.battery_supported(),
            disks_enabled: ring.disks_supported(),
            events: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            report: IdentityReport::default(),
        })
    }

    pub(crate) fn read(
        &mut self,
        collector: &mut Collector,
        meter: &mut Meter,
        ring: &mut Writer,
        times: Option<&ObservationTimes>,
    ) -> io::Result<()> {
        if self.battery_enabled {
            if let Some(times) = times {
                collector.read_battery(times.continuous_ns, meter)?;
            }
        }
        if self.disks_enabled {
            if let Some(times) = times {
                collector.read_disks(times.continuous_ns, meter)?;
            }
        }

        if self.identity_enabled {
            collector.read_with_identity(meter, ring)?;
            self.report.add(collector.identities.diagnostics());

            Ok(())
        } else {
            collector.read(meter)
        }
    }

    pub(crate) fn wait_until(
        &mut self,
        collector: &mut Collector,
        deadline: u64,
        meter: &mut Meter,
        ring: &mut Writer,
    ) -> io::Result<()> {
        if self.identity_enabled {
            collector.wait_until_with_identity(deadline, meter, ring)?;

            self.report.add(collector.identities.diagnostics());

            Ok(())
        } else {
            collector.wait_until(deadline, meter)
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

        let battery =
            times.and_then(|times| collector.battery.as_ref()?.reading(times.continuous_ns));
        let disks = times.and_then(|times| {
            let snapshot = collector.disks.as_ref()?.snapshot();
            (snapshot.source_continuous_ns == times.continuous_ns).then_some(snapshot)
        });

        self.publish_readings(
            ring,
            &readings,
            times,
            collector.identities.observations(),
            collector.event_identities(),
            &SupplementalReadings { battery, disks },
        )
    }

    fn publish_readings(
        &mut self,
        ring: &mut Writer,
        readings: &Readings,
        times: Option<&ObservationTimes>,
        processes: &[IdentityObservation],
        event_identities: &[EventIdentity],
        supplemental: &SupplementalReadings,
    ) -> io::Result<()> {
        if self.identity_enabled {
            self.fill_events(readings.events, event_identities);
            let identities = IdentityPublication {
                processes,
                events: &self.events,
            };

            if self.disks_enabled {
                ring.publish_with_disks(readings, times, &identities, supplemental)
            } else {
                if self.battery_enabled {
                    ring.publish_with_battery(readings, times, &identities, supplemental.battery)
                } else {
                    ring.publish_with_identity(readings, times, &identities)
                }
            }
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

    fn fill_events(&mut self, events: &[Event], identities: &[EventIdentity]) {
        assert!(events.len() <= EVENTS_PER_TICK_MAX);
        assert_eq!(events.len(), identities.len());
        assert!(self.events.capacity() >= EVENTS_PER_TICK_MAX);

        self.events.clear();

        for (event, source) in events.iter().zip(identities) {
            let (pid, usage, identity) = match event {
                Event::Start { pid, .. } => {
                    let identity = match source.creation {
                        EventCreation::Start(identity) => identity,
                        EventCreation::NotApplicable => IdentityObservation::EMPTY.identity,
                        EventCreation::Exit(_) => unreachable!("Start carries no Exit metadata"),
                    };
                    (*pid, UsageAvailability::NotApplicable, identity)
                }
                Event::Exec { pid } => {
                    assert_eq!(source.creation, EventCreation::NotApplicable);
                    (
                        *pid,
                        UsageAvailability::NotApplicable,
                        ProcessIdentity::Unavailable {
                            creation: None,
                            reason: IdentityUnavailable::Unsupported,
                        },
                    )
                }
                Event::Exit { pid, last, .. } => {
                    let EventCreation::Exit(creation) = source.creation else {
                        unreachable!("exit source metadata accompanies only an exit");
                    };

                    let (creation, reason) = match creation {
                        Ok(creation) => (Some(creation), IdentityUnavailable::Unsupported),
                        Err(reason) => (None, reason),
                    };

                    (
                        *pid,
                        match last {
                            Ok(_) => UsageAvailability::Measured,
                            Err(collector::processes::Unreadable::Refused) => {
                                UsageAvailability::Refused
                            }
                            Err(collector::processes::Unreadable::Gone) => UsageAvailability::Gone,
                        },
                        ProcessIdentity::Unavailable { creation, reason },
                    )
                }
            };
            assert_eq!(source.pid, pid);

            assert!(self.events.len() < self.events.capacity());
            self.events.push(IdentityObservation {
                pid,
                usage,
                identity,
            });
        }

        assert_eq!(self.events.len(), events.len());
    }
}

#[cfg(test)]
mod tests;
