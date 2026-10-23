//! What the persister derives of the Apps of each tick of a window as it seals, by the model's
//! App rule over the ticks it staged, so the viewer shows a saved second's Apps without
//! replaying its minute.
use std::cell::RefCell;
use std::ops::Range;

use model::apps::{Apps, ProcessTotals, Totals};
use model::programs::{Capacity, Programs};
use model::ranks::{Metric, RANKS_MAX, Rank, Ranks};

use crate::app_tables::PARTS_ROWS_MAX;
use crate::schema::APP_PARTS;
use crate::window::PROCESSES_MAX;
use crate::window_ticks::{WindowTick, WindowTicks};
use crate::worker::{Derivation, Derive};
use crate::{Error, Observation, Result, TableManifest};

mod parts;
mod ranks;

/// The run's programs the index holds, as many as the collector's dictionary of a run holds.
const PROGRAMS_CAPACITY: Capacity = Capacity {
    entries: 65_536,
    program_bytes: 64 * 1024 * 1024,
};
/// What the index of the run's programs takes: its bytes of programs, and a few words for each
/// of its entries.
const PROGRAMS_BYTES_MAX: u64 = 72 * 1024 * 1024;
/// What the Apps and processes a window holds for its parts may take, a row of the table each.
const HELD_BYTES_MAX: u64 = 256 * 1024 * 1024;

const _: () = {
    assert!(PROGRAMS_CAPACITY.program_bytes == 64 * 1024 * 1024);
    assert!(PROGRAMS_CAPACITY.entries == 65_536);
    assert!(PARTS_ROWS_MAX == 1 << 19);
    assert!((1 << 19) * size_of::<Totals>() <= 256 * 1024 * 1024);
    assert!((1 << 19) * size_of::<ProcessTotals>() <= 256 * 1024 * 1024);
    // The window staged for its seal, the index of its run's programs, the Apps and processes
    // held for its parts, the table built from them and the buffer it is written through fit
    // the persister's working bound together.
    assert!(
        crate::STAGED_BYTES_MAX
            + PROGRAMS_BYTES_MAX
            + HELD_BYTES_MAX
            + parts::DECODED_BYTES_MAX
            + crate::WRITE_BUFFER_BYTES
            <= crate::resources::WORKING_BYTES_MAX
    );
};

/// Derives the Apps of each tick of a window as it seals, from the ticks the persister staged,
/// and keeps the Apps' ranks and every App's part.
pub struct SealedApps {
    state: RefCell<State>,
}

impl Default for SealedApps {
    fn default() -> Self {
        let programs = Programs::new(PROGRAMS_CAPACITY);
        let processes = model::u32_from_length(
            PROCESSES_MAX + collector::tracker::EVENTS_PER_TICK_MAX,
        );
        Self {
            state: RefCell::new(State {
                ticks: WindowTicks::default(),
                apps: Apps::new(processes, &programs),
                programs,
                ranks: Ranks::default(),
                window: WindowApps::default(),
            }),
        }
    }
}

impl Derive for SealedApps {
    fn derive(&self, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
        let mut state = self.state.borrow_mut();
        state.read(derivation)?;

        let mut tables = vec![ranks::encode(
            &state.window,
            &state.programs,
            derivation.pending,
        )?];
        let parts = parts::encode(&state.window, &state.programs, derivation.pending);
        tables.extend(derive_parts(parts, derivation)?);

        Ok(tables)
    }
}

/// What a derivation keeps from window to window: the last tick read, which the next window's
/// first tick takes its rates from, the index of the run's programs, and the buffers it derives
/// each tick and holds each window in.
struct State {
    ticks: WindowTicks,
    programs: Programs,
    apps: Apps,
    ranks: Ranks,
    window: WindowApps,
}

impl State {
    /// Derives the Apps of each tick of the derivation's window. A refused window starts the
    /// index again, so a full index refuses one window and not every one after it.
    fn read(&mut self, derivation: &Derivation<'_>) -> Result<()> {
        if derivation.window.ticks.is_empty() {
            return Err(Error::Bound("sealed Apps ticks"));
        }

        self.window.clear();
        let Self {
            ticks,
            programs,
            apps,
            ranks,
            window,
        } = self;
        let read = ticks.read(derivation.window, programs, |tick| {
            apps.derive(tick.programs, &tick.previous, &tick.selected);
            ranks.rank(tick.programs, apps.totals(), apps.elapsed_ns());
            window.push(tick, apps, ranks);
            Ok(())
        });
        if read.is_err() {
            *programs = Programs::new(PROGRAMS_CAPACITY);
        }

        read
    }
}

