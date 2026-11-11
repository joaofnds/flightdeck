use super::*;
use crate::tracker::Event;

/// The slots of the rings these tests make, few enough that a tick soon leaves the ring.
const SLOTS: u64 = 4;
/// The events the logs of these tests have room for, 64 segments of them.
const EVENTS: u64 = 64 * SEGMENT_EVENTS;
/// The events of each tick of a burst, six segments of them.
const BURST_TICK_EVENTS: u64 = 6 * SEGMENT_EVENTS;

#[test]
fn a_quiet_log_whose_events_are_saved_writes_two_segments() {
    // Goal: a log whose ticks hold few events, and whose reader saves them as they come, stops
    // writing new memory after the segment its newest events lie in and the one before it,
    // however many events it has logged, since memory the writer has written stays charged to
    // the collector for as long as it runs.
    // Method: log three times the events the log has room for, one a tick, in a ring of four
    // slots, saving each tick, and count the segments the writer wrote a record in.
    let mut source = Source::new(layout::EventLog::Segmented);

    source.publish_saved(1, 3 * EVENTS);

    assert_eq!(2, source.segments_written());
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

    assert_reads_events(&mut reader, logged - EVENTS + 1..=logged);
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
}

#[test]
fn keeps_the_events_from_the_saved_end_a_reader_reported() {
    // Goal: the log lets go of the events before the end a reader reported and of none from it
    // on, so a persister that then stops finds every event it had not saved.
    // Method: log four segments of events, one a tick, reporting each time an end two events
    // behind the newest, so the last two events of the fourth segment are not saved. Then log
    // half the log's room with no report and read from the first event not saved.
    let mut source = Source::new(layout::EventLog::Segmented);
    let reported = 4 * SEGMENT_EVENTS;
    for run_tick in 1..=reported {
        source.publish(run_tick, 1);
        source.writer.mark_events_saved(run_tick - 1);
    }
    let logged = reported + EVENTS / 2;
    for run_tick in reported + 1..=logged {
        source.publish(run_tick, 1);
    }
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();

    reader.seek_event(reported - 1).unwrap();

    assert_reads_events(&mut reader, reported - 1..=logged);
}

#[test]
fn a_report_of_events_not_yet_logged_saves_only_those_logged() {
    // Goal: a report that names more events than the writer has logged lets go of none logged
    // after it, since another program wrote it.
    // Method: log one segment of events, one a tick, reporting each time an end no log reaches,
    // then log half the log's room with no report and read the events logged after the last
    // report.
    let mut source = Source::new(layout::EventLog::Segmented);
    let reported = SEGMENT_EVENTS;
    for run_tick in 1..=reported {
        source.publish(run_tick, 1);
        source.writer.mark_events_saved(u64::MAX);
    }
    let logged = reported + EVENTS / 2;
    for run_tick in reported + 1..=logged {
        source.publish(run_tick, 1);
    }
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();

    reader.seek_event(reported + 1).unwrap();

    assert_reads_events(&mut reader, reported + 1..=logged);
}

#[test]
fn a_burst_keeps_every_event_of_the_ticks_the_ring_holds() {
    // Goal: a burst takes the segments it needs, so a reader still reads every event of every
    // tick the ring holds.
    // Method: after a quiet stretch, log four ticks of six segments of events each, saving each
    // as it comes, and read all of their events.
    let mut source = Source::new(layout::EventLog::Segmented);
    let quiet = 2 * SEGMENT_EVENTS;
    source.publish_saved(1, quiet);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    reader.seek_event(quiet + 1).unwrap();

    source.publish_burst(quiet + 1, SLOTS);

    assert_reads_events(&mut reader, quiet + 1..=quiet + SLOTS * BURST_TICK_EVENTS);
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
}

#[test]
fn a_tick_the_ring_holds_keeps_its_events_in_the_oldest_segment() {
    // Goal: a reader of the oldest tick the ring holds finds its events, although they are saved
    // and lie in the oldest segment the log holds.
    // Method: log a quiet stretch that ends on the last event of a segment, then one burst tick
    // fewer than the ring has slots, saving each tick, and read the last quiet event.
    let mut source = Source::new(layout::EventLog::Segmented);
    let quiet = 2 * SEGMENT_EVENTS;
    source.publish_saved(1, quiet);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    reader.seek_event(quiet).unwrap();

    source.publish_burst(quiet + 1, SLOTS - 1);

    assert_reads_events(&mut reader, quiet..=quiet);
}

