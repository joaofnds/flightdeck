use super::*;

macro_rules! timing_parity {
    ($name:ident, $version:expr, $times:expr) => {
        #[test]
        fn $name() {
            // Goal: timing and full reads agree on supported clocks, numbering and cursors.
            // Method: publish a real versioned source and compare both public read operations.
            let mut source = Source::new($version);
            source.publish(73, $times);
            let mut timing_reader = RingReader::open(&source.place.lock_path).unwrap();
            let mut full_reader = RingReader::open(&source.place.lock_path).unwrap();
            let mut timing = PublicationTiming::default();
            let mut tick = Tick::new(&source.writer.layout);

            let found = timing_reader.read_timing_into(&mut timing).unwrap();

            assert_eq!(found, full_reader.read_into(&mut tick).unwrap());
            assert_eq!(
                timing_reader.next_publication(),
                full_reader.next_publication()
            );
            assert_eq!(
                timing,
                PublicationTiming {
                    run_tick: 73,
                    times: $times.copied()
                }
            );
            assert_eq!(timing.run_tick, tick.run_tick);
            assert_eq!(timing.times, tick.times);
            assert_eq!(
                timing_reader.read_timing_into(&mut timing).unwrap(),
                Found::Nothing
            );
            assert_eq!(full_reader.read_into(&mut tick).unwrap(), Found::Nothing);
        }
    };
}

const TIMES: ObservationTimes = ObservationTimes {
    wall_ns: 999,
    continuous_ns: 123,
    awake_ns: 111,
};

timing_parity!(reads_legacy_unknown_clocks, 2, None::<&ObservationTimes>);
timing_parity!(reads_observed_unknown_clocks, 3, None::<&ObservationTimes>);
timing_parity!(reads_observed_exact_clocks, 3, Some(&TIMES));
timing_parity!(reads_current_unknown_clocks, 4, None::<&ObservationTimes>);
timing_parity!(reads_current_exact_clocks, 4, Some(&TIMES));

#[test]
fn warm_timing_reads_use_no_rust_allocator_calls() {
    // Goal: timing storage and reads stay independent of payload allocation.
    // Method: warm a real current reader, then measure a seek/read/empty-read cycle.
    let mut source = Source::new(4);
    source.publish(73, Some(&TIMES));
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let mut timing = PublicationTiming::default();
    reader.read_timing_into(&mut timing).unwrap();

    let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
        reader.seek_publication(1).unwrap();
        assert!(matches!(
            reader.read_timing_into(&mut timing).unwrap(),
            Found::Publication {
                publication: 1,
                tick: 73
            }
        ));
        assert_eq!(
            reader.read_timing_into(&mut timing).unwrap(),
            Found::Nothing
        );
    });

    assert_eq!(calls, 0);
    assert_eq!(timing.times, Some(TIMES));
}

#[test]
fn clears_clocks_when_the_next_publication_has_none() {
    // Goal: caller storage never retains earlier exact times for an unknown-clock reading.
    // Method: reuse the same timing output across observed and unobserved publications.
    let mut source = Source::new(4);
    source.publish(73, Some(&TIMES));
    source.publish(74, None);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let mut timing = PublicationTiming::default();
    reader.read_timing_into(&mut timing).unwrap();

    reader.read_timing_into(&mut timing).unwrap();

    assert_eq!(
        timing,
        PublicationTiming {
            run_tick: 74,
            times: None
        }
    );
}

macro_rules! sequence_result {
    ($name:ident, $sequence:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // Goal: timing preserves the full reader's distinction between corruption and loss.
            // Method: change a real slot sequence under an advertised finished publication.
            let mut source = Source::new(4);
            source.publish(73, Some(&TIMES));
            source.slot(1)[SLOT_SEQUENCE].store($sequence, Ordering::Release);
            let mut timing_reader = RingReader::open(&source.place.lock_path).unwrap();
            let mut full_reader = RingReader::open(&source.place.lock_path).unwrap();
            let mut timing = PublicationTiming::default();
            let mut tick = Tick::new(&source.writer.layout);

            let result = timing_reader
                .read_timing_into(&mut timing)
                .map_err(|e| e.kind());

            assert_eq!(result, $expected);
            assert_eq!(
                result,
                full_reader.read_into(&mut tick).map_err(|e| e.kind())
            );
            assert_eq!(
                timing_reader.next_publication(),
                full_reader.next_publication()
            );
        }
    };
}

sequence_result!(
    refuses_timing_behind_the_advertised_publication,
    0,
    Err(io::ErrorKind::InvalidData)
);
sequence_result!(
    misses_timing_overwritten_before_reading,
    6,
    Ok(Found::Missed { first: 1, end: 2 })
);
sequence_result!(
    misses_timing_being_rewritten,
    5,
    Ok(Found::Missed { first: 1, end: 2 })
);

#[test]
fn misses_publications_that_fell_out_of_the_ring() {
    // Goal: timing shares the full reader's oldest-retained-publication cursor behavior.
    // Method: open both readers before wrapping a two-slot ring, then compare all outcomes.
    let mut source = Source::new(3);
    let mut timing_reader = RingReader::open(&source.place.lock_path).unwrap();
    let mut full_reader = RingReader::open(&source.place.lock_path).unwrap();
    source.publish(1, None);
    source.publish(2, None);
    source.publish(3, None);
    let mut timing = PublicationTiming::default();
    let mut tick = Tick::new(&source.writer.layout);

    let found = timing_reader.read_timing_into(&mut timing).unwrap();

    assert_eq!(found, Found::Missed { first: 1, end: 2 });
    assert_eq!(found, full_reader.read_into(&mut tick).unwrap());
    assert_eq!(timing_reader.next_publication(), 2);
    assert_eq!(
        timing_reader.next_publication(),
        full_reader.next_publication()
    );
    assert_eq!(
        timing_reader.read_timing_into(&mut timing).unwrap(),
        full_reader.read_into(&mut tick).unwrap()
    );
    assert_eq!(timing.run_tick, tick.run_tick);
}

