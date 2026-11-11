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
mod predecessor;
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
        let processes =
            model::u32_from_length(PROCESSES_MAX + collector::tracker::EVENTS_PER_TICK_MAX);
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
        state.seed(derivation)?;
        state.read(derivation.window)?;

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
    /// Puts the tick saved before the window where its first tick looks for its rates, unless this
    /// derivation read that tick itself. Whatever came before no longer follows, so the index
    /// starts again, and a full index refuses one window and not every one after it. A refusal
    /// lets the window go on being saved with no derived tables for a reader to rebuild.
    fn seed(&mut self, derivation: &Derivation<'_>) -> Result<()> {
        if self.ticks.follows(derivation.window) {
            return Ok(());
        }
        self.ticks.clear();
        self.programs = Programs::new(PROGRAMS_CAPACITY);

        predecessor::seed(
            &mut self.ticks,
            &mut self.programs,
            derivation.window,
            &derivation.saved,
        )
    }

    /// Derives the Apps of each tick of the derivation's window.
    fn read(&mut self, sealing: &crate::Window) -> Result<()> {
        if sealing.ticks.is_empty() {
            return Err(Error::Bound("sealed Apps ticks"));
        }

        // Each tick makes a row of the parts and each process of it another, which tells that a
        // window overfills the parts before any of its rows is held.
        let processes = crate::encoder::bounds::rows(crate::schema::Table::Processes, sealing);
        self.window
            .start(collector::u64_from_usize(sealing.ticks.len() + processes));
        let Self {
            ticks,
            programs,
            apps,
            ranks,
            window,
        } = self;
        ticks.read(sealing, programs, |tick| {
            apps.derive(tick.programs, &tick.previous, &tick.selected);
            ranks.rank(tick.programs, apps.totals(), apps.elapsed_ns());
            window.push(tick, apps, ranks);
            Ok(())
        })?;
        assert!(window.parts_rows_least <= window.parts_rows);

        Ok(())
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
            collector::diagnostics::error(format_args!(
                "persister derived no App parts window={}: {error}",
                derivation.window.identity.window
            ));
            match derivation
                .pending
                .remove_file(&format!("{APP_PARTS}.parquet"))
            {
                Ok(()) => Ok(None),
                Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(problem) => Err(problem.into()),
            }
        }
    }
}

/// The Apps of each tick of one window, as its tables keep them. A derivation reuses it from
/// window to window.
struct WindowApps {
    ticks: Vec<TickApps>,
    rankings: Vec<RankingRow>,
    /// Every tick's Apps end to end, in key order within a tick.
    totals: Vec<Totals>,
    /// Every tick's processes end to end, in order of pid and unique id within a tick.
    processes: Vec<ProcessTotals>,
    /// The rows of the parts table the window's ticks make.
    parts_rows: u64,
    /// The fewest rows the window's ticks can make, counted before any is read.
    parts_rows_least: u64,
    /// The rows the parts table keeps, which a test lowers so that a small window fills it.
    parts_rows_max: u64,
}

impl Default for WindowApps {
    fn default() -> Self {
        Self {
            ticks: Vec::new(),
            rankings: Vec::new(),
            totals: Vec::new(),
            processes: Vec::new(),
            parts_rows: 0,
            parts_rows_least: 0,
            parts_rows_max: u64::from(PARTS_ROWS_MAX),
        }
    }
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
    /// Empties the window for the ticks of the next, which make `parts_rows_least` rows of the
    /// parts or more.
    fn start(&mut self, parts_rows_least: u64) {
        self.ticks.clear();
        self.rankings.clear();
        self.totals.clear();
        self.processes.clear();
        self.parts_rows = 0;
        self.parts_rows_least = parts_rows_least;
    }

    /// Whether the window's ticks make more rows than the parts table keeps. A window whose
    /// fewest rows are already more is full before its first tick, so it holds no row of a
    /// table it will not write.
    fn parts_full(&self) -> bool {
        self.parts_rows.max(self.parts_rows_least) > self.parts_rows_max
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
            previous: tick.previous_observation,
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
