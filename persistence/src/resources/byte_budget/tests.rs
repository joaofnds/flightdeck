use std::sync::Arc;

use super::ByteBudget;

#[test]
fn releases_unused_capacity_without_detaching_the_remaining_owner() {
    // Goal: bounded preflight room can return without an unaccounted ownership gap.
    // Method: shrink one of two owners and reuse exactly the newly available capacity.
    let budget = Arc::new(ByteBudget::new(100));
    let mut first = budget.reserve(80).unwrap();
    let second = budget.reserve(10).unwrap();

    first.shrink_to(30);

    assert_eq!(first.bytes(), 30);
    assert_eq!(budget.used(), 40);
    assert_eq!(budget.used_peak(), 90);
    let reused = budget.reserve(60).unwrap();
    assert_eq!(budget.used(), 100);
    drop(first);
    assert_eq!(budget.used(), 70);
    drop(second);
    drop(reused);
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.used_peak(), 100);
}

#[test]
fn permits_an_unchanged_or_empty_reservation_without_releasing_twice() {
    // Goal: no-op and full release preserve accounting through the owner's later drop.
    // Method: shrink one owner to its current charge and then to zero.
    let budget = Arc::new(ByteBudget::new(80));
    let mut reservation = budget.reserve(80).unwrap();

    reservation.shrink_to(80);
    assert_eq!(budget.used(), 80);
    reservation.shrink_to(0);

    assert_eq!(reservation.bytes(), 0);
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.used_peak(), 80);
    drop(reservation);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retains_the_largest_overlapping_reservation_after_release() {
    // Goal: the peak records overlap rather than the limit or cumulative requests.
    // Method: release part of a peak, reserve again below it, then release every owner.
    let budget = Arc::new(ByteBudget::new(100));
    let first = budget.reserve(30).unwrap();
    let second = budget.reserve(20).unwrap();

    drop(first);
    let third = budget.reserve(10).unwrap();
    drop(second);
    drop(third);

    assert_eq!(budget.used(), 0);
    assert_eq!(budget.used_peak(), 50);
    assert_eq!(budget.maximum(), 100);
}

#[test]
fn excludes_refused_requests_and_accepts_the_exact_limit() {
    // Goal: refused work cannot inflate the peak or consume room for a later valid request.
    // Method: refuse an overlapping request, then fill the exact remaining capacity.
    let budget = Arc::new(ByteBudget::new(100));
    let first = budget.reserve(60).unwrap();

    assert!(budget.reserve(41).is_none());
    assert_eq!(budget.used(), 60);
    assert_eq!(budget.used_peak(), 60);
    let second = budget.reserve(40).unwrap();

    assert_eq!(budget.used(), 100);
    assert_eq!(budget.used_peak(), 100);
    drop(first);
    drop(second);
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.used_peak(), 100);
}

#[test]
fn rejects_overflow_without_changing_the_peak() {
    // Goal: reservation arithmetic cannot wrap either counter.
    // Method: hold the largest representable reservation and refuse one additional byte.
    let budget = Arc::new(ByteBudget::new(usize::MAX));
    let reservation = budget.reserve(usize::MAX).unwrap();

    assert!(budget.reserve(1).is_none());
    drop(reservation);

    assert_eq!(budget.used(), 0);
    assert_eq!(budget.used_peak(), usize::MAX);
}

#[test]
fn records_overlapping_reservations_from_shared_callers() {
    // Goal: all callers contribute to one peak, independent of their execution order.
    // Method: join eight reserving threads while retaining every returned owner.
    let budget = Arc::new(ByteBudget::new(64));

    let reservations = std::thread::scope(|scope| {
        let callers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| budget.reserve(8).unwrap()))
            .collect();

        callers
            .into_iter()
            .map(|caller| caller.join().unwrap())
            .collect::<Vec<_>>()
    });

    assert_eq!(budget.used(), 64);
    assert_eq!(budget.used_peak(), 64);
    drop(reservations);
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.used_peak(), 64);
}
