use super::*;

#[test]
fn charges_a_projection_until_its_last_buffer_clone_is_released() {
    // Goal: returning a frame cannot release storage still referenced by a projected clone.
    // Method: drop the input, copied frame and projection while retaining a native value buffer.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let frame = DataFrame::new(2, vec![Column::new("event".into(), [1_u64, 2])]).unwrap();

    let copied = copy_event_frame(&frame, &budget).unwrap();
    let projected = copied.select(["event"]).unwrap();
    let buffer = projected
        .column("event")
        .unwrap()
        .u64()
        .unwrap()
        .downcast_iter()
        .next()
        .unwrap()
        .values()
        .clone();
    drop(frame);
    drop(copied);
    drop(projected);

    assert!(budget.used() >= 2 * size_of::<u64>());
    assert_eq!(buffer.as_slice(), [1, 2]);
    drop(buffer);
    assert_eq!(budget.used(), 0);
}

#[test]
fn charges_an_independently_cloned_validity_bitmap() {
    // Goal: a validity clone cannot outlive the reservation when numeric values are gone.
    // Method: retain only the Arrow bitmap after dropping every frame and value buffer.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let frame = DataFrame::new(
        3,
        vec![Column::new(
            "publication".into(),
            [Some(7_u64), None, Some(9)],
        )],
    )
    .unwrap();
    let copied = copy_event_frame(&frame, &budget).unwrap();
    let validity = copied
        .column("publication")
        .unwrap()
        .u64()
        .unwrap()
        .downcast_iter()
        .next()
        .unwrap()
        .validity()
        .unwrap()
        .clone();
    drop(copied);
    drop(frame);

    assert!(budget.used() > 0);
    assert_eq!(validity.iter().collect::<Vec<_>>(), [true, false, true]);
    drop(validity);
    assert_eq!(budget.used(), 0);
}

#[test]
fn compacts_filtered_binary_arenas_and_keeps_the_remaining_bytes_charged() {
    // Goal: a tiny associated subset cannot pin detached program storage or its work guard.
    // Method: copy a filtered binary view and retain only its independently cloned byte buffer.
    let budget = Arc::new(ByteBudget::new(64 * 1024));
    let kept = [0xfe_u8; 32];
    let detached = [0xff_u8; 4095];
    let frame = DataFrame::new(
        2,
        vec![Column::new(
            "app_path".into(),
            [Some(kept.as_slice()), Some(detached.as_slice())],
        )],
    )
    .unwrap();
    let selected = frame.slice(0, 1);

    let copied = copy_event_frame(&selected, &budget).unwrap();
    let bytes = copied
        .column("app_path")
        .unwrap()
        .binary()
        .unwrap()
        .downcast_iter()
        .next()
        .unwrap()
        .data_buffers()[0]
        .clone();
    assert_eq!(bytes.len(), kept.len());
    drop(frame);
    drop(selected);
    drop(copied);

    assert!(budget.used() < 16 * 1024);
    assert!(budget.used() >= kept.len());
    assert_eq!(bytes.as_slice(), kept);
    drop(bytes);
    assert_eq!(budget.used(), 0);
}

#[test]
fn refuses_copy_before_allocating_without_byte_admission() {
    // Goal: an exhausted shared budget refuses the copy before allocating output storage.
    // Method: observe requested allocations with a zero-capacity ledger.
    let budget = Arc::new(ByteBudget::new(0));
    let frame = DataFrame::new(1, vec![Column::new("event".into(), [1_u64])]).unwrap();

    let (result, observation) =
        crate::allocation_tests::observe(&budget, || copy_event_frame(&frame, &budget));

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    assert_eq!(observation.allocations, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn refuses_invalid_bitmap_extents_and_cached_counts() {
    // Goal: malformed arguments never enter the unsafe Arrow bitmap constructor.
    // Method: exercise overflow, one bit past storage, and a false cached null count.
    assert!(bitmap(vec![1].into(), usize::MAX, 1, 0).is_err());
    assert!(bitmap(vec![1].into(), 0, 9, 8).is_err());
    assert!(bitmap(vec![1].into(), 0, 8, 9).is_err());
    assert!(bitmap(vec![1].into(), 0, 8, 0).is_err());
    let valid = bitmap(vec![0b1010].into(), 1, 3, 1).unwrap();
    assert_eq!(valid.iter().collect::<Vec<_>>(), [true, false, true]);
}

#[test]
fn preserves_the_offset_of_a_sliced_validity_buffer() {
    // Goal: rebuilding a bitmap uses the buffer's actual storage offset as well as its bit offset.
    // Method: select a nonzero byte offset and compare the three selected validity bits.
    let bytes: Buffer<u8> = vec![0, 0b1010].into();

    let validity = bitmap(bytes.sliced(1..2), 1, 3, 1).unwrap();

    assert_eq!(validity.iter().collect::<Vec<_>>(), [true, false, true]);
}

#[test]
fn admits_every_copy_allocation_before_requesting_it() {
    // Goal: copied event storage and native wrappers fit the reservations already acquired.
    // Method: trace the real nullable numeric/binary bridge and compare requested bytes to charge.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let bytes = [0xff_u8; 4095];
    let frame = DataFrame::new(
        3,
        vec![
            Column::new("publication".into(), [Some(7_u64), None, Some(9)]),
            Column::new(
                "app_path".into(),
                [Some(bytes.as_slice()), None, Some(b"short".as_slice())],
            ),
        ],
    )
    .unwrap();

    let (copied, allocation) =
        crate::allocation_tests::observe(&budget, || copy_event_frame(&frame, &budget).unwrap());

    assert!(allocation.allocation_charge_min > 0);
    assert!(allocation.allocated_bytes <= budget.used_peak());
    assert!(frame.equals_missing(&copied));
    eprintln!(
        "lease requested_bytes={} reserved_peak={} allocations={}",
        allocation.allocated_bytes,
        budget.used_peak(),
        allocation.allocations
    );
    drop(copied);
    assert_eq!(budget.used(), 0);
}
