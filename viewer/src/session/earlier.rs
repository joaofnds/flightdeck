//! The older tail of the scalar context: one reading in every stretch of seconds from before the
//! full-detail context, so a chart can span the whole loaded hour without the viewer holding or
//! sending every second of it. A live session keeps each sealed minute the tail needs, read
//! once, and assembles each stride's tail from the minutes it keeps.

use std::collections::BTreeMap;

use persistence::{Error, Manifest, Result, TimeBounds};
use serde_json::Value;

use super::{CONTEXT_READERS, NS, OBSERVATIONS_MAX, Observation, Reader, Session};
use crate::data::Table;
use crate::metrics;

/// The seconds of history the longest range a chart shows holds. The viewer loads
/// `LEAD_IN_SECONDS` more before them.
pub const LOADED_SECONDS: u64 = 3600;
/// The seconds before the oldest second of a range whose readings the viewer also loads, so a
/// chart whose points each average a minute or less draws its first point at the range's oldest
/// edge, as Grafana's query reaches a rate interval before its range.
pub const LEAD_IN_SECONDS: u64 = 60;
/// The seconds of full detail the context holds, the 15 minutes of the shortest range.
pub(crate) const DETAIL_SECONDS: u64 = 900;
/// The seconds of full-detail readings a selection's charts hold: the context and the lead-in
/// before it. The older tail ends before them.
pub const CHARTED_SECONDS: u64 = DETAIL_SECONDS + LEAD_IN_SECONDS;
/// The seconds after a selected second its context holds, where the recording has them, so a
/// step to a later second shows readings the page already holds. The context still holds
/// `DETAIL_SECONDS` in all, so the rest lie before the selected second.
pub const CONTEXT_AFTER_SECONDS: u64 = 450;
/// The seconds one older reading stands for.
pub const EARLIER_STRIDE_SECONDS: u64 = 10;
/// The seconds the older tail spans, back from its limit to the lead-in before the loaded hour.
const EARLIER_SECONDS: u64 = LOADED_SECONDS + LEAD_IN_SECONDS - CHARTED_SECONDS;
/// The seconds after the tail's limit whose readings decide whether its last segment joins the
/// full-detail context.
const JOIN_SECONDS: u64 = 2 * EARLIER_STRIDE_SECONDS;
/// Older segments are numbered above any the full-detail context counts, so a segment of the
/// tail is never mistaken for one of the recent readings.
const SEGMENT_BASE: u64 = 1 << 32;
const _: () = {
    assert!(CHARTED_SECONDS < LOADED_SECONDS);
    assert!(CONTEXT_AFTER_SECONDS < DETAIL_SECONDS);
    assert!(EARLIER_STRIDE_SECONDS > 0);
    assert!(LOADED_SECONDS + LEAD_IN_SECONDS <= 65_535);
    // A tail's first second is then a stretch's first, as its limit is, so the first reading of
    // a stretch inside a minute does not depend on where the tail starts.
    assert!(EARLIER_SECONDS.is_multiple_of(EARLIER_STRIDE_SECONDS));
};

/// The seconds an older tail holds, from `first_second` up to but not including `limit_second`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TailRange {
    first_second: u64,
    limit_second: u64,
}

impl TailRange {
    /// The tail of the context that ends at `context_last_second`. It ends at the first second
    /// of the stretch that holds the first of the full-detail seconds the context's charts
    /// hold. The division floors.
    pub(crate) fn of_context(context_last_second: u64) -> Self {
        let charted_first = context_last_second.saturating_sub(CHARTED_SECONDS - 1);
        let limit_second = charted_first / EARLIER_STRIDE_SECONDS * EARLIER_STRIDE_SECONDS;

        Self {
            first_second: limit_second.saturating_sub(EARLIER_SECONDS),
            limit_second,
        }
    }

    /// Whether a minute whose readings lie within `bounds` holds a reading of the tail or of
    /// its join, as `Session::observations` skips a minute by its bounds.
    fn needs(self, bounds: Option<&TimeBounds>) -> bool {
        if self.limit_second <= self.first_second {
            return false;
        }
        let Some(bounds) = bounds else {
            return false;
        };
        if bounds.max < self.first_second * NS {
            return false;
        }

        bounds.min < self.join_second() * NS
    }

