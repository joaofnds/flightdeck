//! The older tail of the scalar context: one reading in every stretch of seconds from before the
//! full-detail context, so a chart can span the whole loaded hour without the viewer holding or
//! sending every second of it. A saved session keeps each sealed minute the tail needs, read
//! once, and assembles each stride's tail from the minutes it keeps.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use persistence::retention::Retained;
use persistence::{Error, Manifest, Result, TimeBounds};
use serde_json::Value;

use super::earlier_reach::Reach;
use super::{CONTEXT_READERS, NS, OBSERVATIONS_MAX, Observation, Session};
use crate::metric_view::Sources;
use crate::metrics;
use crate::recording::MachineMinute;
use crate::store::{MachinePlace, reading_at, stretch_of_wall_second};

/// The seconds of history the longest range a chart shows holds. The viewer loads
/// `LEAD_IN_SECONDS` more before them.
pub const LOADED_SECONDS: u64 = 3600;
/// The seconds before the oldest second of a range whose readings the viewer also loads.
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
/// The bytes an older tail's points take once written out, at most.
const CONTEXT_BYTES_MAX: usize = 8 * 1024 * 1024;
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

    /// The last second of a context whose tail this could be, which a session's span must hold.
    pub(crate) fn context_last_second(self) -> u64 {
        self.join_second().saturating_add(CHARTED_SECONDS)
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

/// The older tail last assembled for a range, in time order, split where the last answer began,
/// so a read whose first second leaves the split where it is shares that answer's list.
pub(crate) struct AssembledTail {
    range: TailRange,
    /// The points before the first the last answer held.
    before: Vec<Value>,
    /// The points from the first the last answer held, as the JSON list it shares.
    answer: Arc<Value>,
    /// Whether `answer` is known to pass the bound.
    bounded: bool,
}

impl AssembledTail {
    /// The tail of `points`, which `context_points_order` sorted, so their wall seconds never
    /// fall, as the split of `answer` needs.
    fn new(range: TailRange, points: Vec<Value>) -> Self {
        Self::new_in_wall_order(&points);

        Self {
            range,
            before: Vec::new(),
            answer: Arc::new(Value::Array(points)),
            bounded: false,
        }
    }

    /// Asserts that each point stands no earlier than the point before it. A point with no wall
    /// time stands first, as `context_points_order` puts it, and is never held.
    fn new_in_wall_order(points: &[Value]) {
        for pair in points.windows(2) {
            assert!(
                pair[0]["wall_ns"].as_u64() <= pair[1]["wall_ns"].as_u64(),
                "An older tail's points are in time order"
            );
        }
    }

    /// The points from `first_second` on, as a JSON list a context can carry. The split moves
    /// only where a point crosses `first_second`, which copies the list once where a page still
    /// holds the last answer. The points are in time order, so the points held are those after
    /// the split. The bound is `context_bytes_of` alone, which no caller replaces, and it is
    /// checked again only after points came back into the answer.
    fn answer(&mut self, first_second: u64) -> Result<Arc<Value>> {
        let held = |point: &Value| {
            point["wall_ns"]
                .as_u64()
                .is_some_and(|wall| wall / NS >= first_second)
        };
        let points = assembled_points(&self.answer);
        if points.first().is_some_and(|point| !held(point)) {
            // A cut leaves fewer points and fewer bytes of the list, so a list that passed the
            // bound passes it still, and one that failed is checked again.
            let count = points.partition_point(|point| !held(point));
            let points = assembled_points_mut(&mut self.answer);
            self.before.extend(points.drain(..count));
        } else if self.before.last().is_some_and(held) {
            let index = self.before.partition_point(|point| !held(point));
            let moved = self.before.split_off(index);
            let points = assembled_points_mut(&mut self.answer);
            drop(points.splice(..0, moved));
            self.bounded = false;
        }

        if !self.bounded {
            context_bytes_of(assembled_points(&self.answer))?;
            self.bounded = true;
        }

        Ok(Arc::clone(&self.answer))
    }
}

/// Refuses a tail of more points or bytes than a context carries.
fn context_bytes_of(points: &[Value]) -> Result<()> {
    if points.len() > OBSERVATIONS_MAX {
        return Err(Error::Bound("context observations"));
    }

    if serde_json::to_vec(points)
        .map_err(std::io::Error::other)?
        .len()
        > CONTEXT_BYTES_MAX
    {
        return Err(Error::Bound("context bytes"));
    }

    Ok(())
}

fn assembled_points(answer: &Value) -> &[Value] {
    let Some(points) = answer.as_array() else {
        unreachable!("An older tail is a JSON list");
    };

    points
}

/// The points of `answer`, copied first where a page still holds them.
fn assembled_points_mut(answer: &mut Arc<Value>) -> &mut Vec<Value> {
    let Some(points) = Arc::make_mut(answer).as_array_mut() else {
        unreachable!("An older tail is a JSON list");
    };

    points
}

/// The sealed minutes the older tail of a saved session keeps. The tail assembled from them is
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
    /// finds here the point of every stretch whose first reading the period still covers.
    points: BTreeMap<u64, Value>,
    /// How far the minute's readings of each stretch went, by the stretch's wall seconds divided
    /// by the stride, which the tail adds up across the minutes a stretch spans.
    reaches: BTreeMap<u64, StretchPart>,
    last: Last,
}