/// The table of the window's App parts, or none when its parts are more than the table holds
/// or writing them failed, since its ranks stand without them and the replay of its minute
/// gives its parts.
fn derive_parts(
    parts: Result<TableManifest>,
    derivation: &Derivation<'_>,
) -> Result<Option<TableManifest>> {
    match parts {
        Ok(table) => Ok(Some(table)),
        Err(error) => {
            eprintln!(
                "persister derived no App parts window={}: {error}",
                derivation.window.identity.window
            );
            let path = derivation.pending.join(format!("{APP_PARTS}.parquet"));
            match std::fs::remove_file(path) {
                Ok(()) => Ok(None),
                Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(problem) => Err(problem.into()),
            }
        }
    }
}

/// The Apps of each tick of one window, as its tables keep them. A derivation reuses it from
/// window to window.
#[derive(Default)]
struct WindowApps {
    ticks: Vec<TickApps>,
    rankings: Vec<RankingRow>,
    /// Every tick's Apps end to end, in key order within a tick.
    totals: Vec<Totals>,
    /// Every tick's processes end to end, in order of pid and unique id within a tick.
    processes: Vec<ProcessTotals>,
    /// The rows of the parts table the window's ticks make.
    parts_rows: u64,
}

/// What the tables keep of one tick besides its Apps' totals and processes.
struct TickApps {
    observation: Observation,
    run_tick: u64,
    /// The observation the tick's rates come from, none where it takes no rates.
    previous: Option<Observation>,
    elapsed_ns: Option<u64>,
    ungrouped_members: u32,
    live_ungrouped_members: u32,
    usage_refused: u32,
    usage_gone: u32,
    app_count: u32,
    process_count: u32,
    /// The tick's Apps and processes in the window's, empty once the window holds more parts
    /// than the table keeps.
    totals: Range<usize>,
    processes: Range<usize>,
}

/// One ranking of a tick, and the App at `position` in it when it ranks any.
struct RankingRow {
    /// The tick's place among the window's.
    tick: usize,
    metric: Metric,
    unavailable: u32,
    overflowed: u32,
    partial: u32,
    position: Option<u8>,
    rank: Option<Rank>,
}

impl WindowApps {
    fn clear(&mut self) {
        self.ticks.clear();
        self.rankings.clear();
        self.totals.clear();
        self.processes.clear();
        self.parts_rows = 0;
    }

    /// Whether the window's ticks make more rows than the parts table keeps.
    fn parts_full(&self) -> bool {
        self.parts_rows > u64::from(PARTS_ROWS_MAX)
    }

    /// Holds what the tables keep of `tick`, whose Apps `apps` derived and `ranks` ranked.
    fn push(&mut self, tick: &WindowTick<'_>, apps: &Apps, ranks: &Ranks) {
        let elapsed_ns = apps.elapsed_ns();
        if elapsed_ns.is_some() {
            assert!(tick.previous_observation.is_some());
        }
        let totals_count = apps.totals().len();
        let processes_count = apps.processes().len();
        self.parts_rows += collector::u64_from_usize(1 + totals_count + processes_count);

        let totals_first = self.totals.len();
        let processes_first = self.processes.len();
        if !self.parts_full() {
            self.totals.extend_from_slice(apps.totals());
            self.processes.extend_from_slice(apps.processes());
        }
        self.ticks.push(TickApps {
            observation: tick.observation,
            run_tick: tick.selected.stamp.tick,
            previous: elapsed_ns.and(tick.previous_observation),
            elapsed_ns,
            ungrouped_members: apps.ungrouped_members(),
            live_ungrouped_members: apps.live_ungrouped_members(),
            usage_refused: tick.usage_refused,
            usage_gone: tick.usage_gone,
            app_count: model::u32_from_length(totals_count),
            process_count: model::u32_from_length(processes_count),
            totals: totals_first..self.totals.len(),
            processes: processes_first..self.processes.len(),
        });

        let tick = self.ticks.len() - 1;
        for ranking in &ranks.metrics {
            let entries = ranking.entries();
            assert!(entries.len() <= RANKS_MAX);
            let row = |position: Option<u8>, rank: Option<Rank>| RankingRow {
                tick,
                metric: ranking.metric,
                unavailable: ranking.unavailable,
                overflowed: ranking.overflowed,
                partial: ranking.partial,
                position,
                rank,
            };
            if entries.is_empty() {
                self.rankings.push(row(None, None));
            }
            for (position, rank) in (0_u8..).zip(entries) {
                self.rankings.push(row(Some(position), Some(*rank)));
            }
        }
    }
}

#[cfg(test)]
mod tests;
