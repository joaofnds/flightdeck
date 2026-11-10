use super::*;
use crate::tracker::Event;

/// The slots of the rings these tests make, few enough that a tick soon leaves the ring.
const SLOTS: u64 = 4;
/// The events the logs of these tests have room for, 64 segments of them.
const EVENTS: u64 = 64 * event_log::SEGMENT_EVENTS;

#[test]
fn a_quiet_log_writes_only_the_segments_it_keeps() {
    // Goal: a log whose ticks hold few events, and whose reader saves them as they come, stops
    // writing new memory once it has filled the segments it keeps, however many events it has
    // logged, since memory the writer has written stays charged to the collector for as long as
    // it runs.
    // Method: log three times the events the log has room for, one a tick, in a ring of four
    // slots, saving each tick, and count the segments the writer wrote a record in.
    let mut source = Source::new(layout::EventLog::Segmented);

    for run_tick in 1..=3 * EVENTS {
        source.publish(run_tick, 1);
        source.save_newest();
    }

    assert_eq!(
        event_log::EVENTS_KEPT / event_log::SEGMENT_EVENTS,
        source.segments_written()
    );
}

#[test]
fn a_log_no_reader_saved_keeps_the_events_it_has_room_for() {
    // Goal: while no reader reports its events saved, the log keeps as many events as a log that
    // numbers its records round its room, so a persister that stopped for a while still finds
    // them.
    // Method: log three times the events the log has room for, one a tick, with none saved, and
    // read the newest events the log has room for.
    let mut source = Source::new(layout::EventLog::Segmented);
    let logged = 3 * EVENTS;
    for run_tick in 1..=logged {
        source.publish(run_tick, 1);
    }
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();

    reader.seek_event(logged - EVENTS + 1).unwrap();

    for number in logged - EVENTS + 1..=logged {
        assert!(matches!(
            reader.read_event().unwrap(),
            FoundEvent::Event { number: read, .. } if read == number
        ));
    }
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
}

#[test]
fn a_report_of_a_publication_the_ring_does_not_hold_saves_nothing() {
    // Goal: a report the writer cannot check against a tick it holds lets go of no event, since
    // another program wrote it.
    // Method: after each quiet tick report the newest publication the ring let go and one the
    // writer has not made, and count the segments written.
    let mut source = Source::new(layout::EventLog::Segmented);

    for run_tick in 1..=3 * EVENTS {
        source.publish(run_tick, 1);
        source
            .writer
            .publications_saved(run_tick.saturating_sub(SLOTS));
        source.writer.publications_saved(run_tick + 1);
    }

    assert_eq!(
        EVENTS / event_log::SEGMENT_EVENTS + 1,
        source.segments_written()
    );
}

#[test]
fn a_burst_keeps_every_event_of_the_ticks_the_ring_holds() {
    // Goal: a burst takes the segments it needs, so a reader still reads every event of every
    // tick the ring holds, and once the burst passes the log writes no segment beyond them.
    // Method: after a quiet stretch, log four ticks that together hold half again the events
    // the log keeps, read all of their events, then log quiet ticks for three times the log's
    // room and count the segments written before and after them. Every tick is saved as it
    // comes.
    let mut source = Source::new(layout::EventLog::Segmented);
    for run_tick in 1..=2 * event_log::EVENTS_KEPT {
        source.publish(run_tick, 1);
        source.save_newest();
    }
    let quiet = 2 * event_log::EVENTS_KEPT;
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    reader.seek_event(quiet + 1).unwrap();
    let burst_tick_events = 6 * event_log::SEGMENT_EVENTS;

    for burst in 1..=SLOTS {
        source.publish(quiet + burst, burst_tick_events);
        source.save_newest();
    }

    for number in quiet + 1..=quiet + SLOTS * burst_tick_events {
        assert!(matches!(
            reader.read_event().unwrap(),
            FoundEvent::Event { number: read, .. } if read == number
        ));
    }
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
    let written = source.segments_written();
    assert!(written > event_log::EVENTS_KEPT / event_log::SEGMENT_EVENTS);
    assert!(written < EVENTS / event_log::SEGMENT_EVENTS);
    for run_tick in 1..=3 * EVENTS {
        source.publish(quiet + SLOTS + run_tick, 1);
        source.save_newest();
    }
    assert_eq!(written, source.segments_written());
}

