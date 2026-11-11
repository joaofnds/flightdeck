//! Where the event log keeps each event. The log is cut into segments of `SEGMENT_EVENTS`
//! records, and a table in the ring names the segment that holds each span of that many event
//! numbers. The writer starts a span in the segment of the oldest span once no tick the ring
//! holds has an event there and a reader has saved its events. Otherwise it takes a segment it
//! never wrote, and where it has none left it starts the span in the oldest span's segment
//! whether or not a reader saved it, as a log that numbers its records overwrites its oldest.
//!
//! A log that numbers its records round the whole of its room writes every page of it in time,
//! however few events its ticks hold, and memory the writer has written stays charged to it
//! until it exits: 126 MB for the collector's log. A segmented log writes the segments that the
//! ticks in the ring and the events no reader had saved needed at their most.

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use super::layout::{HEADER_EVENT_OLDEST, Held, Layout, usize_from};
use crate::u64_from_usize;

/// The events a segment holds, where the log has room for at least as many. A segment of the
/// collector's log is 196,608 bytes, twelve pages of 16 KiB.
pub const SEGMENT_EVENTS: u64 = 1_024;

/// The words of a span's entry in the table: one more than the span, zero where the entry
/// names no span, and then the span's segment.
const ENTRY_WORDS: usize = 2;
const ENTRY_SPAN: usize = 0;
const ENTRY_SEGMENT: usize = 1;

/// How a ring's event log is cut into segments, and where its table lies.
#[derive(Debug, Clone)]
pub(super) struct Segments {
    /// The events each segment holds.
    segment_events: u64,
    /// The segments of the log, one more than its events fill, since the events of the ticks
    /// the ring holds can start part-way into one span and end part-way into another.
    segment_count: usize,
    /// The first word of the first segment, from the ring's start.
    records_start: usize,
    record_words: usize,
    /// The table's first word, from the ring's start.
    table_start: usize,
}

impl Segments {
    /// The segments of a log with room for `event_capacity` events of `record_words` each,
    /// whose first record lies at `records_start`. The table follows the last segment.
    pub(super) fn plan(event_capacity: u64, records_start: usize, record_words: usize) -> Self {
        assert!(event_capacity > 0);
        assert!(record_words > 0);

        let segment_events = SEGMENT_EVENTS.min(event_capacity);
        let segment_count = usize_from(event_capacity.div_ceil(segment_events)) + 1;
        let record_count = segment_count * usize_from(segment_events);
        Self {
            segment_events,
            segment_count,
            records_start,
            record_words,
            table_start: records_start + record_count * record_words,
        }
    }

    /// The first word after the table, from the ring's start.
    pub(super) fn end(&self) -> usize {
        self.table_start + self.segment_count * ENTRY_WORDS
    }

    /// The words the segments and the table take.
    pub(super) fn word_count(&self) -> usize {
        self.end() - self.records_start
    }

    pub(super) fn segment_events(&self) -> u64 {
        self.segment_events
    }

    /// The span of event numbers that holds `number`, counting spans from 0.
    fn span(&self, number: u64) -> u64 {
        assert!(number > 0);

        (number - 1) / self.segment_events
    }

    /// The first event of `span`.
    fn span_first(&self, span: u64) -> u64 {
        span * self.segment_events + 1
    }

    /// The index of the entry of `span` in the table, counting entries from 0.
    fn entry_index(&self, span: u64) -> usize {
        usize_from(span % u64_from_usize(self.segment_count))
    }

    /// The first word of the entry of `span` in the table, from the ring's start.
    fn entry_start(&self, span: u64) -> usize {
        self.table_start + self.entry_index(span) * ENTRY_WORDS
    }

    /// The words of the record of event `number` in `segment`, from the ring's start.
    fn record(&self, segment: usize, number: u64) -> Range<usize> {
        assert!(segment < self.segment_count);
        assert!(number > 0);

        let index = segment * usize_from(self.segment_events)
            + usize_from((number - 1) % self.segment_events);
        let start = self.records_start + index * self.record_words;
        start..start + self.record_words
    }
}

