use std::sync::Arc;

use super::ByteBudget;

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
