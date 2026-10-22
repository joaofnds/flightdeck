use super::*;
use crate::disks::{DISKS_MAX, DiskReading, DiskSnapshot, ScanState};

#[test]
fn carries_maximum_disks_after_the_unchanged_battery_extent() {
    // Goal: the bounded tail coexists with the exact previous prefix and battery bytes.
    // Method: publish all sixteen disks with high counters through a real ring and read back.
    let mut fixture = Fixture::new();
    let snapshot = snapshot();
    fixture.publish(Some(&snapshot));
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();

    reader.read().unwrap();

    assert_eq!(reader.tick().disks, Some(snapshot));
    assert!(fixture.writer.disks_supported());
    let disk_start = fixture.writer.layout.disks.as_ref().unwrap().start;
    assert_eq!(
        disk_start,
        fixture.writer.layout.battery.as_ref().unwrap().word_count()
    );
}

fn snapshot() -> DiskSnapshot {
    let mut snapshot = DiskSnapshot {
        source_continuous_ns: 123,
        state: ScanState::Complete,
        count: DISKS_MAX,
        ..DiskSnapshot::default()
    };
    for (index, record) in snapshot.records.iter_mut().enumerate() {
        *record = DiskReading {
            driver_id: u64_from_usize(index + 1),
            media_id: u64_from_usize(index + 100),
            name: [0; 32],
            read_bytes: Some(u64::MAX),
            write_operations: Some(0),
            ..DiskReading::default()
        };
        record.name[..5].copy_from_slice(b"disk0");
    }

    snapshot
}

struct Fixture {
    writer: Writer,
    place: Place,
}

impl Fixture {
    fn new() -> Self {
        Self::create(false)
    }

    fn old() -> Self {
        Self::create(true)
    }

    fn create(old: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let place = Place {
            lock_path: std::env::temp_dir().join(format!(
                "flightdeck-disk-ring-test-{}-{index}",
                std::process::id()
            )),
            name_prefix: "/fd.dk.".to_owned(),
        };
        let identity = RunIdentity {
            recording: Identity([11, 12]),
            run: Identity([21, 22]),
            boot: Identity([31, 32]),
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
            event_capacity: 2,
        };
        let create = if old {
            Writer::create_with_battery
        } else {
            Writer::create_with_disks
        };
        let writer = create(
            &place,
            shape,
            DictionaryCapacity {
                entries: 2,
                arena_bytes: 256,
            },
            &identity,
            100,
        )
        .unwrap();

        Self { writer, place }
    }

    fn publish(&mut self, disks: Option<&DiskSnapshot>) {
        self.publish_sources(disks, false);
    }

    fn publish_old(&mut self) {
        self.publish_sources(None, true);
    }

    fn publish_sources(&mut self, disks: Option<&DiskSnapshot>, old: bool) {
        let readings = Readings {
            run_tick: 7,
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
        let identities = IdentityPublication {
            processes: &[],
            events: &[],
        };
        if old {
            self.writer
                .publish_with_battery(&readings, None, &identities, None)
                .unwrap();
            return;
        }
        self.writer
            .publish_with_disks(
                &readings,
                None,
                &identities,
                &SupplementalReadings {
                    battery: None,
                    disks,
                    system: None,
                },
            )
            .unwrap();
    }
}

#[test]
fn keeps_absence_empty_and_failed_scans_distinct() {
    // Goal: no extension value, a valid empty inventory and an acquisition error stay distinct.
    // Method: publish and read each state through one real ring.
    let mut fixture = Fixture::new();
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();
    fixture.publish(None);
    reader.read().unwrap();
    assert_eq!(reader.tick().disks, None);
    let empty = DiskSnapshot {
        state: ScanState::Complete,
        ..DiskSnapshot::default()
    };

    fixture.publish(Some(&empty));
    reader.read().unwrap();
    assert_eq!(reader.tick().disks, Some(empty));
    fixture.publish(Some(&DiskSnapshot::default()));
    reader.read().unwrap();
    assert_eq!(reader.tick().disks, Some(DiskSnapshot::default()));
}

#[test]
fn old_battery_publication_clears_reserved_disks() {
    // Goal: every old publisher clears any disk values left in a reused slot.
    // Method: wrap a populated disk slot using the original Battery publication method.
    let mut fixture = Fixture::new();
    fixture.publish(Some(&snapshot()));
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();
    reader.read().unwrap();

    fixture.publish_old();
    fixture.publish_old();
    reader.read().unwrap();
    assert_eq!(reader.tick().disks, None);
    reader.read().unwrap();
    assert_eq!(reader.tick().disks, None);
}

#[test]
fn old_battery_rings_clear_a_reused_reading_to_unavailable() {
    // Goal: an old ring never inherits the disk sample a caller buffer already holds.
    // Method: read an original Battery writer into a buffer seeded with a disk snapshot.
    let mut fixture = Fixture::old();
    fixture.publish_old();
    let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
    let mut tick = Tick::new(&fixture.writer.layout);
    tick.disks = Some(snapshot());

    reader.read_into(&mut tick).unwrap();

    assert!(!reader.disks_supported());
    assert_eq!(tick.disks, None);
}

macro_rules! corrupt_headers {
    ($($name:ident, $index:expr, $value:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: recognized malformed tails never enlarge or reinterpret a ring mapping.
            // Method: corrupt one immutable header word before opening the private ring.
            let fixture = Fixture::new();
            let start = fixture.writer.layout.disks.as_ref().unwrap().start;
            fixture.writer.mapping.words()[start + $index].store($value, Ordering::Relaxed);

            let result = RingReader::open(&fixture.place.lock_path);

            assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
        }
    )*};
}