    /// Whether a reading at `wall` nanoseconds belongs to the tail.
    fn holds(self, wall: u64) -> bool {
        if wall < self.first_second * NS {
            return false;
        }

        wall < self.limit_second * NS
    }

    /// The second before which the readings after the limit decide whether the tail's last
    /// segment joins the full-detail context.
    fn join_second(self) -> u64 {
        self.limit_second + JOIN_SECONDS
    }
}

/// The sealed minutes the older tail of a live session keeps. The tail assembled from them is
/// kept by the `Session` whose list it was assembled from, so a refreshed list never answers
/// with a tail of the list before it.
#[derive(Default)]
pub(crate) struct KeptTail {
    minutes: Vec<KeptMinute>,
    #[cfg(test)]
    pub(crate) after_minute: Option<Box<dyn FnOnce()>>,
}

/// A sealed minute as the older tail keeps it.
struct KeptMinute {
    /// The minute's commitment. A listed minute whose manifest differs is read again.
    manifest: Manifest,
    /// The minute that held the publication before this minute's first when its points were
    /// made. Its points are made again once another minute holds it.
    before: Option<Manifest>,
    /// Whether the point of the minute's first publication is missing, since the minute before
    /// it was not kept when this one was read.
    first_missing: bool,
    /// Every reading of the minute with a wall time, in publication order.
    readings: Vec<Reading>,
    /// The point of the first reading of each stretch inside the minute, by publication. The
    /// first of a stretch across minutes is the first of it inside its own minute, so the tail
    /// finds the point of every reading it keeps here.
    points: BTreeMap<u64, Value>,
    last: Last,
}

/// One reading of a kept minute. It holds no index of its minute, since a reopened session
/// lists its minutes in another order once a run sorts before another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reading {
    publication: u64,
    run_tick: u64,
    wall: u64,
    continuous: Option<u64>,
}

impl Reading {
    fn of(observation: &Observation) -> Self {
        Self {
            publication: observation.publication,
            run_tick: observation.run_tick,
            wall: observation.wall,
            continuous: observation.continuous,
        }
    }

    fn observation(&self, window: usize) -> Observation {
        Observation {
            window,
            publication: self.publication,
            run_tick: self.run_tick,
            wall: self.wall,
            continuous: self.continuous,
        }
    }
}

/// What the minute after a kept minute needs of it to make the point of its first reading: the
/// reading of the kept minute's last publication, and that publication's rows of every table.
/// Each table is present even where the publication holds no rows of it, as the tables a full
/// read loads are, since `metrics::point` gives no traffic rate without the previous interfaces.
struct Last {
    reading: Option<Reading>,
    tables: MinuteTables,
}

/// The rows of every table of a minute, or of one publication of it, by table name.
type MinuteTables = Vec<(&'static str, BTreeMap<u64, Table>)>;

/// The listed minute that holds the publication before a minute's first.
#[derive(Clone, Copy)]
enum Holder<'kept> {
    /// No listed minute holds it.
    None,
    /// The listed minute at `window` holds it, and the tail keeps what the minute after it
    /// needs of it.
    Kept { window: usize, last: &'kept Last },
    /// The listed minute at this index holds it, and the tail does not keep it.
    Unread(usize),
}

impl Holder<'_> {
    fn window(self) -> Option<usize> {
        match self {
            Holder::None => None,
            Holder::Kept { window, .. } | Holder::Unread(window) => Some(window),
        }
    }
}

/// A listed minute the tail reads next, which it does not keep or keeps from before its holder
/// changed, and the minute that holds the publication before its first.
#[derive(Clone, Copy)]
struct Step<'kept> {
    window: usize,
    holder: Holder<'kept>,
}

/// The reading before the one whose point is made, where the minute is read.
enum Previous {
    Known(Option<Observation>),
    /// The minute before it holds that reading, and the tail does not keep it.
    Missing,
}