/// What one minute's readings of one stretch reached.
struct StretchPart {
    /// The clock of the first of those readings. The tail leaves out a part whose first reading
    /// the period no longer covers, so no chart finds a peak the recording has let go of.
    first_continuous: Option<u64>,
    reach: Reach,
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
struct Last {
    reading: Option<Reading>,
    minute: MachineMinute,
}

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
    /// to `CONTEXT_READERS` at a time, each on a thread of its own, and kept one at a time
    /// in the session's order, so a canceled read loses only the minutes of the batch it was
    /// reading, and a failed read keeps the minutes before the one that failed.
    pub(crate) fn fill(&mut self, history: &Session, range: TailRange) -> Result<()> {
        // A minute is read at most twice in one fill: once to hold the reading before a needed
        // minute's first, without the point of its own first reading, and once whole when the
        // tail needs it too.
        let steps_max = 2 * history.windows.len() + 1;
        let retained = history.reader().retained();
        let needed = history.earlier_needed(range, &retained);
        let mut batch: Vec<(usize, Result<MachineMinute>)> = Vec::new();
        for _ in 0..steps_max {
            let Some(step) = self.next(history, &needed) else {
                self.fill_drop(history, &needed);
                let points = self.assemble(history, range, &needed, &retained)?;
                history
                    .earlier_tail
                    .replace(Some(AssembledTail::new(range, points)));
                return Ok(());
            };
            if self.fill_readings(history, range, &needed) > OBSERVATIONS_MAX {
                return Err(Error::Bound("context observations"));
            }

            if !batch.iter().any(|(window, _)| *window == step.window) {
                // Holds one batch at a time, so the fill holds no more minutes than a context.
                batch.clear();
                batch =
                    history.earlier_batch(&self.fill_batch(history, &needed, CONTEXT_READERS))?;
            }
            let Some(read) = batch.iter().position(|(window, _)| *window == step.window) else {
                unreachable!("the older tail's batch misses the minute it reads next");
            };
            let read_minute = batch.swap_remove(read).1?;
            let minute = history.earlier_minute(step, &read_minute)?;
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
    fn fill_readings(&self, history: &Session, range: TailRange, needed: &[usize]) -> usize {
        let mut count = 0;
        for &window in needed {
            if let Some(minute) = self.kept(&history.windows[window]) {
                count += minute
                    .readings
                    .iter()
                    .filter(|reading| range.holds(reading.wall))
                    .count();
            }
        }

        count
    }

    /// Whether the fill of `range` lists no minutes again and reads the minutes it lacks in one
    /// batch, whose minutes each read on a thread of their own.
    pub(crate) fn fills_in_one_batch(&self, history: &Session, range: TailRange) -> Result<bool> {
        if !history.span_holds(range.context_last_second())? {
            return Ok(false);
        }
        let retained = history.reader().retained();
        let needed = history.earlier_needed(range, &retained);
        // One minute more than a batch holds tells a fill of one batch from a fill of more.
        let batch = self.fill_batch(history, &needed, CONTEXT_READERS + 1);

        Ok(batch.len() <= CONTEXT_READERS)
    }

    /// The minutes the fill reads together: the minutes the steps `next` gives would read, in
    /// their order, up to `windows_max`, as the tail stands before the first of them.
    fn fill_batch(&self, history: &Session, needed: &[usize], windows_max: usize) -> Vec<usize> {
        let mut windows = Vec::with_capacity(windows_max);
        for &window in needed {
            if windows.len() == windows_max {
                break;
            }

            let holder = self.holder(history, window);
            if self.current(history, window, holder) {
                continue;
            }
            if let Holder::Unread(before) = holder {
                fill_batch_push(&mut windows, before, windows_max);
            }
            fill_batch_push(&mut windows, window, windows_max);
        }

        windows
    }

    /// Drops the kept minutes the tail needs neither for a reading nor as the minute before one
    /// it needs.
    fn fill_drop(&mut self, history: &Session, needed: &[usize]) {
        let mut retained = Vec::new();
        for &window in needed {
            retained.push(&history.windows[window]);
            if let Some(holder) = history.earlier_holder(window) {
                retained.push(&history.windows[holder]);
            }
        }
        self.minutes
            .retain(|kept| retained.iter().any(|manifest| **manifest == kept.manifest));

        assert!(self.minutes.len() <= history.windows.len());
    }

    /// The points of the tail of `range` from `first_second` on, as a JSON list a context can
    /// carry, where answering reads no saved history, and `None` where it would. A read that
    /// finds the range and the first point of the read before shares that read's list.
    pub(crate) fn answer(
        &self,
        history: &Session,
        range: TailRange,
        first_second: u64,
    ) -> Result<Option<Arc<Value>>> {
        if !history.span_holds(range.context_last_second())? {
            return Ok(None);
        }
        let assembled = history
            .earlier_tail
            .borrow()
            .as_ref()
            .is_some_and(|tail| tail.range == range);
        if !assembled {
            let retained = history.reader().retained();
            let needed = history.earlier_needed(range, &retained);
            if self.next(history, &needed).is_some() {
                return Ok(None);
            }
            let points = self.assemble(history, range, &needed, &retained)?;
            history
                .earlier_tail
                .replace(Some(AssembledTail::new(range, points)));
        }

        let mut tail = history.earlier_tail.borrow_mut();
        let Some(tail) = tail.as_mut() else {
            unreachable!("The older tail was assembled above");
        };

        tail.answer(first_second).map(Some)
    }

    /// The first read the tail needs, in the session's order of minutes, so a minute is read
    /// after the minute before it.
    fn next(&self, history: &Session, needed: &[usize]) -> Option<Step<'_>> {
        for &window in needed {
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
    /// needs, with the point of every first reading of a stretch, so a stretch has no point only
    /// where `retained` no longer covers its first reading.
    fn assemble(
        &self,
        history: &Session,
        range: TailRange,
        needed: &[usize],
        retained: &Retained,
    ) -> Result<Vec<Value>> {
        let (mut observations, after) = self.assemble_readings(history, range, needed, retained)?;
        observations.sort_by_key(|value| (value.wall, value.window, value.publication));
        let segments = segment_numbers(&observations);
        let reaches = self.assemble_reaches(history, needed, retained);

        let mut points = Vec::new();
        for index in thin(&observations) {
            let observation = &observations[index];
            let point = self
                .kept(&history.windows[observation.window])
                .and_then(|minute| minute.points.get(&observation.publication));
            // A stretch whose first second expired has lost the one reading that stood for it. Any
            // other stretch with no point lies in a file whose clocks no run read.
            let Some(point) = point else {
                if self.assemble_stretch_expired(history, observation, retained) {
                    continue;
                }
                return Err(Error::Bound("older tail point"));
            };
            let mut point = point.clone();
            point["segment"] = (SEGMENT_BASE + segments[index]).into();
            if let Some(reach) = reaches.get(&stretch_of_wall_second(observation.wall / NS)) {
                point["reach"] = reach.json();
            }
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
        // The seconds between the tail's limit and the first full-detail second lie in no point,
        // so the point after the join takes what their stretch reached.
        let joined = reaches.get(&stretch_of_wall_second(range.limit_second));
        if let Some((last, reach)) = points.last_mut().zip(joined) {
            last["reach_after"] = reach.json();
        }

        Ok(points)
    }

    /// How far the readings of each stretch the period still covers went, in every minute they
    /// lie in.
    fn assemble_reaches(
        &self,
        history: &Session,
        needed: &[usize],
        retained: &Retained,
    ) -> BTreeMap<u64, Reach> {
        let mut reaches: BTreeMap<u64, Reach> = BTreeMap::new();
        for &window in needed {
            let manifest = &history.windows[window];
            let parts = self.kept(manifest).map(|minute| &minute.reaches);
            for (stretch, part) in parts.into_iter().flatten() {
                if retained.holds(manifest.identity.run.boot, part.first_continuous) {
                    reaches.entry(*stretch).or_default().merge(&part.reach);
                }
            }
        }

        reaches
    }

    /// Whether a reading before `observation` in its minute and its stretch has expired.
    fn assemble_stretch_expired(
        &self,
        history: &Session,
        observation: &Observation,
        retained: &Retained,
    ) -> bool {
        let manifest = &history.windows[observation.window];
        let stretch = stretch_of_wall_second(observation.wall / NS);
        self.kept(manifest).is_some_and(|minute| {
            minute.readings.iter().any(|reading| {
                stretch_of_wall_second(reading.wall / NS) == stretch
                    && reading.publication < observation.publication
                    && !retained.holds(manifest.identity.run.boot, reading.continuous)
            })
        })
    }

    /// The kept readings `range` holds, and those after its limit that decide its join.
    fn assemble_readings(
        &self,
        history: &Session,
        range: TailRange,
        needed: &[usize],
        retained: &Retained,
    ) -> Result<(Vec<Observation>, Vec<Observation>)> {
        let limit_ns = range.limit_second * NS;
        let join_ns = range.join_second() * NS;
        let mut observations = Vec::new();
        let mut after = Vec::new();
        for &window in needed {
            let manifest = &history.windows[window];
            let Some(minute) = self.kept(manifest) else {
                unreachable!("the older tail misses a minute its range needs");
            };
            for reading in &minute.readings {
                if !retained.holds(manifest.identity.run.boot, reading.continuous) {
                    continue;
                }
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
    /// The minutes of the list that hold a reading of `range` or of its join and may hold one
    /// `retained` covers.
    fn earlier_needed(&self, range: TailRange, retained: &Retained) -> Vec<usize> {
        let mut needed = Vec::new();
        for (window, manifest) in self.windows.iter().enumerate() {
            if range.needs(self.wall_bounds[window].as_ref())
                && super::window_may_hold_retained(manifest, retained)
            {
                needed.push(window);
            }
        }

        needed
    }

    /// The listed minute of the same run that holds the publication before the first of the
    /// minute at `window`, the first of the windows `Reader::predecessor_windows` names for
    /// that reading.
    fn earlier_holder(&self, window: usize) -> Option<usize> {
        let publication = self.windows[window].publication_first.checked_sub(1)?;

        self.predecessor_windows_holding(window, publication)
            .first()
            .copied()
    }

    /// Decodes each minute of `windows` whole, as `metrics::trends` does a minute it takes points
    /// from, each minute on a thread of its own, and gives each minute or the error that stopped
    /// its read.
    fn earlier_batch(&self, windows: &[usize]) -> Result<Vec<(usize, Result<MachineMinute>)>> {
        assert!(!windows.is_empty());

        self.operation.run(|| {
            let minutes =
                self.context_readers("flightdeck-earlier", windows.to_vec(), |reader, window| {
                    let minute = reader.machine_minute(window)?;
                    metrics::minutes_bytes_check(minute.heap_bytes())?;

                    Ok(minute)
                })?;
            Ok(windows.iter().copied().zip(minutes).collect())
        })
    }

    /// Makes the point of the first reading of each stretch inside the minute `step` names, and
    /// what every reading of each stretch reached, from the minute the batch read.
    fn earlier_minute(&self, step: Step, minute: &MachineMinute) -> Result<KeptMinute> {
        assert!(step.window < self.windows.len());
        if let Some(before) = step.holder.window() {
            assert_ne!(before, step.window);
        }

        self.operation
            .run(|| self.earlier_minute_inner(step, minute))
    }

    fn earlier_minute_inner(&self, step: Step, minute: &MachineMinute) -> Result<KeptMinute> {
        let Step { window, holder } = step;
        let manifest = &self.windows[window];
        // The rows the minute before lends count toward the bound, as a context counts its
        // predecessor's minute.
        let lent = match holder {
            Holder::Kept { last, .. } => last.minute.heap_bytes(),
            Holder::None | Holder::Unread(_) => 0,
        };
        metrics::minutes_bytes_check(minute.heap_bytes() + lent)?;
        let readings = earlier_minute_readings(minute, window);

        let firsts: BTreeSet<u64> = earlier_minute_firsts(&readings, window)
            .iter()
            .map(|first| first.publication)
            .collect();
        let mut first_missing = false;
        let mut points = BTreeMap::new();
        let mut reaches: BTreeMap<u64, StretchPart> = BTreeMap::new();
        let mut unread = None;
        for reading in readings.values() {
            let current = reading.observation(window);
            match earlier_minute_previous(&readings, &current, manifest, holder) {
                Previous::Missing => first_missing = true,
                Previous::Known(previous) => {
                    let first = firsts.contains(&reading.publication);
                    let point =
                        self.earlier_minute_point(minute, holder, &current, previous.as_ref());
                    // A reading no chart draws costs only what it reached.
                    let point = match point {
                        Err(Error::Bound(reason)) if !first => {
                            unread = Some(reason);
                            continue;
                        }
                        point => point?,
                    };
                    earlier_minute_reach(&mut reaches, manifest, reading, &point);
                    if first {
                        points.insert(reading.publication, point);
                    }
                }
            }
        }

        if let Some(reason) = unread {
            eprintln!("flightdeck history: a saved minute left readings out of a reach: {reason}");
        }

        let last = earlier_minute_last(minute, &readings, manifest);
        Ok(KeptMinute {
            manifest: manifest.clone(),
            before: holder.window().map(|before| self.windows[before].clone()),
            first_missing,
            readings: readings.into_values().collect(),
            points,
            reaches,
            last,
        })
    }

    /// The point of `current`, whose rates come from `previous`, a reading of `minute` or of the
    /// minute before where the tail keeps it.
    fn earlier_minute_point(
        &self,
        minute: &MachineMinute,
        holder: Holder,
        current: &Observation,
        previous: Option<&Observation>,
    ) -> Result<Value> {
        let reading = reading_at(minute, current.publication)
            .ok_or(Error::Bound("missing required recorded table rows"))?;
        let before = previous.and_then(|previous| {
            let holding = match holder {
                Holder::Kept { window, last } if window == previous.window => &last.minute,
                _ => minute,
            };
            reading_at(holding, previous.publication)
        });

        metrics::trends_point(
            &self.reader(),
            &Sources {
                current: &reading,
                previous: before.as_ref(),
            },
            current,
            previous,
            0,
        )
    }
}

/// Adds the reading whose point is `point` to what its stretch reached.
fn earlier_minute_reach(
    reaches: &mut BTreeMap<u64, StretchPart>,
    manifest: &Manifest,
    reading: &Reading,
    point: &Value,
) {
    let part = reaches
        .entry(stretch_of_wall_second(reading.wall / NS))
        .or_insert_with(|| StretchPart {
            first_continuous: reading.continuous,
            reach: Reach::default(),
        });
    part.reach.add(
        point,
        &MachinePlace {
            boot: manifest.identity.run.boot.0,
            run: manifest.identity.run.run.0,
            publication: reading.publication,
            wall_ns: reading.wall,
        },
    );
}

/// Adds `window` to the minutes a batch reads, once, while it holds fewer than `windows_max`.
fn fill_batch_push(windows: &mut Vec<usize>, window: usize, windows_max: usize) {
    if windows.len() < windows_max {
        if !windows.contains(&window) {
            windows.push(window);
        }
    }
}

/// The readings of the minute at `window` that have a wall time, by publication.
fn earlier_minute_readings(minute: &MachineMinute, window: usize) -> BTreeMap<u64, Reading> {
    minute
        .ticks
        .iter()
        .filter_map(|tick| Observation::of_tick(tick, window))
        .map(|observation| (observation.publication, Reading::of(&observation)))
        .collect()
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

/// What the minute after `minute` needs of it: its last publication's reading and rows.
fn earlier_minute_last(
    minute: &MachineMinute,
    readings: &BTreeMap<u64, Reading>,
    manifest: &Manifest,
) -> Last {
    let publication = manifest.publication_end.saturating_sub(1);
    let reading = readings.get(&publication).copied();
    let kept = reading.and_then(|_| reading_at(minute, publication));
    let minute = kept
        .as_ref()
        .map_or_else(MachineMinute::default, crate::store::Reading::copied_minute);
    let copied = collector::u64_from_usize(minute.heap_bytes());
    persistence::work::record(|counts| counts.minute_copy_bytes += copied);

    Last { reading, minute }
}

/// The index of the first observation of each stretch of seconds, from observations in time
/// order.
fn thin(observations: &[Observation]) -> Vec<usize> {
    let mut kept = Vec::new();
    let mut stretch = None;
    for (index, observation) in observations.iter().enumerate() {
        let current = stretch_of_wall_second(observation.wall / NS);
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
    fn a_minute_counts_the_minute_before_it_lends_toward_what_a_reader_may_hold() {
        // Goal: the older tail stays inside the bound on what a reader holds, as a context that
        // keeps its predecessor's minute does. Method: read an empty minute beside a kept minute
        // past the bound, which a read of either alone would not reach.
        let fixture = crate::session::retention_tests::saved_seconds();
        let session = Session::open(&fixture.root).unwrap();
        let bound = metrics::READER_MINUTES_BYTES_MAX;
        let last = Last {
            reading: None,
            minute: MachineMinute {
                fans: Vec::with_capacity(bound / size_of::<crate::recording::FanRow>() + 1),
                ..MachineMinute::default()
            },
        };
        let step = Step {
            window: 0,
            holder: Holder::Kept {
                window: 1,
                last: &last,
            },
        };

        let result = session
            .earlier_minute(step, &MachineMinute::default())
            .map(|_kept| ());

        assert!(
            matches!(result, Err(Error::Bound("context minute bytes"))),
            "{result:?}"
        );
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

    #[test]
    fn an_assembled_tail_checks_its_bound_only_once_its_answer_changes() {
        // Goal: a read whose first second leaves the kept answer as it was shares that answer
        // and checks no bound, and a read that moves its first point, forward or back, gives
        // the points from its first second. Method: assemble four points ten seconds apart,
        // answer from first seconds that move forward and back across them, and count what each
        // read allocates, since the bound writes the points out and a read that shares its
        // answer allocates nothing.
        let points = (0..4)
            .map(|stretch| serde_json::json!({ "wall_ns": (100 + stretch * 10) * NS }))
            .collect();
        let mut tail = AssembledTail::new(TailRange::of_context(10_000), points);

        let (answers, allocated): (Vec<Arc<Value>>, Vec<bool>) =
            [100, 101, 105, 110, 115, 100, 100]
                .into_iter()
                .map(|first_second| {
                    let (answer, allocations) =
                        crate::allocation_tests::observe(|| tail.answer(first_second));
                    (answer.unwrap(), allocations > 0)
                })
                .unzip();

        let lengths: Vec<usize> = answers
            .iter()
            .map(|answer| assembled_points(answer).len())
            .collect();
        assert_eq!(lengths, [4, 3, 3, 3, 2, 4, 4]);
        assert_eq!(allocated, [true, true, false, false, true, true, false]);
        assert!(Arc::ptr_eq(&answers[1], &answers[2]));
        assert!(Arc::ptr_eq(&answers[1], &answers[3]));
        assert!(Arc::ptr_eq(&answers[5], &answers[6]));
        assert_eq!(answers[5], answers[0]);
    }

    #[test]
    fn an_assembled_tail_whose_answer_fails_its_bound_fails_every_read_of_it() {
        // Goal: a tail too large for a context never reaches a page, however often it is read,
        // and a read that brings a left point back checks the bound again. Method: assemble one
        // point more than a context carries, one second apart, and answer from its first second
        // twice, from the second after, which leaves the first point out, and from its first
        // second again.
        let points = (0..=OBSERVATIONS_MAX)
            .map(|index| {
                let second = 100 + u64::try_from(index).unwrap();
                serde_json::json!({ "wall_ns": second * NS })
            })
            .collect();
        let mut tail = AssembledTail::new(TailRange::of_context(10_000), points);

        let first = tail.answer(100);
        let second = tail.answer(100);
        let cut = tail.answer(101);
        let returned = tail.answer(100);

        assert!(matches!(first, Err(Error::Bound("context observations"))));
        assert!(matches!(second, Err(Error::Bound("context observations"))));
        assert_eq!(assembled_points(&cut.unwrap()).len(), OBSERVATIONS_MAX);
        assert!(matches!(
            returned,
            Err(Error::Bound("context observations"))
        ));
    }

    #[test]
    #[should_panic(expected = "An older tail's points are in time order")]
    fn an_assembled_tail_whose_points_are_out_of_time_order_stops_the_program() {
        // Goal: a tail whose points come out of time order stops the program, since its split
        // would hold other points than those from a read's first second. Method: assemble two
        // points, the later first.
        let points = vec![
            serde_json::json!({ "wall_ns": 110 * NS }),
            serde_json::json!({ "wall_ns": 100 * NS }),
        ];

        drop(AssembledTail::new(TailRange::of_context(10_000), points));
    }

    #[test]
    fn context_rejects_more_than_4096_scalar_points() {
        // Goal: the context budget refuses a result larger than the public observation bound.
        // Method: measure 4097 individually tiny scalar values, whose bytes fit the other bound.
        let points = vec![serde_json::json!({"cpu_percent":0}); 4097];

        let result = context_bytes_of(&points);

        assert!(matches!(result, Err(Error::Bound("context observations"))));
    }

    #[test]
    fn context_rejects_more_than_eight_mib_of_serialized_scalars() {
        // Goal: a small point count cannot evade the separate serialized context byte bound.
        // Method: encode one 8 MiB value, whose JSON framing exceeds the 8 MiB contract.
        let points = [Value::String("x".repeat(8 * 1024 * 1024))];

        let result = context_bytes_of(&points);

        assert!(matches!(result, Err(Error::Bound("context bytes"))));
    }
}
