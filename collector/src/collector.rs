use std::io;
use std::time::Duration;

use crate::clock::Timebase;
use crate::coalitions::{self, Coalitions};
use crate::cost::{Meter, Source};
use crate::interfaces::Interfaces;
use crate::ioreport::IoReport;
use crate::machine::{self, Machine};
use crate::processes::{self, Sweep};
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
}

impl Collector {
    pub fn new(timebase: Timebase, rescan_interval: Duration) -> io::Result<Self> {
        let process_limit = processes::process_limit()?;
        let cpu_limit = machine::cpu_limit()?;
        // Room for a coalition for every process macOS runs and one for every start a tick
        // holds, with a coalition past it reported as an error.
        let coalition_limit = process_limit + EVENTS_PER_TICK_MAX;

        let mut collector = Self {
            timebase,
            listing: processes::pid_list(process_limit),
            tracker: Tracker::new(
                timebase,
                rescan_interval,
                Limits {
                    process_limit,
                    events_per_tick_max: EVENTS_PER_TICK_MAX,
                },
            )?,
            machine: Machine::new(cpu_limit)?,
            interfaces: Interfaces::default(),
            ioreport: IoReport::open(timebase, cpu_limit)?,
            smc: Smc::open()?,
            processes: Sweep::new(process_limit),
            coalitions: Coalitions::new(timebase, coalition_limit),
            events: Vec::with_capacity(EVENTS_PER_TICK_MAX),
        };

        processes::list_pids(&mut collector.listing)?;
        for &pid in &collector.listing {
            if let Some(id) = coalitions::coalition_of(pid) {
                collector.coalitions.add(id)?;
            }
        }

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
            self.events.extend(self.tracker.drain_events());
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
            self.coalitions.read_all();
            io::Result::Ok(())
        })?;

        Ok(())
    }
}
