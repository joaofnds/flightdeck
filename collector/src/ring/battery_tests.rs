use super::*;
use crate::battery::BatteryReading;

#[test]
fn carries_optional_battery_with_the_original_tick_and_identity() {
    // Goal: the optional tail preserves the base tick and the run's exact identity.
    // Method: publish synthetic data and read through both owned and borrowed readers.
    let mut fixture = Fixture::new();
    let battery = BatteryReading {
        source_continuous_ns: 123,
        charge_current: Some(0),
        charge_maximum: Some(100),
        full_charge_capacity: Some(5000),
        design_capacity: Some(4500),
        cycle_count: Some(42),
        charging: Some(false),
        external_power: Some(true),
        voltage_mv: Some(12000),
        current_ma: Some(-123),
        adapter_watts: None,
    };
    fixture.publish(Some(&battery));
    let mut owned = Reader::open(&fixture.place.lock_path).unwrap();
    let mut borrowed = RingReader::open(&fixture.place.lock_path).unwrap();
    let mut tick = Tick::new(&fixture.writer.layout);

    assert_eq!(
        owned.read().unwrap(),
        Found::Publication {
            publication: 1,
            tick: 7
        }
    );
    assert_eq!(
        borrowed.read_into(&mut tick).unwrap(),
        Found::Publication {
            publication: 1,
            tick: 7
        }
    );

    assert_eq!(owned.tick().battery, Some(battery));
    assert_eq!(tick, *owned.tick());
    assert_eq!(borrowed.identity(), Some(fixture.identity));
    assert_eq!(borrowed.source_layout_version(), 4);
    assert_eq!(borrowed.run_start_continuous_ns(), Some(100));
    assert_eq!(borrowed.mapped_size_bytes(), 2736);
    assert!(fixture.writer.battery_supported());
    assert!(borrowed.battery_supported());
    assert_eq!(
        fixture.writer.mapping.words()[38].load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        fixture.writer.mapping.words()[39].load(Ordering::Relaxed),
        0
    );
}

struct Fixture {
    writer: Writer,
    place: Place,
    identity: RunIdentity,
}

impl Fixture {
    fn new() -> Self {
        Self::create(true)
    }

    fn create(tail: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let place = Place {
            lock_path: std::env::temp_dir().join(format!(
                "flightdeck-battery-ring-test-{}-{index}",
                std::process::id()
            )),
            name_prefix: "/fd.bt.".to_owned(),
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
        let capacity = DictionaryCapacity {
            entries: 2,
            arena_bytes: 256,
        };
        let writer = if tail {
            Writer::create_with_battery(&place, shape, capacity, &identity, 100)
        } else {
            Writer::create_with_identity(&place, shape, capacity, &identity, 100)
        }
        .unwrap();
        Self {
            writer,
            place,
            identity,
        }
    }

    fn publish(&mut self, battery: Option<&BatteryReading>) {
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
        let times = ObservationTimes {
            wall_ns: 900,
            continuous_ns: 200,
            awake_ns: 150,
        };
        let identities = IdentityPublication {
            processes: &[],
            events: &[],
        };
        if self.writer.layout.battery.is_some() {
            self.writer
                .publish_with_battery(&readings, Some(&times), &identities, battery)
        } else {
            assert!(battery.is_none());
            self.writer
                .publish_with_identity(&readings, Some(&times), &identities)
        }
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.place.lock_path).unwrap();
    }
}

#[test]
fn distinguishes_unavailable_from_present_fields_with_zero_values() {
    // Goal: unavailable battery data never becomes numeric zero.
    // Method: overwrite a present zero reading with unavailable data in the same slot.
    let mut fixture = Fixture::new();
    let reading = BatteryReading {
        source_continuous_ns: 0,
        charge_current: Some(0),
        current_ma: Some(0),
        charging: Some(false),
        ..BatteryReading::default()
    };
    fixture.publish(Some(&reading));
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();
    reader.read().unwrap();
    assert_eq!(reader.tick().battery, Some(reading));

    fixture.publish(None);
    fixture.publish(None);
    reader.read().unwrap();
    assert_eq!(reader.tick().battery, None);
    reader.read().unwrap();
    assert_eq!(reader.tick().battery, None);
}

#[test]
fn retains_every_signed_current_bit() {
    // Goal: the wire encoding preserves the entire signed domain.
    // Method: round trip the minimum signed integer through a real shared-memory ring.
    let mut fixture = Fixture::new();
    let reading = BatteryReading {
        source_continuous_ns: 123,
        current_ma: Some(i64::MIN),
        ..BatteryReading::default()
    };
    fixture.publish(Some(&reading));
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();

    reader.read().unwrap();

    assert_eq!(reader.tick().battery, Some(reading));
}

