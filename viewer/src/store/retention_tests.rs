use std::sync::Arc;

use super::{Held, MachineSecond, View};

const RUN: collector::ring::RunIdentity = collector::ring::RunIdentity {
    recording: collector::ring::Identity([1, 1]),
    run: collector::ring::Identity([2, 2]),
    boot: collector::ring::Identity([3, 3]),
};

#[test]
fn a_sparse_history_without_eviction_preserves_its_gaps() {
    // Goal: the first retained reading does not turn a valid preceding gap into an eviction.
    // Method: retain two distant observations without filling the full-detail span.
    let mut held = Held::new();
    take(&mut held, &second(100));
    take(&mut held, &second(1_000));

    let view = publish(&mut held);

    assert_eq!(view.full_detail_evicted_through_ns, None);
    assert_eq!(view.seconds.len(), 2);
}

#[test]
fn an_eviction_records_the_departed_time_without_moving_an_older_view() {
    // Goal: full-detail availability records actual eviction rather than the next reading's time.
    // Method: retain a full View, then evict its first reading across a large wall-clock gap.
    let mut held = Held::new();
    take(&mut held, &second(1));
    for publication in 2..=u64::try_from(super::SECONDS_MAX).unwrap() {
        take(
            &mut held,
            &MachineSecond {
                wall_ns: (publication + 100) * 1_000_000_000,
                ..second(publication)
            },
        );
    }
    let original = publish(&mut held);
    assert_eq!(original.full_detail_evicted_through_ns, None);

    take(
        &mut held,
        &MachineSecond {
            wall_ns: (u64::try_from(super::SECONDS_MAX).unwrap() + 101) * 1_000_000_000,
            ..second(u64::try_from(super::SECONDS_MAX).unwrap() + 1)
        },
    );
    let later = publish(&mut held);

    assert_eq!(later.full_detail_evicted_through_ns, Some(1_000_000_000));
    assert_eq!(later.seconds[0].wall_ns, 102_000_000_000);
    assert_eq!(original.full_detail_evicted_through_ns, None);
    assert_eq!(original.seconds[0].publication, 1);
}

#[test]
fn backpressure_does_not_advance_the_eviction_boundary() {
    // Goal: a refused publication cannot invalidate more full detail than ingestion
    // actually evicts.
    // Method: pin a full View, consume every spare buffer, then offer one further publication.
    let mut held = full_span();
    let original = publish(&mut held);
    let count = u64::try_from(super::SECONDS_HELD_MAX).unwrap();
    let full = u64::try_from(super::SECONDS_MAX).unwrap();
    for offset in 1..=count {
        take(&mut held, &second(full + offset));
    }
    let accepted = publish(&mut held);

    let pushed = held.take(&RUN, &second(full + count + 1), unread());
    let refused = publish(&mut held);

    assert!(!pushed);
    assert_eq!(
        accepted.full_detail_evicted_through_ns,
        Some(count * 1_000_000_000)
    );
    assert_eq!(
        refused.full_detail_evicted_through_ns,
        accepted.full_detail_evicted_through_ns
    );
    assert_eq!(refused.seconds, accepted.seconds);
    assert_eq!(original.full_detail_evicted_through_ns, None);
}

#[test]
fn a_load_reset_starts_a_new_eviction_account_without_mutating_old_views() {
    // Goal: a replacement load starts its own retention account while old readers stay immutable.
    // Method: publish an evicted span, reset the owner, and publish one new observation.
    let mut held = full_span();
    take(
        &mut held,
        &second(u64::try_from(super::SECONDS_MAX).unwrap() + 1),
    );
    let original = publish(&mut held);
    assert_eq!(original.full_detail_evicted_through_ns, Some(1_000_000_000));

    held.reset();
    take(&mut held, &second(10_000));
    let replacement = publish(&mut held);

    assert_eq!(replacement.full_detail_evicted_through_ns, None);
    assert_eq!(original.full_detail_evicted_through_ns, Some(1_000_000_000));
}

