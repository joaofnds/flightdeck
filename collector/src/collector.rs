use std::io;
use std::time::Duration;

use crate::clock::Timebase;
use crate::coalitions::{self, Coalitions};
use crate::cost::{Meter, Source};
use crate::interfaces::Interfaces;
use crate::ioreport::IOReport;
use crate::machine::{self, Machine};
use crate::processes::{self, Sweep};
use crate::push_without_growing;
use crate::smc::Smc;
use crate::tracker::{Counts, EVENTS_PER_TICK_MAX, Event, Limits, Tracker};

pub struct Collector {
    timebase: Timebase,
    listing: Vec<i32>,
    pub tracker: Tracker,
    pub machine: Machine,
    pub interfaces: Interfaces,
    /// `None` where `IOReport` failed to open, as on a Mac with none of its channels.
    pub ioreport: Option<IOReport>,
    /// `None` where the SMC failed to open, as on a Mac with no SMC.
    pub smc: Option<Smc>,
    pub processes: Sweep,
    pub coalitions: Coalitions,
    events: Vec<Event>,
    /// What the tracker had counted when the previous tick was read.
    counts_previous: Counts,
}

impl Collector {
    pub fn new(timebase: Timebase, rescan_interval: Duration) -> io::Result<Self> {
        let limits = Limits {
            process_limit: processes::process_limit()?,
            events_per_tick_max: EVENTS_PER_TICK_MAX,
        };
        assert!(limits.process_limit > 0);
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
            ioreport: new_source("IOReport", IOReport::open(timebase, cpu_limit)),
            smc: new_source("SMC", Smc::open()),
            processes: Sweep::new(limits.process_limit),
            coalitions: Coalitions::new(timebase, coalition_limit),
            // The tracker holds at most this many events a tick, so a tick's events fit.
            events: Vec::with_capacity(limits.events_per_tick_max),
            counts_previous: Counts::default(),
        };

        processes::list_pids(&mut collector.listing)?;
        add_listed_coalitions(&collector.listing, &mut collector.coalitions)?;

        Ok(collector)
    }

    /// The events the last tick drained from the tracker. The list is private, since a tick
    /// fills it within the room `new` gave it and never grows it.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        assert!(self.events.len() <= EVENTS_PER_TICK_MAX);
        assert!(self.events.capacity() >= EVENTS_PER_TICK_MAX);

        &self.events
    }

    pub fn wait_until(&mut self, deadline: u64, meter: &mut Meter) -> io::Result<()> {
        self.tracker.follow_until(deadline, meter)
    }

    pub fn read(&mut self, meter: &mut Meter) -> io::Result<()> {
        // A tick fills the lists that `new` sized and never grows them.
        let listing_capacity = self.listing.capacity();
        let events_capacity = self.events.capacity();

        meter.measure(Source::Machine, || self.machine.read())?;
        meter.measure(Source::Interfaces, || self.interfaces.read())?;
        if let Some(ioreport) = &mut self.ioreport {
            meter.measure(Source::IOReport, || ioreport.read())?;
        }
        if let Some(smc) = &mut self.smc {
            meter.measure(Source::Smc, || smc.read())?;
        }

        meter.measure(Source::Processes, || {
            processes::list_pids(&mut self.listing)?;
            self.processes.run(self.timebase, &self.listing)
        })?;
        self.tracker.observe(&self.listing, meter)?;
        meter.measure(Source::Events, || {
            self.events.clear();
            let drained = self.tracker.drain_events();
            // `new` gave the tracker and these events one limit, so a tick's events fit.
            assert!(drained.len() <= self.events.capacity());
            for event in drained {
                push_without_growing(&mut self.events, event);
            }
            Ok(())
        })?;
        meter.measure(Source::Coalitions, || {
            let dropped_starts =
                read_dropped_starts(&mut self.counts_previous, self.tracker.counts());
            read_coalitions(
                &self.events,
                dropped_starts,
                &self.listing,
                &mut self.coalitions,
            )
        })?;

        assert!(!self.listing.is_empty());
        assert!(self.events.len() <= EVENTS_PER_TICK_MAX);
        assert_eq!(listing_capacity, self.listing.capacity());
        assert_eq!(events_capacity, self.events.capacity());
        Ok(())
    }
}

/// Returns the source `opened` holds, or reports why it failed to open and returns `None`, so
/// a Mac without one source records every other source.
fn new_source<T>(name: &str, opened: io::Result<T>) -> Option<T> {
    match opened {
        Ok(source) => Some(source),
        Err(error) => {
            eprintln!("collector: recording no {name} readings, since it failed to open: {error}");
            None
        }
    }
}

/// Returns the starts the tracker dropped since the previous tick, whose counts
/// `counts_previous` holds, and keeps `counts` there for the next tick.
fn read_dropped_starts(counts_previous: &mut Counts, counts: Counts) -> u64 {
    let added = counts.since(counts_previous);
    *counts_previous = counts;

    assert!(added.dropped_starts <= counts.dropped_starts);
    added.dropped_starts
}