#[test]
fn rejects_timing_rewritten_during_the_shared_sequence_check() {
    // Goal: the shared seqlock cannot accept copied timing from a changed publication.
    // Method: load timing in read_finished and advance the actual slot before its final fence.
    let mut source = Source::new(4);
    source.publish(73, Some(&TIMES));
    let mut timing = PublicationTiming::default();
    let slot = source.slot(1);

    let result = read_finished(slot, SLOT_SEQUENCE, 1, || {
        tick::load_timing(slot, &source.writer.layout, &mut timing);
        slot[SLOT_SEQUENCE].store(5, Ordering::Release);
        Some(())
    })
    .unwrap();

    assert_eq!(result, None);
}

#[test]
fn coherent_timing_does_not_validate_numeric_payload() {
    // Goal: readiness timing remains readable while malformed numeric data is refused.
    // Method: corrupt a published process count, then use timing and full public reads.
    let mut source = Source::new(4);
    source.publish(73, Some(&TIMES));
    source.slot(1)[layout::SLOT_PROCESS_COUNT].store(2, Ordering::Release);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let mut timing = PublicationTiming::default();

    let found = reader.read_timing_into(&mut timing).unwrap();

    assert_eq!(
        found,
        Found::Publication {
            publication: 1,
            tick: 73
        }
    );
    reader.seek_publication(1).unwrap();
    assert_eq!(
        reader
            .read_into(&mut Tick::new(&source.writer.layout))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn coherent_timing_does_not_validate_identity_payload() {
    // Goal: captured identity validation remains the full decoder's responsibility.
    // Method: corrupt a coherent identity count and compare timing with an identity read.
    let mut source = Source::new(4);
    source.publish(73, Some(&TIMES));
    source.slot(1)[layout::SLOT_IDENTITY_COUNT].store(2, Ordering::Release);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let mut timing = PublicationTiming::default();
    let mut identities = Vec::with_capacity(1);

    let found = reader.read_timing_into(&mut timing).unwrap();

    assert_eq!(
        found,
        Found::Publication {
            publication: 1,
            tick: 73
        }
    );
    reader.seek_publication(1).unwrap();
    assert_eq!(
        reader
            .read_identity_into(&mut Tick::new(&source.writer.layout), &mut identities)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn coherent_timing_does_not_validate_supplemental_payload() {
    // Goal: optional payload tails have no role in a timing-only observation.
    // Method: corrupt battery presence under a coherent parent sequence and compare reads.
    let mut source = Source::new(4);
    source.publish(73, Some(&TIMES));
    let record = source.writer.layout.battery.as_ref().unwrap().record(1);
    source.writer.mapping.words()[record.start].store(u64::MAX, Ordering::Release);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let mut timing = PublicationTiming::default();

    let found = reader.read_timing_into(&mut timing).unwrap();

    assert_eq!(
        found,
        Found::Publication {
            publication: 1,
            tick: 73
        }
    );
    reader.seek_publication(1).unwrap();
    assert_eq!(
        reader
            .read_into(&mut Tick::new(&source.writer.layout))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

struct Source {
    writer: Writer,
    place: Place,
}

impl Source {
    fn new(version: u64) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let place = Place {
            lock_path: std::env::temp_dir().join(format!(
                "fd-timing-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
            name_prefix: "/fd.tm.".to_owned(),
        };
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        };
        let layout = if version == 4 {
            Layout::identity_system(
                shape,
                DictionaryCapacity {
                    entries: 2,
                    arena_bytes: 128,
                },
            )
        } else {
            Layout::versioned(shape, version)
        }
        .unwrap();
        let writer = Writer::create_layout(
            LockedPlace::acquire(&place).unwrap(),
            layout,
            WriterOrigin::Legacy,
        )
        .unwrap();
        if version == 2 {
            writer.mapping.words()[layout::HEADER_SLOT_FIXED_WORDS]
                .store(u64_from_usize(layout::SLOT_WALL_NS), Ordering::Relaxed);
        }
        Self { writer, place }
    }

    fn publish(&mut self, run_tick: u64, times: Option<&ObservationTimes>) {
        if self.writer.layout.version == 2 {
            self.publish_legacy(run_tick);
            return;
        }
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
            events: &[],
            dropped: Dropped::default(),
        };
        let identities = (self.writer.layout.version == 4).then_some(IdentityPublication {
            processes: &[],
            events: &[],
        });
        self.writer.publish_inner(
            &readings,
            times,
            identities.as_ref(),
            &SupplementalReadings::default(),
        );
    }

    fn publish_legacy(&mut self, run_tick: u64) {
        self.writer.publication += 1;
        let slot = self.slot(self.writer.publication);
        slot[layout::SLOT_RUN_TICK].store(run_tick, Ordering::Relaxed);
        slot[layout::SLOT_EVENTS_FIRST].store(1, Ordering::Relaxed);
        slot[layout::SLOT_EVENTS_END].store(1, Ordering::Relaxed);
        slot[SLOT_SEQUENCE].store(2 * self.writer.publication, Ordering::Release);
        self.writer.mapping.words()[HEADER_PUBLICATION_NEWEST]
            .store(self.writer.publication, Ordering::Release);
    }

    fn slot(&self, publication: u64) -> &[AtomicU64] {
        &self.writer.mapping.words()[self.writer.layout.slot(publication)]
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.place.lock_path).unwrap();
    }
}
