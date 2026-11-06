//! The ticks of a sealed minute as the columns the model's App rule reads, each beside the tick
//! before it, so a store or the persister derives a minute's Apps from its processes and events
//! without building frames.

use model::apps::{Stamp, Tick};
use model::columns::TickColumns;
use model::programs::Programs;
use persistence::tick_rows::{TickPlace, TickRows, read_tick};
use persistence::{Error, Result};

use super::processes::read_processes_batch;
use super::{Refused, ScopedSealedMinute as SealedMinute};
use crate::live_apps::committed::{ENDPOINTS_MAX, Endpoint};

/// A tick of a sealed minute beside the tick before it, as the App rule reads them, with the
/// index that numbers their programs.
pub struct MinuteTick<'ticks> {
    pub programs: &'ticks Programs,
    pub previous: Tick<'ticks>,
    pub selected: Tick<'ticks>,
}

/// The columns of the ticks of sealed minutes, which keep the tick read last as the one before
/// the next, so the first tick of a minute takes its rates from the minute before it.
pub struct MinuteTicks {
    /// The tick before the one read last, then the one read last.
    pair: [TickColumns; 2],
}

impl Default for MinuteTicks {
    fn default() -> Self {
        let processes = model::u32_from_length(crate::apps::PROCESS_INTERVALS_MAX);
        let events = model::u32_from_length(collector::tracker::EVENTS_PER_TICK_MAX);
        Self {
            pair: [
                TickColumns::new(processes, events),
                TickColumns::new(processes, events),
            ],
        }
    }
}

impl MinuteTicks {
    /// Hands `consume` each tick of `minute` in publication order beside the tick before it,
    /// the first beside the last tick read before this minute, and indexes in `programs` each
    /// App the minute names. A refusal, `consume`'s included, leaves no tick to take rates from.
    pub fn read(
        &mut self,
        minute: &SealedMinute,
        programs: &mut Programs,
        consume: impl FnMut(&MinuteTick<'_>) -> Result<()>,
    ) -> std::result::Result<(), Refused> {
        let read = self.read_minute(minute, programs, consume);
        if read.is_err() {
            self.clear();
        }
        read
    }

    /// Forgets the tick read last, so the next tick takes no rates from it.
    pub fn clear(&mut self) {
        for columns in &mut self.pair {
            columns.clear();
        }
    }

    fn read_minute(
        &mut self,
        minute: &SealedMinute,
        programs: &mut Programs,
        mut consume: impl FnMut(&MinuteTick<'_>) -> Result<()>,
    ) -> std::result::Result<(), Refused> {
        let batch = read_processes_batch(minute)?;
        read_minute_endpoints(&batch.endpoints)?;
        let rows = TickRows::new(&batch.rows)?;

        let mut placed = 0;
        for endpoint in &batch.endpoints {
            // The oldest columns take the tick, which then follows the one read before it.
            let columns = &mut self.pair[0];
            columns.start(stamp(endpoint));
            let place = TickPlace {
                publication: endpoint.stamp.publication,
                run_tick: endpoint.run_tick,
                events_first: endpoint.events_first,
                events_end: endpoint.events_end,
            };
            placed += read_tick(&rows, &batch.order[placed..], &place, programs, columns)?;
            columns.finish();
            self.pair.swap(0, 1);

            consume(&MinuteTick {
                programs,
                previous: self.pair[0].tick(),
                selected: self.pair[1].tick(),
            })?;
        }
        // The placement puts every row with one of the endpoints, in endpoint order.
        assert_eq!(placed, batch.order.len());
        Ok(())
    }
}

/// Refuses `endpoints` unless they are at most `ENDPOINTS_MAX` ticks, each with an event range.
/// The placement has already refused ticks out of publication order.
fn read_minute_endpoints(endpoints: &[Endpoint]) -> Result<()> {
    if endpoints.len() > ENDPOINTS_MAX {
        return Err(Error::Bound("sealed tick count"));
    }
    for endpoint in endpoints {
        if endpoint.events_first > endpoint.events_end {
            return Err(Error::Bound("sealed tick event range"));
        }
    }
    Ok(())
}

fn stamp(endpoint: &Endpoint) -> Stamp {
    Stamp {
        recording: endpoint.identity.recording.0,
        boot: endpoint.identity.boot.0,
        run: endpoint.identity.run.0,
        tick: endpoint.run_tick,
        continuous_ns: endpoint.stamp.continuous_ns,
    }
}
