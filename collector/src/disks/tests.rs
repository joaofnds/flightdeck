use super::*;

#[test]
fn counter_presence_and_high_bits_survive_the_shared_codec() {
    // Goal: unavailable fields remain independent from measured zero and high unsigned values.
    // Method: round trip all 256 presence masks through the public word codec.
    for mask in 0_u64..256 {
        let mut snapshot = DiskSnapshot {
            source_continuous_ns: 71,
            state: ScanState::Complete,
            count: 1,
            ..DiskSnapshot::default()
        };
        snapshot.records[0] = DiskReading {
            driver_id: 1,
            media_id: 2,
            name: *b"disk0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
            read_bytes: (mask & 1 != 0).then_some(u64::MAX),
            write_bytes: (mask & 2 != 0).then_some(0),
            read_operations: (mask & 4 != 0).then_some(1 << 63),
            write_operations: (mask & 8 != 0).then_some(0),
            read_errors: (mask & 16 != 0).then_some(u64::MAX),
            write_errors: (mask & 32 != 0).then_some(0),
            read_time_ns: (mask & 64 != 0).then_some(u64::MAX),
            write_time_ns: (mask & 128 != 0).then_some(0),
            ..DiskReading::default()
        };
        let mut words = [0; encoding::WORDS_MAX];
        let count = encoding::encode(&snapshot, &mut words).unwrap();
        let mut decoded = DiskSnapshot::default();

        encoding::decode(&words[..count], &mut decoded).unwrap();

        assert_eq!(snapshot, decoded);
        assert!(words[count..].iter().all(|word| *word == 0));
    }
}

#[test]
fn an_attempt_after_success_cannot_publish_stale_disks() {
    // Goal: a native failure invalidates the preceding successful sample.
    // Method: an injectable source writes success then partially mutates and fails.
    struct Source(bool);
    impl DiskSource for Source {
        fn read_into(&mut self, now_ns: u64, snapshot: &mut DiskSnapshot) -> Result<(), Error> {
            if self.0 {
                snapshot.count = 1;
                return Err(Error::Native(71));
            }

            self.0 = true;
            snapshot.source_continuous_ns = now_ns;
            snapshot.state = ScanState::Complete;
            Ok(())
        }
    }
    let mut disks = Disks::from_source(Box::new(Source(false)));
    disks.refresh(1).unwrap();
    assert_eq!(disks.snapshot().state, ScanState::Complete);

    assert_eq!(disks.refresh(2), Err(Error::Native(71)));

    assert_eq!(disks.snapshot().state, ScanState::Failed);
    assert_eq!(disks.snapshot().count, 0);
    assert_eq!(disks.snapshot().source_continuous_ns, 2);
}

fn snapshot() -> DiskSnapshot {
    let mut snapshot = DiskSnapshot {
        source_continuous_ns: 71,
        state: ScanState::Complete,
        count: 1,
        ..DiskSnapshot::default()
    };
    snapshot.records[0] = DiskReading {
        driver_id: 5,
        media_id: 7,
        read_bytes: Some(u64::MAX),
        ..DiskReading::default()
    };
    snapshot.records[0].name[..5].copy_from_slice(b"disk0");

    snapshot
}

macro_rules! malformed_words {
    ($($name:ident, $index:expr, $value:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: malformed shared words fail without normalizing corrupt values.
            // Method: alter one field of a valid encoded snapshot through the public codec.
            let mut words = [0; encoding::WORDS_MAX];
            let used = encoding::encode(&snapshot(), &mut words).unwrap();
            words[$index] = $value;
            let mut decoded = DiskSnapshot::default();

            let result = encoding::decode(&words[..used], &mut decoded);

            assert_eq!(result, Err(Error::InvalidSnapshot));
        }
    )*};
}

malformed_words! {
    refuses_unknown_versions, 0, 2;
    refuses_unknown_scan_states, 2, 4;
    refuses_overflowing_counts, 3, u64::MAX;
    refuses_zero_driver_identity, 4, 0;
    refuses_zero_media_identity, 5, 0;
    refuses_unknown_counter_flags, 6, 1 << 10;
    refuses_nonzero_absent_counter_lanes, 12, 1;
    refuses_failed_records_with_counters, 6, (1 << 8) | 1;
    refuses_nonzero_label_padding, 8, 1;
    refuses_empty_labels, 7, 0;
    refuses_control_label_bytes, 7, u64::from_le_bytes(*b"di\nsk0\0\0");
    refuses_non_ascii_labels, 7, u64::from_le_bytes(*b"di\xc3\xa9k0\0\0");
    refuses_failed_scan_with_devices, 2, 1;
}

