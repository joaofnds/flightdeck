use std::io;

use crate::clock::Timebase;
use crate::coalitions::Coalitions;
use crate::cost::{Meter, Source};
use crate::interfaces::Interfaces;
use crate::ioreport::IoReport;
use crate::machine::Machine;
use crate::processes::{self, Sweep};
use crate::smc::Smc;
use crate::tracker::{Event, Tracker};

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
    pub fn new(timebase: Timebase) -> io::Result<Self> {
        let mut collector = Self {
            timebase,
            listing: Vec::new(),
            tracker: Tracker::new(timebase)?,
            machine: Machine::new()?,
            interfaces: Interfaces::default(),
            ioreport: IoReport::open(timebase)?,
            smc: Smc::open()?,
            processes: Sweep::default(),
            coalitions: Coalitions::new(timebase),
            events: Vec::new(),
        };

        processes::list_pids(&mut collector.listing)?;
        for &pid in &collector.listing {
            collector.coalitions.join(pid);
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
                if let Event::Start { pid } = event {
                    self.coalitions.join(*pid);
                }
            }
            self.coalitions.read_all();
        });
        Ok(())
    }
}