#[test]
fn treats_page_rounded_original_v4_storage_as_no_battery_tail() {
    // Goal: old zero padding is absent data rather than an invalid extension.
    // Method: open an original V4 writer with no opted-in battery bytes.
    let mut fixture = Fixture::create(false);
    fixture.publish(None);
    let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
    let mut tick = Tick::new(&fixture.writer.layout);
    tick.battery = Some(BatteryReading::default());

    assert!(fixture.writer.layout.battery.is_none());
    assert!(!fixture.writer.battery_supported());
    assert!(!reader.battery_supported());
    assert_eq!(reader.mapped_size_bytes(), 2512);
    assert_eq!(
        reader.read_into(&mut tick).unwrap(),
        Found::Publication {
            publication: 1,
            tick: 7
        }
    );
    assert_eq!(tick.battery, None);
}

fn corrupt_header(index: usize, value: u64) -> io::ErrorKind {
    let fixture = Fixture::new();
    let start = fixture.writer.layout.base_word_count();
    fixture.writer.mapping.words()[start + index].store(value, Ordering::Relaxed);
    RingReader::open(&fixture.place.lock_path)
        .err()
        .unwrap()
        .kind()
}

#[test]
fn refuses_an_unknown_battery_tail_version() {
    // Goal: recognized tails with unsupported versions are rejected before full mapping.
    // Method: spoil the immutable version in a private empty ring.
    assert_eq!(corrupt_header(1, 2), io::ErrorKind::InvalidData);
}

#[test]
fn refuses_a_battery_tail_with_the_wrong_record_width() {
    // Goal: admission never interprets a record of a different width.
    // Method: spoil the width in a private empty ring.
    assert_eq!(corrupt_header(3, 13), io::ErrorKind::InvalidData);
}

#[test]
fn refuses_a_battery_tail_with_a_different_slot_count() {
    // Goal: the tail cannot select slots independently of the base ring.
    // Method: spoil the tail slot count without changing the original header.
    assert_eq!(corrupt_header(2, u64::MAX), io::ErrorKind::InvalidData);
}

fn corrupt_record(index: usize, value: u64) -> io::ErrorKind {
    let mut fixture = Fixture::new();
    fixture.publish(Some(&BatteryReading::default()));
    let record = fixture.writer.layout.battery.as_ref().unwrap().record(1);
    fixture.writer.mapping.words()[record.start + index].store(value, Ordering::Relaxed);
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();
    reader.read().unwrap_err().kind()
}

#[test]
fn refuses_unknown_battery_presence_flags() {
    // Goal: unknown field meanings are not silently accepted.
    // Method: set a flag outside the version-one field mask.
    assert_eq!(corrupt_record(0, 1 << 20), io::ErrorKind::InvalidData);
}

#[test]
fn refuses_nonzero_missing_battery_lanes() {
    // Goal: missing fields have one canonical representation.
    // Method: write a nonzero value under a cleared presence bit.
    assert_eq!(corrupt_record(2, 1), io::ErrorKind::InvalidData);
}

#[test]
fn refuses_noncanonical_battery_booleans() {
    // Goal: booleans do not normalize arbitrary integers to true.
    // Method: write two into the charging lane of a committed publication.
    assert_eq!(corrupt_record(7, 2), io::ErrorKind::InvalidData);
}

#[test]
fn rejects_a_slot_overwritten_between_base_and_battery_loading() {
    // Goal: a base tick and a newer tail can never become one returned publication.
    // Method: finish a wrapped publication after loading base but before loading battery.
    let mut fixture = Fixture::new();
    let old = BatteryReading {
        source_continuous_ns: 123,
        ..BatteryReading::default()
    };
    let new = BatteryReading {
        source_continuous_ns: 456,
        ..BatteryReading::default()
    };
    fixture.publish(Some(&old));
    let words = fixture.writer.mapping.words();
    let layout = &fixture.writer.layout;
    let slot = &words[layout.slot(1)];
    let record = &words[layout.battery.as_ref().unwrap().record(1)];
    let mut tick = Tick::new(layout);

    let loaded = read_tick(
        slot,
        1,
        &mut tick,
        |tick| load_tick(slot, layout, tick),
        || {
            slot[SLOT_SEQUENCE].store(5, Ordering::Relaxed);
            battery::store(record, Some(&new));
            slot[SLOT_SEQUENCE].store(6, Ordering::Release);
            battery::load(record)
        },
    )
    .unwrap();

    assert_eq!(loaded, None);
}