impl KeptTail {
    /// Reads each minute `range` needs that the tail does not keep, keeps it, drops the minutes
    /// it no longer needs, and assembles the tail. The minutes the next steps read are read up
    /// to `CONTEXT_READERS` at a time, each on a connection of its own, and kept one at a time
    /// in the session's order, so a canceled read loses only the minutes of the batch it was
    /// reading, and a failed read keeps the minutes before the one that failed.
    pub(crate) fn fill(&mut self, history: &Session, range: TailRange) -> Result<()> {
        // A minute is read at most twice in one fill: once to hold the reading before a needed
        // minute's first, without the point of its own first reading, and once whole when the
        // tail needs it too.
        let steps_max = 2 * history.windows.len() + 1;
        let mut batch: Vec<(usize, Result<MinuteTables>)> = Vec::new();
        for _ in 0..steps_max {
            let Some(step) = self.next(history, range) else {
                self.fill_drop(history, range);
                let points = self.assemble(history, range)?;
                history.earlier_tail.replace(Some((range, points)));
                return Ok(());
            };
            if self.fill_readings(history, range) > OBSERVATIONS_MAX {
                return Err(Error::Bound("context observations"));
            }

            if !batch.iter().any(|(window, _)| *window == step.window) {
                // Holds one batch at a time, so the fill holds no more tables than a context.
                batch.clear();
                batch = history.earlier_batch(&self.fill_batch(history, range))?;
            }
            let Some(read) = batch.iter().position(|(window, _)| *window == step.window) else {
                unreachable!("the older tail's batch misses the minute it reads next");
            };
            let tables = batch.swap_remove(read).1?;
            let minute = history.earlier_minute(step, tables)?;
            self.minutes
                .retain(|kept| kept.manifest.identity != minute.manifest.identity);
            self.minutes.push(minute);
            #[cfg(test)]
            if let Some(barrier) = self.after_minute.take() {
                barrier();
            }
        }

        unreachable!("the older tail took more steps than its minutes allow");
    }

    /// The readings in `range` of the kept minutes it needs, which stop the fill once a context
    /// could not hold them, before it reads every minute they come from.
    fn fill_readings(&self, history: &Session, range: TailRange) -> usize {
        let mut count = 0;
        for (window, manifest) in history.windows.iter().enumerate() {
            if range.needs(history.wall_bounds[window].as_ref()) {
                if let Some(minute) = self.kept(manifest) {
                    count += minute
                        .readings
                        .iter()
                        .filter(|reading| range.holds(reading.wall))
                        .count();
                }
            }
        }

        count
    }

    /// The minutes the fill reads together: the minutes the steps `next` gives would read, in
    /// their order, up to `CONTEXT_READERS`, as the tail stands before the first of them.
    fn fill_batch(&self, history: &Session, range: TailRange) -> Vec<usize> {
        let mut windows = Vec::with_capacity(CONTEXT_READERS);
        for window in 0..history.windows.len() {
            if windows.len() == CONTEXT_READERS {
                break;
            }
            if !range.needs(history.wall_bounds[window].as_ref()) {
                continue;
            }

            let holder = self.holder(history, window);
            if self.current(history, window, holder) {
                continue;
            }
            if let Holder::Unread(before) = holder {
                fill_batch_push(&mut windows, before);
            }
            fill_batch_push(&mut windows, window);
        }

        windows
    }

    /// Drops the kept minutes `range` needs neither for a reading nor as the minute before one
    /// it needs.
    fn fill_drop(&mut self, history: &Session, range: TailRange) {
        let mut retained = Vec::new();
        for window in 0..history.windows.len() {
            if range.needs(history.wall_bounds[window].as_ref()) {
                retained.push(&history.windows[window]);
                if let Some(holder) = history.earlier_holder(window) {
                    retained.push(&history.windows[holder]);
                }
            }
        }
        self.minutes
            .retain(|kept| retained.iter().any(|manifest| **manifest == kept.manifest));

        assert!(self.minutes.len() <= history.windows.len());
    }

    /// Gives `respond` the tail of `range`, where answering reads no saved history, and `None`
    /// where it would.
    pub(crate) fn answer<T>(
        &self,
        history: &Session,
        range: TailRange,
        respond: impl FnOnce(&[Value]) -> Result<T>,
    ) -> Result<Option<T>> {
        if let Some((assembled, points)) = history.earlier_tail.borrow().as_ref() {
            if *assembled == range {
                return respond(points).map(Some);
            }
        }
        if self.next(history, range).is_some() {
            return Ok(None);
        }

        let points = self.assemble(history, range)?;
        let response = respond(&points);
        history.earlier_tail.replace(Some((range, points)));
        response.map(Some)
    }