#[test]
fn retains_a_label_at_the_exact_thirty_one_byte_bound() {
    // Goal: the full bounded label fits without truncating its terminator.
    // Method: round trip thirty-one ASCII graphic bytes and the required final zero.
    let mut snapshot = snapshot();
    snapshot.records[0].name[..31].fill(b'd');
    let mut words = [0; encoding::WORDS_MAX];
    let used = encoding::encode(&snapshot, &mut words).unwrap();
    let mut decoded = DiskSnapshot::default();

    encoding::decode(&words[..used], &mut decoded).unwrap();

    assert_eq!(decoded, snapshot);
}

#[test]
fn refuses_a_label_that_has_no_room_for_its_terminator() {
    // Goal: a label longer than the bound never becomes a silently truncated identity label.
    // Method: encode thirty-two graphic bytes with no NUL terminator.
    let mut snapshot = snapshot();
    snapshot.records[0].name.fill(b'd');

    assert_eq!(
        encoding::encode(&snapshot, &mut [0; encoding::WORDS_MAX]),
        Err(Error::InvalidSnapshot)
    );
}

#[test]
fn refuses_two_media_records_that_share_one_driver_counter_source() {
    // Goal: multiple whole media cannot double-count their shared driver's statistics.
    // Method: encode different media IDs attached to one driver ID.
    let mut snapshot = snapshot();
    snapshot.count = 2;
    snapshot.records[1] = snapshot.records[0];
    snapshot.records[1].media_id += 1;

    assert_eq!(
        encoding::encode(&snapshot, &mut [0; encoding::WORDS_MAX]),
        Err(Error::InvalidSnapshot)
    );
}

#[test]
fn refuses_noncanonical_trailing_text_words() {
    // Goal: saved text has one exact used extent and no ignored trailing payload.
    // Method: append a zero word to a valid one-device snapshot.
    let mut words = [0; encoding::WORDS_MAX];
    let used = encoding::encode(&snapshot(), &mut words).unwrap();

    assert_eq!(
        encoding::decode(&words[..=used], &mut DiskSnapshot::default()),
        Err(Error::InvalidSnapshot)
    );
}

#[test]
fn replaces_disappeared_and_changed_media_without_retaining_old_records() {
    // Goal: disappearance and media replacement cannot retain an old sample or identity.
    // Method: an injectable source emits one disk, none, and the same driver with new media.
    struct FakeSource;
    impl DiskSource for FakeSource {
        fn read_into(&mut self, now_ns: u64, snapshot: &mut DiskSnapshot) -> Result<(), Error> {
            snapshot.state = ScanState::Complete;
            if now_ns != 2 {
                snapshot.count = 1;
                snapshot.records[0].driver_id = 5;
                snapshot.records[0].media_id = now_ns;
                snapshot.records[0].read_bytes = Some(now_ns);
                snapshot.records[0].name[..5].copy_from_slice(b"disk0");
            }
            Ok(())
        }
    }
    let mut disks = Disks::from_source(Box::new(FakeSource));
    disks.refresh(1).unwrap();
    let first = *disks.snapshot();

    disks.refresh(2).unwrap();
    assert_eq!(disks.snapshot().count, 0);
    assert_eq!(
        disks.snapshot().records,
        [DiskReading::default(); DISKS_MAX]
    );
    disks.refresh(3).unwrap();
    assert_eq!(
        disks.snapshot().records[0].driver_id,
        first.records[0].driver_id
    );
    assert_ne!(
        disks.snapshot().records[0].media_id,
        first.records[0].media_id
    );
    assert_eq!(disks.snapshot().records[0].read_bytes, Some(3));
}

#[test]
fn reads_native_snapshots_without_rust_heap_activity() {
    // Goal: dynamic discovery and native snapshots require no Rust allocations after startup.
    // Method: warm native calls, then count Rust allocation/free calls around a fresh scan.
    let mut disks = Disks::open().unwrap();
    disks.refresh(1).unwrap();
    let mut result = Ok(());

    let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
        result = disks.refresh(2);
    });

    assert_eq!(result, Ok(()));
    assert_eq!(calls, 0);
    assert_eq!(disks.snapshot().source_continuous_ns, 2);
    let snapshot = disks.snapshot();
    eprintln!(
        "native_disks_state={:?} count={} rust_heap_calls={calls}",
        snapshot.state, snapshot.count
    );
    eprintln!(
        "native_disks_records={:?}",
        &snapshot.records[..snapshot.count]
    );
}