#[test]
fn a_log_writes_no_segment_beyond_those_its_busiest_ticks_needed() {
    // Goal: once a burst has passed, the log goes on in the segments its busiest ticks needed
    // and writes no other, however many events follow. Those are the segments the burst
    // filled and one more, for the tick after the burst, which the ring holds with it.
    // Method: log a burst of four ticks of six segments of events each, then quiet ticks for
    // three times the log's room, saving each tick, and count the segments written.
    let mut source = Source::new(layout::EventLog::Segmented);
    source.publish_burst(1, SLOTS);

    source.publish_saved(SLOTS + 1, 3 * EVENTS);

    let burst_segments = SLOTS * BURST_TICK_EVENTS / SEGMENT_EVENTS;
    assert_eq!(burst_segments + 1, source.segments_written());
}

#[test]
fn a_reader_behind_the_saved_events_learns_which_it_missed() {
    // Goal: a reader that fell behind the events the log holds learns in one step which events
    // it missed, and reads every later one, so no event is lost without a count.
    // Method: open a reader on an empty ring, log eight segments of quiet ticks, each saved as
    // another reader would, and read to the newest event from where the first step ends.
    let mut source = Source::new(layout::EventLog::Segmented);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let logged = 8 * SEGMENT_EVENTS;
    source.publish_saved(1, logged);

    let FoundEvent::Missed { first, end } = reader.read_event().unwrap() else {
        panic!("the reader missed no event");
    };

    assert_eq!(1, first);
    assert!(end > first);
    // Each tick logged one event, so the ticks the ring holds have the newest `SLOTS` events.
    assert!(end <= logged - SLOTS + 1);
    assert_reads_events(&mut reader, end..=logged);
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
}

#[test]
fn a_reader_keeps_its_binding_to_the_ring_when_the_log_lets_events_go() {
    // Goal: a persister goes on taking a ring for the one it registered once the log has let
    // events go, which moves a word of the ring's header.
    // Method: take a reader's binding, log saved events until the log starts a segment over,
    // and take the binding again.
    let mut source = Source::new(layout::EventLog::Segmented);
    let reader = RingReader::open(&source.place.lock_path).unwrap();
    let bound = reader.object_binding().unwrap();

    source.publish_saved(1, 8 * SEGMENT_EVENTS);

    let words = source.writer.mapping.words();
    assert!(words[layout::HEADER_EVENT_OLDEST].load(Ordering::Relaxed) > 1);
    assert_eq!(bound, reader.object_binding().unwrap());
}

#[test]
fn an_event_let_go_after_the_reader_looked_reads_as_missed() {
    // Goal: a reader that learned what the log held just before the writer let a segment go
    // counts the event it then cannot find as missed, and does not take another event for it.
    // Method: let the log start a segment over, put the header's oldest event back to what a
    // reader would have loaded before, and read the first event.
    let source = source_let_go_unannounced();
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    reader.seek_event(1).unwrap();

    let found = reader.read_event().unwrap();

    assert_eq!(FoundEvent::Missed { first: 1, end: 2 }, found);
}

#[test]
fn an_event_let_go_after_the_reader_looked_reads_as_missed_with_its_identity() {
    // Goal: the reader that also takes each event's identity counts such an event the same way.
    // Method: as above, through the read that fills an identity.
    let source = source_let_go_unannounced();
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    reader.seek_event(1).unwrap();
    let mut identity = IdentityObservation::EMPTY;

    let found = reader.read_event_identity(&mut identity).unwrap();

    assert_eq!(FoundEvent::Missed { first: 1, end: 2 }, found);
}

#[test]
fn a_writer_that_starts_again_logs_on_in_a_later_segment() {
    // Goal: a collector that starts again logs on from the event its last ring ended at, which
    // is part-way into a later segment, and a reader finds that event.
    // Method: log a segment and a half of events, start the writer again on the same lock file,
    // log one event and read it.
    let mut source = Source::new(layout::EventLog::Segmented);
    let before = SEGMENT_EVENTS + SEGMENT_EVENTS / 2;
    source.publish_saved(1, before);
    let mut source = source.restarted();
    source.publish(1, 1);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();

    reader.seek_event(before + 1).unwrap();

    assert_reads_events(&mut reader, before + 1..=before + 1);
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

    assert_reads_events(&mut reader, logged - EVENTS + 1..=logged);
    assert_eq!(FoundEvent::Nothing, reader.read_event().unwrap());
}

/// Reads each event of `numbers` from `reader`, in order.
#[track_caller]
fn assert_reads_events(reader: &mut RingReader, numbers: std::ops::RangeInclusive<u64>) {
    for number in numbers {
        let found = reader.read_event().unwrap();
        assert!(
            matches!(found, FoundEvent::Event { number: read, .. } if read == number),
            "event {number} read as {found:?}"
        );
    }
}