/// The writer's own account of the spans the log holds, kept apart from the table in the
/// mapping, which another process can write, so the writer places events by its own work.
pub(super) struct Placement {
    /// How the log is cut, as the layout planned it.
    log: Segments,
    /// The segment of each span the log holds, at the index of the span's entry in the table.
    segments: Vec<usize>,
    /// The oldest span the log holds.
    span_oldest: u64,
    /// One past the newest span the log holds, which is the oldest where it holds none.
    span_end: u64,
    /// The first event no reader has saved.
    unsaved_first: u64,
}

impl Placement {
    /// The placement of a log cut as `log` that holds no span yet and whose first event is
    /// `event_first`.
    pub(super) fn new(log: Segments, event_first: u64) -> Self {
        let span = log.span(event_first);
        Self {
            segments: vec![0; log.segment_count],
            log,
            span_oldest: span,
            span_end: span,
            unsaved_first: event_first,
        }
    }

    /// Takes the writer's word that a reader saved every event before `unsaved_first`.
    pub(super) fn save(&mut self, unsaved_first: u64) {
        self.unsaved_first = self.unsaved_first.max(unsaved_first);
    }

    /// The words of the record for event `number`, from the ring's start, starting its span in
    /// the table where it is the span's first. The writer places its events in order, and
    /// `held_first` is the first event of the oldest tick the ring holds, so no tick the ring
    /// holds has an event before it.
    pub(super) fn place(
        &mut self,
        words: &[AtomicU64],
        number: u64,
        held_first: u64,
    ) -> Range<usize> {
        assert!(held_first <= number);
        assert!(self.unsaved_first <= number);

        let span = self.log.span(number);
        // An event falls in the newest span or starts the one after it.
        assert!(span <= self.span_end);
        if span == self.span_end {
            let segment = self.place_segment(words, self.log.span(held_first));
            let entry = self.log.entry_start(span);
            // A reader takes an entry only where it names the span before and after it reads
            // the segment, so the span is stored last.
            words[entry + ENTRY_SPAN].store(0, Ordering::Release);
            words[entry + ENTRY_SEGMENT].store(u64_from_usize(segment), Ordering::Release);
            words[entry + ENTRY_SPAN].store(span + 1, Ordering::Release);
            self.segments[self.log.entry_index(span)] = segment;
            self.span_end = span + 1;
        } else {
            assert_eq!(span + 1, self.span_end);
        }

        self.log
            .record(self.segments[self.log.entry_index(span)], number)
    }

    /// The segment for the span the writer starts next, which lets go of the oldest span where
    /// it takes that span's segment. No tick the ring holds has an event in a span before
    /// `span_held`.
    fn place_segment(&mut self, words: &[AtomicU64], span_held: u64) -> usize {
        // Each span the log holds has a segment of its own, and the writer took the segments
        // in order from the first, so the first segment it never wrote is their count.
        let span_count = usize_from(self.span_end - self.span_oldest);
        assert!(span_count <= self.log.segment_count);

        // The log lets go of its oldest span only where no tick the ring holds has an event in
        // it, and then where a reader saved its events or no segment is left to take. A log
        // that holds no span counts the one it starts as its oldest, which is never before an
        // event of a tick the ring holds, so it takes the first segment.
        let free = self.span_oldest < span_held;
        let saved = self.span_oldest < self.log.span(self.unsaved_first);
        let full = span_count == self.log.segment_count;
        if free && (saved || full) {
            let segment = self.segments[self.log.entry_index(self.span_oldest)];
            // Readers learn the span is gone before any word of its entry or its segment
            // changes, so a record they find changed is one they know to have missed.
            words[HEADER_EVENT_OLDEST]
                .store(self.log.span_first(self.span_oldest + 1), Ordering::Release);
            words[self.log.entry_start(self.span_oldest) + ENTRY_SPAN].store(0, Ordering::Release);
            self.span_oldest += 1;
            return segment;
        }

        // The events of the ticks the ring holds fit the log's room, which fills one segment
        // fewer than the log has. So a log with no segment left holds a span before those
        // events, which is its oldest, and the branch above took it.
        assert!(span_count < self.log.segment_count);
        span_count
    }
}