    /// The first read `range` needs, in the session's order of minutes, so a minute is read
    /// after the minute before it.
    fn next(&self, history: &Session, range: TailRange) -> Option<Step<'_>> {
        for window in 0..history.windows.len() {
            if !range.needs(history.wall_bounds[window].as_ref()) {
                continue;
            }

            let holder = self.holder(history, window);
            if self.current(history, window, holder) {
                continue;
            }

            if let Holder::Unread(before) = holder {
                return Some(Step {
                    window: before,
                    holder: self.holder(history, before),
                });
            }
            return Some(Step { window, holder });
        }

        None
    }

    /// Whether the tail keeps the minute at `window` whole, with its points made from the
    /// minute that holds the publication before its first now.
    fn current(&self, history: &Session, window: usize, holder: Holder) -> bool {
        let Some(minute) = self.kept(&history.windows[window]) else {
            return false;
        };
        if minute.first_missing {
            return false;
        }

        let before = holder.window().map(|before| &history.windows[before]);
        minute.before.as_ref() == before
    }

    fn holder(&self, history: &Session, window: usize) -> Holder<'_> {
        let Some(before) = history.earlier_holder(window) else {
            return Holder::None;
        };

        match self.kept(&history.windows[before]) {
            Some(minute) => Holder::Kept {
                window: before,
                last: &minute.last,
            },
            None => Holder::Unread(before),
        }
    }

    fn kept(&self, manifest: &Manifest) -> Option<&KeptMinute> {
        self.minutes
            .iter()
            .find(|minute| minute.manifest == *manifest)
    }

    /// The tail of `range` from the kept minutes: readings in time order, numbered in segments,
    /// thinned to the first of each stretch, and joined to the full-detail context where the
    /// readings after the limit continue its last segment. The tail keeps every minute `range`
    /// needs, with the point of every first reading of a stretch.
    fn assemble(&self, history: &Session, range: TailRange) -> Result<Vec<Value>> {
        let (mut observations, after) = self.assemble_readings(history, range)?;
        observations.sort_by_key(|value| (value.wall, value.window, value.publication));
        let segments = segment_numbers(&observations);

        let mut points = Vec::new();
        for index in thin(&observations) {
            let observation = &observations[index];
            let point = self
                .kept(&history.windows[observation.window])
                .and_then(|minute| minute.points.get(&observation.publication));
            let Some(point) = point else {
                unreachable!("the older tail keeps no point of a first reading of a stretch");
            };
            let mut point = point.clone();
            point["segment"] = (SEGMENT_BASE + segments[index]).into();
            points.push(point);
        }
        if let Some(last) = observations.last() {
            let next = after
                .iter()
                .min_by_key(|value| (value.wall, value.publication, value.window));
            if next.is_some_and(|next| metrics::compatible(last, next)) {
                let joined = SEGMENT_BASE + segments[segments.len() - 1];
                for point in &mut points {
                    if point["segment"] == joined {
                        point["segment"] = 1.into();
                    }
                }
            }
        }
        super::context_points_order(&mut points);

        Ok(points)
    }

    /// The kept readings `range` holds, and those after its limit that decide its join.
    fn assemble_readings(
        &self,
        history: &Session,
        range: TailRange,
    ) -> Result<(Vec<Observation>, Vec<Observation>)> {
        let limit_ns = range.limit_second * NS;
        let join_ns = range.join_second() * NS;
        let mut observations = Vec::new();
        let mut after = Vec::new();
        for (window, manifest) in history.windows.iter().enumerate() {
            if !range.needs(history.wall_bounds[window].as_ref()) {
                continue;
            }

            let Some(minute) = self.kept(manifest) else {
                unreachable!("the older tail misses a minute its range needs");
            };
            for reading in &minute.readings {
                if range.holds(reading.wall) {
                    observations.push(reading.observation(window));
                } else {
                    if reading.wall >= limit_ns {
                        if reading.wall < join_ns {
                            after.push(reading.observation(window));
                        }
                    }
                }
            }
            if observations.len() > OBSERVATIONS_MAX {
                return Err(Error::Bound("context observations"));
            }
        }

        Ok((observations, after))
    }
}

impl Session {
    /// The listed minute of the same run that holds the publication before the first of the
    /// minute at `window`, the first of the windows `Reader::predecessor_windows` names for
    /// that reading.
    fn earlier_holder(&self, window: usize) -> Option<usize> {
        let publication = self.windows[window].publication_first.checked_sub(1)?;

        self.predecessor_windows_holding(window, publication)
            .first()
            .copied()
    }

