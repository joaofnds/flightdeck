use std::io;
use std::time::Duration;

use crate::clock::Timebase;
use crate::coalitions::{self, Coalitions};
use crate::cost::{Meter, Source};
use crate::interfaces::Interfaces;
use crate::ioreport::IoReport;
use crate::machine::{self, Machine};
use crate::processes::{self, Sweep};
use crate::push_without_growing;
use crate::smc::Smc;
use crate::tracker::{EVENTS_PER_TICK_MAX, Event, Limits, Tracker};

pub struct Collector {
    timebase: Timebase,
    listing: Vec<i32>,
    pub tracker: Tracker,
    pub machine: Machine,
    pub interfaces: Interfaces,
    pub ioreport: IoReport,
    pub smc: Smc,
    pub processes: Sweep,
    pub coalitions: Coalitions,
    pub events: Vec<Event>,
    /// The starts the tracker had dropped when the previous tick was read.
    dropped_starts_previous: u64,
}

impl Collector {
    pub fn new(timebase: Timebase, rescan_interval: Duration) -> io::Result<Self> {
        let limits = Limits {
            process_limit: processes::process_limit()?,
            events_per_tick_max: EVENTS_PER_TICK_MAX,
        };
        let cpu_limit = machine::cpu_limit()?;
        // Room for a coalition for every process macOS runs and one for every start a tick
        // holds, with a coalition past it reported as an error.
        let coalition_limit = limits.process_limit + limits.events_per_tick_max;

        let mut collector = Self {
            timebase,
            listing: processes::pid_list(limits.process_limit),
            tracker: Tracker::new(timebase, rescan_interval, limits)?,
            machine: Machine::new(cpu_limit)?,
            interfaces: Interfaces::default(),
            ioreport: IoReport::open(timebase, cpu_limit)?,
            smc: Smc::open()?,
            processes: Sweep::new(limits.process_limit),
            coalitions: Coalitions::new(timebase, coalition_limit),
            // The tracker holds at most this many events a tick, so a tick's events fit.
            events: Vec::with_capacity(limits.events_per_tick_max),
            dropped_starts_previous: 0,
        };

        processes::list_pids(&mut collector.listing)?;
        add_listed_coalitions(&collector.listing, &mut collector.coalitions)?;

        Ok(collector)
    }

    pub fn wait_until(&mut self, deadline: u64, meter: &mut Meter) -> io::Result<()> {
        self.tracker.follow_until(deadline, meter)
    }

    pub fn read(&mut self, meter: &mut Meter) -> io::Result<()> {
        meter.measure(Source::Machine, || self.machine.read())?;
        meter.measure(Source::Interfaces, || self.interfaces.read_all())?;
        meter.measure(Source::IoReport, || self.ioreport.read())?;
        meter.measure(Source::Smc, || self.smc.read())?;

        meter.measure(Source::Processes, || {
            processes::list_pids(&mut self.listing)?;
            self.processes.run(&self.listing, self.timebase);
            io::Result::Ok(())
        })?;
        self.tracker.observe(&self.listing, meter)?;
        meter.measure(Source::Events, || {
            self.events.clear();
            for event in self.tracker.drain_events() {
                push_without_growing(&mut self.events, event);
            }
        });
        meter.measure(Source::Coalitions, || {
            for event in &self.events {
                if let Event::Start {
                    coalition: Some(id),
                    ..
                } = event
                {
                    self.coalitions.add(*id)?;
                }
            }
            // A dropped start takes its coalition with it, so a tick that dropped one adds the
            // coalition of every process listed, as startup does.
            if self.tracker.counts.dropped_starts > self.dropped_starts_previous {
                add_listed_coalitions(&self.listing, &mut self.coalitions)?;
            }
            self.dropped_starts_previous = self.tracker.counts.dropped_starts;
            self.coalitions.read_all();
            io::Result::Ok(())
        })?;

        Ok(())
    }
}

/// Adds the coalition of every process in `listing`.
fn add_listed_coalitions(listing: &[i32], coalitions: &mut Coalitions) -> io::Result<()> {
    for &pid in listing {
        if let Some(id) = coalitions::coalition_of(pid) {
            coalitions.add(id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_listed_coalition_after_a_tick_that_dropped_a_start() {
        // Goal: a start dropped from a full event list takes its coalition with it, so the tick
        // that dropped it adds the coalition of every process listed.
        // Method: empty a collector's coalitions, count one start as dropped, and read a tick.
        // launchd's coalition holds only launchd and the kernel, so no start names it.
        let timebase = Timebase::read();
        let mut collector = Collector::new(timebase, Duration::ZERO).unwrap();
        collector.coalitions = Coalitions::new(timebase, collector.listing.capacity());
        collector.tracker.counts.dropped_starts += 1;

        collector.read(&mut Meter::default()).unwrap();

        let launchd = coalitions::coalition_of(1).unwrap();
        let read = &collector.coalitions.read;
        assert!(read.iter().any(|coalition| coalition.id == launchd));
    }
}