/// The events the log of `layout` holds once `newest` is written.
pub(super) fn held(words: &[AtomicU64], layout: &Layout, newest: u64) -> Held {
    let numbered = layout.events_held(newest);
    if layout.event_segments.is_none() {
        return numbered;
    }

    // Another program wrote the word, so it is held to the events the writer has written.
    let oldest = words[HEADER_EVENT_OLDEST].load(Ordering::Acquire);
    Held {
        oldest: oldest.max(numbered.oldest).min(newest + 1),
        newest,
    }
}

/// The words of the record that holds event `number` in the log of `layout`, from the ring's
/// start, or `None` where the log let go of the event's span. A table that names a segment the
/// log lacks is a broken ring.
pub(super) fn locate(
    words: &[AtomicU64],
    layout: &Layout,
    number: u64,
) -> io::Result<Option<Range<usize>>> {
    let Some(log) = &layout.event_segments else {
        return Ok(Some(layout.event(number)));
    };

    let span = log.span(number);
    let entry = log.entry_start(span);
    let named = words[entry + ENTRY_SPAN].load(Ordering::Acquire);
    let segment = words[entry + ENTRY_SEGMENT].load(Ordering::Acquire);
    let named_after = words[entry + ENTRY_SPAN].load(Ordering::Acquire);
    if named != span + 1 {
        return Ok(None);
    }

    if named_after != named {
        return Ok(None);
    }

    if segment >= u64_from_usize(log.segment_count) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    Ok(Some(log.record(usize_from(segment), number)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::Shape;

    /// A layout whose log has room for `event_capacity` events, with its words, all zero.
    fn ring(event_capacity: u64) -> (Layout, Vec<AtomicU64>) {
        let layout = Layout::versioned(
            Shape {
                slot_count: 1,
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity,
            },
            super::super::layout::LAYOUT_VERSION_IDENTITY,
        )
        .unwrap();
        let words = (0..layout.dictionary.start)
            .map(|_| AtomicU64::new(0))
            .collect();
        (layout, words)
    }

    /// The placement of the log of `layout`, whose first event is `event_first`.
    fn placement(layout: &Layout, event_first: u64) -> Placement {
        Placement::new(layout.event_segments.clone().unwrap(), event_first)
    }

    #[test]
    fn places_the_events_of_one_span_in_one_segment_in_order() {
        // Goal: the events of a span lie side by side in its segment, each in a record of its
        // own, where a reader finds them.
        // Method: place the first three events of a log and compare where a reader locates them.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        let mut placement = placement(&layout, 1);

        let placed: Vec<_> = (1..=3)
            .map(|number| placement.place(&words, number, 1))
            .collect();

        for (number, record) in (1..=3).zip(&placed) {
            assert_eq!(
                Some(record.clone()),
                locate(&words, &layout, number).unwrap()
            );
        }
        assert_eq!(placed[0].end, placed[1].start);
        assert_eq!(placed[1].end, placed[2].start);
    }

    #[test]
    fn starts_in_the_span_of_a_first_event_past_the_first_span() {
        // Goal: a collector that starts again logs on from the event its last ring ended at,
        // which is seldom the first of a span.
        // Method: start a log at an event part-way into its third span, place that event and
        // locate it.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        let first = 2 * SEGMENT_EVENTS + 5;
        let mut placement = placement(&layout, first);

        let placed = placement.place(&words, first, first);

        assert_eq!(Some(placed), locate(&words, &layout, first).unwrap());
    }

    #[test]
    fn a_span_the_log_let_go_is_not_located() {
        // Goal: a reader never takes a record of a later span for an event of a span the log
        // let go, although both spans share an entry of the table.
        // Method: in a log of two segments of one event, place three events no reader saved,
        // each the oldest a tick still holds, so the third finds no segment left to take and
        // starts its span in the first's segment and entry.
        let (layout, words) = ring(1);
        let mut placement = placement(&layout, 1);
        let first = placement.place(&words, 1, 1);
        placement.place(&words, 2, 2);

        let third = placement.place(&words, 3, 3);

        assert_eq!(first, third);
        assert_eq!(None, locate(&words, &layout, 1).unwrap());
        assert_eq!(Some(third), locate(&words, &layout, 3).unwrap());
        assert_eq!(2, words[HEADER_EVENT_OLDEST].load(Ordering::Relaxed));
    }

    #[test]
    fn keeps_a_span_no_reader_saved_while_a_segment_is_unwritten() {
        // Goal: the log lets go of no event before a reader saved it while it has a segment it
        // never wrote, so a reader that stopped for a while finds the events the log had room
        // for.
        // Method: place the events of two spans and one more, each the oldest a tick still
        // holds, with no event saved, and locate the first.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        let mut placement = placement(&layout, 1);

        for number in 1..=2 * SEGMENT_EVENTS + 1 {
            placement.place(&words, number, number);
        }

        assert!(locate(&words, &layout, 1).unwrap().is_some());
    }

    #[test]
    fn keeps_a_saved_span_a_tick_the_ring_holds_has_an_event_in() {
        // Goal: a reader of a tick the ring holds finds the tick's events, whether or not
        // another reader saved them.
        // Method: place the events of one span, the last of them the oldest a tick still holds,
        // save them all, start the next span and locate that last event.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        let mut placement = placement(&layout, 1);
        for number in 1..=SEGMENT_EVENTS {
            placement.place(&words, number, number);
        }
        placement.save(SEGMENT_EVENTS + 1);

        placement.place(&words, SEGMENT_EVENTS + 1, SEGMENT_EVENTS);

        assert!(locate(&words, &layout, SEGMENT_EVENTS).unwrap().is_some());
    }

    #[test]
    fn lets_go_of_a_span_a_reader_saved() {
        // Goal: the log starts a span in the segment of a span whose events a reader saved, so
        // it writes no segment it does not need.
        // Method: place the events of one span, each the oldest a tick still holds, save them
        // all, place one more and locate the first.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        let mut placement = placement(&layout, 1);
        for number in 1..=SEGMENT_EVENTS {
            placement.place(&words, number, number);
        }
        let first = locate(&words, &layout, 1).unwrap();
        placement.save(SEGMENT_EVENTS + 1);

        let last = placement.place(&words, SEGMENT_EVENTS + 1, SEGMENT_EVENTS + 1);

        assert_eq!(first, Some(last));
        assert_eq!(None, locate(&words, &layout, 1).unwrap());
        assert_eq!(
            SEGMENT_EVENTS + 1,
            words[HEADER_EVENT_OLDEST].load(Ordering::Relaxed)
        );
    }

    #[test]
    fn keeps_the_events_saved_when_a_later_report_saves_fewer() {
        // Goal: a report of fewer saved events than an earlier one takes none of them back,
        // since a persister that starts again reports nothing saved until it has read the
        // recording.
        // Method: place the events of one span, each the oldest a tick still holds, save them
        // all, report none saved, place one more and locate the first.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        let mut placement = placement(&layout, 1);
        for number in 1..=SEGMENT_EVENTS {
            placement.place(&words, number, number);
        }
        placement.save(SEGMENT_EVENTS + 1);
        placement.save(0);

        placement.place(&words, SEGMENT_EVENTS + 1, SEGMENT_EVENTS + 1);

        assert_eq!(None, locate(&words, &layout, 1).unwrap());
    }

    #[test]
    fn refuses_a_table_that_names_a_segment_the_log_lacks() {
        // Goal: a table another program wrote never sends a reader past the log's segments.
        // Method: name a segment one past the log's last in the entry of a placed event.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        let log = layout.event_segments.as_ref().unwrap();
        placement(&layout, 1).place(&words, 1, 1);
        words[log.entry_start(0) + ENTRY_SEGMENT]
            .store(u64_from_usize(log.segment_count), Ordering::Relaxed);

        let located = locate(&words, &layout, 1);

        assert_eq!(io::ErrorKind::InvalidData, located.unwrap_err().kind());
    }

    #[test]
    fn holds_no_event_past_the_newest_whatever_the_header_names() {
        // Goal: a header another program wrote never moves a reader past the newest event.
        // Method: name an oldest event far past the newest and ask what the log holds.
        let (layout, words) = ring(4 * SEGMENT_EVENTS);
        words[HEADER_EVENT_OLDEST].store(u64::MAX, Ordering::Relaxed);

        let held = held(&words, &layout, 9);

        assert_eq!(10, held.oldest);
        assert_eq!(9, held.newest);
    }
}