corrupt_headers! {
    refuses_unknown_disk_tail_versions, 1, 2;
    refuses_disk_tail_slot_count_disagreement, 2, 3;
    refuses_wrong_disk_tail_record_width, 3, 245;
}

#[test]
fn refuses_nonzero_unused_ring_words() {
    // Goal: omitted records cannot conceal data in the reserved tail.
    // Method: corrupt the first unused word of a committed zero-device scan.
    let mut fixture = Fixture::new();
    fixture.publish(Some(&DiskSnapshot {
        state: ScanState::Complete,
        ..DiskSnapshot::default()
    }));
    let start = fixture
        .writer
        .layout
        .disks
        .as_ref()
        .unwrap()
        .record(1)
        .start;
    fixture.writer.mapping.words()[start + 4].store(1, Ordering::Relaxed);
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();

    assert_eq!(
        reader.read().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_a_slot_overwritten_after_the_disk_snapshot_was_loaded() {
    // Goal: disks and the original tick can only return under one stable publication sequence.
    // Method: load disks then overwrite the slot while the shared sequence helper is reading.
    let mut fixture = Fixture::new();
    fixture.publish(Some(&snapshot()));
    let words = fixture.writer.mapping.words();
    let layout = &fixture.writer.layout;
    let slot = &words[layout.slot(1)];
    let record = &words[layout.disks.as_ref().unwrap().record(1)];
    let mut tick = Tick::new(layout);

    let loaded = read_tick(
        slot,
        1,
        &mut tick,
        |tick| {
            disks::load(record, &mut tick.disks).unwrap();
            slot[SLOT_SEQUENCE].store(5, Ordering::Relaxed);
            disks::store(record, None);
            slot[SLOT_SEQUENCE].store(6, Ordering::Release);
            load_tick(slot, layout, tick)
        },
        || Ok(None),
    )
    .unwrap();

    assert_eq!(loaded, None);
}

#[test]
fn publishes_and_reads_disks_without_rust_heap_activity() {
    // Goal: the shared codec and real atomic ring use only startup-owned Rust storage.
    // Method: count allocation/free calls around a publication and borrowed read.
    let mut fixture = Fixture::new();
    let snapshot = snapshot();
    let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
    let mut tick = Tick::new(&fixture.writer.layout);

    let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
        fixture.publish(Some(&snapshot));
        reader.read_into(&mut tick).unwrap();
    });

    assert_eq!(calls, 0);
    assert_eq!(tick.disks, Some(snapshot));
}

#[test]
fn reserves_the_full_production_profile_below_one_gibibyte() {
    // Goal: enabling disks retains the existing live-reader admission bound.
    // Method: plan the documented production maximum without mapping its memory.
    let shape = Shape {
        slot_count: 120,
        cpu_capacity: 18,
        interface_capacity: 1638,
        core_capacity: 18,
        state_capacity: 64,
        fan_capacity: 10,
        process_capacity: 12000,
        coalition_capacity: 28384,
        event_capacity: 1_966_080,
    };
    let capacity = DictionaryCapacity::default();
    let previous = Writer::battery_size_bytes(shape, capacity).unwrap();

    let bytes = Writer::disks_size_bytes(shape, capacity).unwrap();

    assert_eq!(bytes - previous, 234_272);
    assert!(bytes < 1024 * 1024 * 1024);
}

#[test]
fn refuses_truncated_disk_records_during_admission() {
    // Goal: a valid extension header cannot map a truncated record region.
    // Method: admit a two-slot header with one byte missing from its planned extent.
    let header = [disks::MAGIC, 1, 2, 244].map(AtomicU64::new);
    let start = 100;
    let bytes = (start + disks::HEADER_WORDS + 2 * crate::disks::encoding::WORDS_MAX) * 8;

    assert!(
        disks::DisksLayout::admit(
            &header,
            2,
            &disks::DisksExtents {
                start_words: start,
                size_bytes: bytes - 1,
            },
        )
        .is_none()
    );
}

#[test]
fn refuses_overflowing_disk_tail_plans() {
    // Goal: fixed tail planning cannot wrap untrusted counts or offsets.
    // Method: plan from an impossible memory offset.
    assert!(disks::DisksLayout::plan(usize::MAX, 2).is_none());
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.place.lock_path).unwrap();
    }
}