    /// Reads every table of each minute of `windows` whole, as `metrics::trends` reads a minute
    /// it takes points from, each minute on a context connection of its own, and gives each
    /// minute's tables or the error that stopped its read.
    fn earlier_batch(&self, windows: &[usize]) -> Result<Vec<(usize, Result<MinuteTables>)>> {
        assert!(!windows.is_empty());

        self.operation.run(|| {
            let tables =
                self.context_readers("flightdeck-earlier", windows.to_vec(), earlier_batch_minute)?;
            Ok(windows.iter().copied().zip(tables).collect())
        })
    }

    /// Makes the point of the first reading of each stretch inside the minute `step` names,
    /// whose tables the batch read as `tables`.
    fn earlier_minute(&self, step: Step, tables: MinuteTables) -> Result<KeptMinute> {
        assert!(step.window < self.windows.len());
        if let Some(before) = step.holder.window() {
            assert_ne!(before, step.window);
        }

        self.operation.attach(&self.connection)?;
        self.operation
            .run(|| self.earlier_minute_inner(step, tables))
    }

    fn earlier_minute_inner(&self, step: Step, minute_tables: MinuteTables) -> Result<KeptMinute> {
        let Step { window, holder } = step;
        let manifest = &self.windows[window];
        let mut tables = metrics::Tables::new();
        if let Holder::Kept {
            window: before,
            last,
        } = holder
        {
            for (name, rows) in &last.tables {
                tables.insert((before, *name), rows.clone());
            }
        }
        for (name, rows) in minute_tables {
            tables.insert((window, name), rows);
        }
        // The cells of the rows the minute before lends count toward the bound, as a context
        // counts its predecessor's tables.
        metrics::cells_check(metrics::trends_cell_count(&tables))?;
        let readings = earlier_minute_readings(&tables, window)?;

        let mut first_missing = false;
        let mut points = BTreeMap::new();
        for reading in earlier_minute_firsts(&readings, window) {
            let current = reading.observation(window);
            match earlier_minute_previous(&readings, &current, manifest, holder) {
                Previous::Missing => first_missing = true,
                Previous::Known(previous) => {
                    let point = metrics::trends_point(
                        &self.reader(),
                        &tables,
                        &current,
                        previous.as_ref(),
                        0,
                    )?;
                    points.insert(reading.publication, point);
                }
            }
        }

        let last = earlier_minute_last(&tables, &readings, window, manifest);
        Ok(KeptMinute {
            manifest: manifest.clone(),
            before: holder.window().map(|before| self.windows[before].clone()),
            first_missing,
            readings: readings.into_values().collect(),
            points,
            last,
        })
    }
}

/// Adds `window` to the minutes a batch reads, once, while it holds fewer than
/// `CONTEXT_READERS`.
fn fill_batch_push(windows: &mut Vec<usize>, window: usize) {
    if windows.len() < CONTEXT_READERS {
        if !windows.contains(&window) {
            windows.push(window);
        }
    }
}

/// Reads every table of the minute at `window` whole, once, through `reader`.
fn earlier_batch_minute(reader: &Reader<'_>, window: usize) -> Result<MinuteTables> {
    let mut cell_count = 0;
    let mut tables = Vec::with_capacity(metrics::TABLE_NAMES.len());
    for name in metrics::TABLE_NAMES {
        let rows = metrics::table_read(reader, window, name, &mut cell_count)?;
        tables.push((name, rows));
    }

    Ok(tables)
}

/// The readings of the minute at `window` that have a wall time, by publication.
fn earlier_minute_readings(
    tables: &metrics::Tables,
    window: usize,
) -> Result<BTreeMap<u64, Reading>> {
    let Some(ticks) = tables.get(&(window, "ticks")) else {
        unreachable!("the older tail reads a minute without its ticks");
    };

    let mut readings = BTreeMap::new();
    for table in ticks.values() {
        for row in &table.rows {
            if table.integer(row, "wall_ns")?.is_some() {
                let observation = Observation::from_row(table, row, window)?;
                readings.insert(observation.publication, Reading::of(&observation));
            }
        }
    }

    Ok(readings)
}