/// A source whose log started a segment over and let its first event go, with the header's
/// oldest event put back to the first, as a reader loaded it just before.
fn source_let_go_unannounced() -> Source {
    let mut source = Source::new(layout::EventLog::Segmented);
    source.publish_saved(1, 4 * SEGMENT_EVENTS);
    let words = source.writer.mapping.words();
    assert!(words[layout::HEADER_EVENT_OLDEST].load(Ordering::Relaxed) > 1);
    words[layout::HEADER_EVENT_OLDEST].store(1, Ordering::Relaxed);
    source
}

#[test]
fn a_source_that_started_again_leaves_no_ring_behind() {
    // Goal: these tests leave no shared memory on the machine that runs them, where a ring
    // nobody can name stays until the next boot.
    // Method: start a source again over a ring that holds a publication, end it, and unlink
    // each ring it made by name, which finds a ring only if the source left it.
    let mut source = Source::new(layout::EventLog::Segmented);
    source.publish(1, 1);
    let first = source.writer.name().to_owned();
    let source = source.restarted();
    let second = source.writer.name().to_owned();

    drop(source);

    let left = [first, second].map(|name| unlink_generation(&name).map_err(|error| error.kind()));
    assert_eq!([Err(io::ErrorKind::NotFound); 2], left);
}

/// A ring of `SLOTS` slots whose log has room for `EVENTS` events, with its writer.
struct Source {
    writer: Writer,
    place: Place,
    events: Vec<Event>,
    identities: Vec<IdentityObservation>,
    removal: Removal,
}

/// Removes a source's ring and its lock file when the test ends.
struct Removal {
    lock_path: PathBuf,
}

impl Drop for Removal {
    fn drop(&mut self) {
        // The lock file names the newest ring a writer made on it.
        let name = std::fs::read_to_string(&self.lock_path).unwrap();
        unlink_generation(name.trim_end_matches(['\0', '\n'])).unwrap();
        std::fs::remove_file(&self.lock_path).unwrap();
    }
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
            removal: Removal {
                lock_path: place.lock_path.clone(),
            },
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

    /// Publishes `tick_count` ticks of `BURST_TICK_EVENTS` events each from `run_tick_first`,
    /// reporting each tick's events saved as it comes.
    fn publish_burst(&mut self, run_tick_first: u64, tick_count: u64) {
        for run_tick in run_tick_first..run_tick_first + tick_count {
            self.publish(run_tick, BURST_TICK_EVENTS);
            self.save_newest();
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

    /// Publishes `tick_count` ticks of one event each from `run_tick_first`, reporting each
    /// tick's event saved as it comes, as a reader that keeps up with the writer does.
    fn publish_saved(&mut self, run_tick_first: u64, tick_count: u64) {
        for run_tick in run_tick_first..run_tick_first + tick_count {
            self.publish(run_tick, 1);
            self.save_newest();
        }
    }

    /// Reports every event logged saved.
    fn save_newest(&mut self) {
        self.writer.mark_events_saved(self.writer.event + 1);
    }

    /// This source with its writer ended and another started on the same lock file, as a
    /// collector that starts again does, whose log goes on from the event the last ring ended
    /// at.
    fn restarted(self) -> Self {
        let Self {
            mut writer,
            place,
            events,
            identities,
            removal,
        } = self;
        let layout =
            Layout::versioned(writer.layout.shape, layout::LAYOUT_VERSION_IDENTITY).unwrap();
        let ended = writer.name().to_owned();
        writer.finish();
        // The next writer takes the lock this one holds.
        drop(writer);
        let writer = Writer::create_layout(
            LockedPlace::acquire(&place).unwrap(),
            layout,
            WriterOrigin::Legacy,
        )
        .unwrap();
        // A writer keeps the ring before its own while that ring holds a publication, for a
        // persister to seal, and the lock file names only the newest, so the ring that ended
        // goes here.
        match unlink_generation(&ended) {
            Ok(()) => {}
            Err(error) => assert_eq!(io::ErrorKind::NotFound, error.kind()),
        }
        Self {
            writer,
            place,
            events,
            identities,
            removal,
        }
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
        let segments = EVENTS / SEGMENT_EVENTS + 1;
        let mut count = 0;
        for segment in 0..segments {
            let first = segment * SEGMENT_EVENTS;
            if (first..first + SEGMENT_EVENTS).any(written) {
                count += 1;
            }
        }
        count
    }
}