#[test]
fn a_reader_behind_the_kept_events_learns_which_it_missed() {
    // Goal: a reader that fell behind the events the log keeps learns in one step which events
    // it missed, and reads every later one, so no event is lost without a count.
    // Method: open a reader on an empty ring, log quiet ticks past the segments the log keeps,
    // each saved as another reader would, and read to the newest event, adding the missed
    // events to the events read.
    let mut source = Source::new(layout::EventLog::Segmented);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let logged = 3 * event_log::EVENTS_KEPT;
    for run_tick in 1..=logged {
        source.publish(run_tick, 1);
        source.save_newest();
    }

    let FoundEvent::Missed { first, end } = reader.read_event().unwrap() else {
        panic!("the reader missed no event");
    };

    assert_eq!(1, first);
    assert!(end > logged - event_log::EVENTS_KEPT - event_log::SEGMENT_EVENTS);
    assert!(end <= logged - SLOTS);
    for number in end..=logged {
        assert!(matches!(
            reader.read_event().unwrap(),
            FoundEvent::Event { number: read, .. } if read == number
        ));
    }
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
}

#[test]
fn a_log_that_is_not_segmented_still_reads() {
    // Goal: a reader still reads the events of a ring an older collector made, whose log numbers
    // its records round the whole of its room.
    // Method: log more events than the log has room for in a ring of the identity layout as
    // it was before segments, and read the events the log holds.
    let mut source = Source::new(layout::EventLog::Numbered);
    let logged = EVENTS + 5;
    for run_tick in 1..=logged {
        source.publish(run_tick, 1);
    }
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();

    for number in 6..=logged {
        assert!(matches!(
            reader.read_event().unwrap(),
            FoundEvent::Event { number: read, .. } if read == number
        ));
    }
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
}

/// A ring of `SLOTS` slots whose log has room for `EVENTS` events, with its writer.
struct Source {
    writer: Writer,
    place: Place,
    events: Vec<Event>,
    identities: Vec<IdentityObservation>,
}

impl Source {
    fn new(log: layout::EventLog) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let place = Place {
            lock_path: std::env::temp_dir().join(format!(
                "fd-event-log-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
            name_prefix: "/fd.el.".to_owned(),
        };
        let shape = Shape {
            slot_count: SLOTS,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: EVENTS,
        };
        let layout = match log {
            layout::EventLog::Segmented => {
                Layout::versioned(shape, layout::LAYOUT_VERSION_IDENTITY)
            }
            layout::EventLog::Numbered => Layout::identity_numbered(shape),
        }
        .unwrap();
        let writer = Writer::create_layout(
            LockedPlace::acquire(&place).unwrap(),
            layout,
            WriterOrigin::Legacy,
        )
        .unwrap();
        let events_per_tick = usize_from(EVENTS / SLOTS);
        Self {
            writer,
            place,
            events: (0..events_per_tick)
                .map(|_| Event::Exec { pid: 7 })
                .collect(),
            identities: vec![
                IdentityObservation {
                    pid: 7,
                    ..IdentityObservation::EMPTY
                };
                events_per_tick
            ],
        }
    }

    /// Publishes a tick that logs `event_count` events.
    fn publish(&mut self, run_tick: u64, event_count: u64) {
        let event_count = usize_from(event_count);
        let readings = Readings {
            run_tick,
            cpus: &[],
            memory: &crate::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &self.events[..event_count],
            dropped: Dropped::default(),
        };
        let identities = IdentityPublication {
            processes: &[],
            events: &self.identities[..event_count],
        };
        self.writer.publish_inner(
            &readings,
            None,
            Some(&identities),
            &SupplementalReadings::default(),
        );
    }

    /// Reports the newest publication saved, as a reader that keeps up with the writer does.
    fn save_newest(&mut self) {
        self.writer
            .publications_saved(self.writer.publication_newest());
    }

    /// The segments of a segmented log the writer has written a record in. The log's room is
    /// one segment more than its events fill.
    fn segments_written(&self) -> u64 {
        let layout = &self.writer.layout;
        assert!(layout.event_segments.is_some());
        let words = self.writer.mapping.words();
        let written = |record: u64| {
            let sequence =
                layout.events_start + usize_from(record) * layout.event_words + EVENT_SEQUENCE;
            words[sequence].load(Ordering::Relaxed) != 0
        };
        let segments = EVENTS / event_log::SEGMENT_EVENTS + 1;
        let mut count = 0;
        for segment in 0..segments {
            let first = segment * event_log::SEGMENT_EVENTS;
            if (first..first + event_log::SEGMENT_EVENTS).any(written) {
                count += 1;
            }
        }
        count
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.place.lock_path).unwrap();
    }
}