/// The first reading of each stretch inside the minute at `window`, in time order as the tail
/// sorts them.
fn earlier_minute_firsts(readings: &BTreeMap<u64, Reading>, window: usize) -> Vec<Reading> {
    let mut sorted: Vec<Observation> = readings
        .values()
        .map(|reading| reading.observation(window))
        .collect();
    sorted.sort_by_key(|value| (value.wall, value.publication));

    thin(&sorted)
        .into_iter()
        .map(|index| Reading::of(&sorted[index]))
        .collect()
}

/// The reading before `current`, publication − 1 of the same run, as `metrics::trends` finds
/// it: in the minute itself, or in the minute `holder` names where the tail keeps it.
fn earlier_minute_previous(
    readings: &BTreeMap<u64, Reading>,
    current: &Observation,
    manifest: &Manifest,
    holder: Holder,
) -> Previous {
    let Some(publication) = current.publication.checked_sub(1) else {
        return Previous::Known(None);
    };
    if publication >= manifest.publication_first {
        let previous = readings.get(&publication);
        return Previous::Known(previous.map(|previous| previous.observation(current.window)));
    }

    match holder {
        Holder::Kept { window, last } => {
            let previous = last
                .reading
                .as_ref()
                .filter(|kept| kept.publication == publication)
                .map(|kept| kept.observation(window));
            Previous::Known(previous)
        }
        Holder::Unread(_) => Previous::Missing,
        Holder::None => Previous::Known(None),
    }
}

/// What the minute after the one at `window` needs of it: its last publication's reading and
/// rows.
fn earlier_minute_last(
    tables: &metrics::Tables,
    readings: &BTreeMap<u64, Reading>,
    window: usize,
    manifest: &Manifest,
) -> Last {
    let publication = manifest.publication_end.saturating_sub(1);
    let tables = metrics::TABLE_NAMES
        .iter()
        .map(|name| {
            let Some(publications) = tables.get(&(window, *name)) else {
                unreachable!("the older tail reads a minute without one of its tables");
            };
            let rows = publications
                .get(&publication)
                .map(|rows| BTreeMap::from([(publication, rows.clone())]))
                .unwrap_or_default();
            (*name, rows)
        })
        .collect();

    Last {
        reading: readings.get(&publication).copied(),
        tables,
    }
}

/// The index of the first observation of each stretch of seconds, from observations in time
/// order.
fn thin(observations: &[Observation]) -> Vec<usize> {
    let mut kept = Vec::new();
    let mut stretch = None;
    for (index, observation) in observations.iter().enumerate() {
        let current = observation.wall / NS / EARLIER_STRIDE_SECONDS;
        if stretch != Some(current) {
            kept.push(index);
            stretch = Some(current);
        }
    }

    kept
}

/// The segment each observation belongs to, counted from one, where a segment ends at any break
/// in the readings.
fn segment_numbers(observations: &[Observation]) -> Vec<u64> {
    let mut numbers = Vec::with_capacity(observations.len());
    let mut segment = 0;
    for (index, observation) in observations.iter().enumerate() {
        let unbroken = index
            .checked_sub(1)
            .is_some_and(|previous| metrics::compatible(&observations[previous], observation));
        if !unbroken {
            segment += 1;
        }
        numbers.push(segment);
    }

    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(second: u64, publication: u64) -> Observation {
        Observation {
            window: 0,
            publication,
            run_tick: publication,
            wall: second * NS + 500,
            continuous: Some(publication * NS),
        }
    }

    #[test]
    fn thinning_keeps_the_first_observation_of_each_stretch() {
        // Goal: an older tail keeps one reading per ten seconds, even where seconds are missing.
        // Method: thin observations that fill one stretch, skip one, and start another.
        let observations: Vec<_> = [100, 101, 109, 110, 119, 140, 141]
            .iter()
            .zip(1..)
            .map(|(second, publication)| observation(*second, publication))
            .collect();

        assert_eq!(thin(&observations), [0, 3, 5]);
        assert_eq!(thin(&[]), Vec::<usize>::new());
    }

    #[test]
    fn segments_break_where_the_readings_do() {
        // Goal: the tail draws no line across a missed stretch of readings.
        // Method: number consecutive observations around a gap of missing publications.
        let observations = [
            observation(100, 1),
            observation(101, 2),
            observation(102, 3),
            observation(200, 40),
            observation(201, 41),
        ];

        assert_eq!(segment_numbers(&observations), [1, 1, 1, 2, 2]);
        assert!(segment_numbers(&[]).is_empty());
    }
}