/// Adds the coalition of each process `events` saw start, and of every process in `listing`
/// when the tick dropped a start, then reads every coalition held.
fn read_coalitions(
    events: &[Event],
    dropped_starts: u64,
    listing: &[i32],
    coalitions: &mut Coalitions,
) -> io::Result<()> {
    assert!(events.len() <= EVENTS_PER_TICK_MAX);

    for event in events {
        if let Event::Start {
            coalition: Some(id),
            ..
        } = event
        {
            coalitions.add(*id)?;
        }
    }
    // A dropped start takes its coalition with it, so a tick that dropped one adds the
    // coalition of every process listed, as startup does.
    if dropped_starts > 0 {
        add_listed_coalitions(listing, coalitions)?;
    }

    coalitions.read()
}

/// Adds the coalition of every process in `listing`.
fn add_listed_coalitions(listing: &[i32], coalitions: &mut Coalitions) -> io::Result<()> {
    // A listing holds this process's pid at least.
    assert!(!listing.is_empty());

    for &pid in listing {
        if let Some(id) = coalitions::coalition_of(pid)? {
            coalitions.add(id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pids of the processes running now, in a list with room for every process macOS
    /// runs.
    fn listing_now() -> Vec<i32> {
        let mut listing = processes::pid_list(processes::process_limit().unwrap());
        processes::list_pids(&mut listing).unwrap();
        listing
    }

    #[test]
    fn reads_a_tick_without_an_smc_or_ioreport() {
        // Goal: a Mac without an SMC or IOReport channels, as CI's virtual machine is, still
        // records every other source.
        // Method: start a collector, leave out its SMC and IOReport as `new` does where they
        // fail to open, and read a tick.
        let timebase = Timebase::read().unwrap();
        let mut collector = Collector::new(timebase, Duration::ZERO).unwrap();
        collector.smc = None;
        collector.ioreport = None;

        collector.read(&mut Meter::default()).unwrap();

        assert!(!collector.processes.readings.is_empty());
        assert!(!collector.coalitions.readings.is_empty());
    }

    #[test]
    fn reads_the_starts_dropped_since_the_previous_tick() {
        // Goal: a tick's dropped starts are those the tracker dropped since the previous tick,
        // never the total it has dropped.
        // Method: read the dropped starts of a tick whose tracker has dropped 3, after a tick
        // whose tracker had dropped 2.
        let mut counts_previous = Counts {
            dropped_starts: 2,
            ..Counts::default()
        };
        let counts = Counts {
            dropped_starts: 3,
            ..Counts::default()
        };

        assert_eq!(1, read_dropped_starts(&mut counts_previous, counts));
    }

    #[test]
    fn keeps_the_counts_of_a_tick_for_the_next() {
        // Goal: a tick's counts are the previous counts of the tick after it, so a start dropped
        // once counts in one tick only.
        // Method: read the dropped starts of two ticks whose tracker has dropped 1 start, both
        // after no start dropped.
        let mut counts_previous = Counts::default();
        let counts = Counts {
            dropped_starts: 1,
            ..Counts::default()
        };
        read_dropped_starts(&mut counts_previous, counts);

        assert_eq!(0, read_dropped_starts(&mut counts_previous, counts));
    }

    #[test]
    fn reads_the_coalition_a_start_names() {
        // Goal: a tick reads the coalition of each process it saw start.
        // Method: read the coalitions of a tick whose one event is a start in launchd's
        // coalition, with nothing listed and no start dropped.
        let timebase = Timebase::read().unwrap();
        let launchd = coalitions::coalition_of(1).unwrap().unwrap();
        let start = Event::Start {
            pid: 1,
            coalition: Some(launchd),
        };
        let mut coalitions = Coalitions::new(timebase, 1);

        read_coalitions(&[start], 0, &[], &mut coalitions).unwrap();

        let readings = &coalitions.readings;
        assert_eq!(
            vec![launchd],
            readings
                .iter()
                .map(|coalition| coalition.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn reads_no_listed_coalition_after_a_tick_that_dropped_no_start() {
        // Goal: a tick that dropped no start reads only the coalitions its starts name, never
        // those of every process listed.
        // Method: read the coalitions of a tick with every process listed, no event, and no
        // start dropped.
        let timebase = Timebase::read().unwrap();
        let listing = listing_now();
        let mut coalitions = Coalitions::new(timebase, listing.capacity());

        read_coalitions(&[], 0, &listing, &mut coalitions).unwrap();

        assert!(coalitions.readings.is_empty());
    }

    #[test]
    fn reads_every_listed_coalition_after_a_tick_that_dropped_a_start() {
        // Goal: a start dropped from a full event list takes its coalition with it, so the tick
        // that dropped it adds the coalition of every process listed.
        // Method: read the coalitions of a tick with every process listed, no event, and one
        // start dropped. launchd's coalition holds only launchd and the kernel, so no start
        // names it.
        let timebase = Timebase::read().unwrap();
        let listing = listing_now();
        let mut coalitions = Coalitions::new(timebase, listing.capacity());

        read_coalitions(&[], 1, &listing, &mut coalitions).unwrap();

        let launchd = coalitions::coalition_of(1).unwrap().unwrap();
        let readings = &coalitions.readings;
        assert!(readings.iter().any(|coalition| coalition.id == launchd));
    }
}