#[test]
fn repeated_wall_publications_keep_their_shared_eviction_time() {
    // Goal: losing one sibling makes that wall time incomplete even while another remains.
    // Method: evict two same-wall siblings separately before evicting their later successor.
    let mut held = Held::new();
    for publication in 1..=2 {
        take(
            &mut held,
            &MachineSecond {
                wall_ns: 100_000_000_000,
                ..second(publication)
            },
        );
    }
    let full = u64::try_from(super::SECONDS_MAX).unwrap();
    for publication in 3..=full {
        take(
            &mut held,
            &MachineSecond {
                wall_ns: (publication + 100) * 1_000_000_000,
                ..second(publication)
            },
        );
    }

    take(&mut held, &second(full + 101));
    let first = publish(&mut held);
    take(&mut held, &second(full + 102));
    let second_sibling = publish(&mut held);
    take(&mut held, &second(full + 103));
    let successor = publish(&mut held);

    assert_eq!(first.full_detail_evicted_through_ns, Some(100_000_000_000));
    assert_eq!(
        second_sibling.full_detail_evicted_through_ns,
        Some(100_000_000_000)
    );
    assert_eq!(
        successor.full_detail_evicted_through_ns,
        Some(103_000_000_000)
    );
}

#[test]
fn a_backward_clock_cannot_restore_already_evicted_detail() {
    // Goal: a later eviction at an earlier clock cannot erase an already recorded loss.
    // Method: evict a wall-500 reading, then a wall-100 reading in publication order.
    let mut held = Held::new();
    take(
        &mut held,
        &MachineSecond {
            wall_ns: 500_000_000_000,
            ..second(1)
        },
    );
    take(
        &mut held,
        &MachineSecond {
            wall_ns: 100_000_000_000,
            ..second(2)
        },
    );
    let full = u64::try_from(super::SECONDS_MAX).unwrap();
    for publication in 3..=full {
        take(
            &mut held,
            &MachineSecond {
                wall_ns: (publication + 1_000) * 1_000_000_000,
                ..second(publication)
            },
        );
    }

    take(&mut held, &second(full + 1_001));
    let first = publish(&mut held);
    take(&mut held, &second(full + 1_002));
    let later = publish(&mut held);

    assert_eq!(first.full_detail_evicted_through_ns, Some(500_000_000_000));
    assert_eq!(later.full_detail_evicted_through_ns, Some(500_000_000_000));
}

#[test]
fn an_already_accepted_publication_cannot_advance_the_eviction_account() {
    // Goal: ignoring a duplicate must not discard another full-detail observation.
    // Method: fill and evict once, then resubmit an old publication with a different clock.
    let mut held = full_span();
    let full = u64::try_from(super::SECONDS_MAX).unwrap();
    take(&mut held, &second(full + 1));
    let original = publish(&mut held);

    take(
        &mut held,
        &MachineSecond {
            wall_ns: 99_999_000_000_000,
            ..second(1)
        },
    );
    let duplicate = publish(&mut held);

    assert_eq!(
        duplicate.full_detail_evicted_through_ns,
        Some(1_000_000_000)
    );
    assert_eq!(duplicate.seconds, original.seconds);
}

fn full_span() -> Held {
    let mut held = Held::new();
    for publication in 1..=u64::try_from(super::SECONDS_MAX).unwrap() {
        take(&mut held, &second(publication));
    }
    held
}

fn second(publication: u64) -> MachineSecond {
    MachineSecond {
        publication,
        run_tick: publication,
        wall_ns: publication * 1_000_000_000,
        ..MachineSecond::default()
    }
}

fn take(held: &mut Held, second: &MachineSecond) {
    assert!(held.take(&RUN, second, unread()));
}

fn unread() -> super::apps::TickApps<'static> {
    super::apps::TickApps::Missing(&super::Missing::Unread)
}

fn publish(held: &mut Held) -> Arc<View> {
    let mailbox = super::Mailbox::new();
    held.publish(&mailbox);
    mailbox.take().unwrap()
}
