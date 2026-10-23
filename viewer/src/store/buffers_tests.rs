use std::sync::Arc;
use std::thread;

use super::{Buffers, Mailbox, Span};

/// A span of `capacity` readings over `buffers` buffers, each reading a number.
fn span(capacity: usize, buffers: usize) -> Span<u64> {
    Span::new(capacity, Buffers::new(buffers, || 0))
}

fn push(span: &mut Span<u64>, reading: u64) -> bool {
    span.push(|buffer| *buffer = reading)
}

fn held(span: &Span<u64>) -> Vec<u64> {
    span.held().map(|reading| **reading).collect()
}

/// What a View of `span` holds: a copy of its handles, as the store's View is.
fn view(span: &Span<u64>) -> Vec<Arc<u64>> {
    span.held().map(Arc::clone).collect()
}

#[test]
fn a_span_holds_its_newest_readings_oldest_first() {
    // Goal: a full span lets go of its oldest reading for each new one. Method: push five
    // readings into a span of three and read what it holds.
    let mut span = span(3, 5);

    for reading in 1..=5 {
        assert!(push(&mut span, reading));
    }

    assert_eq!(held(&span), [3, 4, 5]);
}

#[test]
fn a_reading_a_view_holds_never_changes() {
    // Goal: the store never writes a buffer the main thread reads. Method: hold a View of a
    // full span, push readings until the span has let go of every reading the View holds, and
    // read the View again.
    let mut span = span(2, 5);
    assert!(push(&mut span, 1));
    assert!(push(&mut span, 2));
    let view = view(&span);

    assert!(push(&mut span, 3));
    assert!(push(&mut span, 4));
    assert!(push(&mut span, 5));

    assert_eq!(
        view.iter().map(|reading| **reading).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(held(&span), [4, 5]);
}

#[test]
fn a_released_buffer_is_written_again() {
    // Goal: the span runs forever on the buffers allocated at its start. Method: push many
    // more readings than buffers into a span no View holds.
    let mut span = span(4, 5);

    for reading in 1..=1000 {
        assert!(push(&mut span, reading));
    }

    assert_eq!(held(&span), [997, 998, 999, 1000]);
}

#[test]
fn a_view_released_frees_its_buffers() {
    // Goal: a View the main thread drops lets the store write its buffers again. Method: hold
    // a View until every buffer is held, drop it, and push again.
    let mut span = span(2, 3);
    assert!(push(&mut span, 1));
    assert!(push(&mut span, 2));
    let view = view(&span);
    assert!(push(&mut span, 3));
    assert!(!push(&mut span, 4));

    drop(view);

    assert!(push(&mut span, 4));
    assert_eq!(held(&span), [3, 4]);
}

mod when_every_buffer_is_held {
    use super::{held, push, span, view};

    #[test]
    fn a_push_changes_nothing() {
        // Goal: a reading the span cannot hold yet is taken again at the next step, with the
        // span as it was. Method: hold Views until no buffer is free, then push.
        let mut span = span(2, 3);
        assert!(push(&mut span, 1));
        assert!(push(&mut span, 2));
        let first = view(&span);
        assert!(push(&mut span, 3));
        let second = view(&span);

        let pushed = push(&mut span, 4);

        assert!(!pushed);
        assert_eq!(held(&span), [2, 3]);
        drop((first, second));
    }

    #[test]
    fn repeated_pushes_keep_every_reading() {
        // Goal: a span that cannot take a reading does not shrink while it waits. Method: push
        // into a span with every buffer held, several times.
        let mut span = span(2, 3);
        assert!(push(&mut span, 1));
        assert!(push(&mut span, 2));
        let first = view(&span);
        assert!(push(&mut span, 3));
        let second = view(&span);

        for _ in 0..5 {
            assert!(!push(&mut span, 4));
        }

        assert_eq!(held(&span), [2, 3]);
        drop((first, second));
    }
}

#[test]
fn the_main_thread_takes_the_newest_view() {
    // Goal: the window draws the store's newest View and none it already took. Method: put two
    // Views, take, and take again.
    let mailbox = Mailbox::new();
    mailbox.put(Arc::new(1));
    mailbox.put(Arc::new(2));

    let taken = mailbox.take();

    assert_eq!(taken.as_deref(), Some(&2));
    assert_eq!(mailbox.take(), None);
}

#[test]
fn the_main_thread_never_waits_on_a_view_being_put() {
    // Goal: the main thread keeps its View rather than wait while the store hands one over.
    // Method: hold the mailbox's lock as a put does, and take.
    let mailbox = Mailbox::new();
    mailbox.put(Arc::new(1));
    let putting = mailbox.newest.lock().unwrap();

    let taken = mailbox.take();

    assert_eq!(taken, None);
    drop(putting);
}

#[test]
fn views_taken_while_the_store_writes_never_change() {
    // Goal: no View the main thread reads changes while the store keeps writing. Method: one
    // thread pushes 100,000 readings and puts a View after each, while another takes Views,
    // reads each twice with the writer running between, and checks the readings run on.
    let mailbox = Arc::new(Mailbox::<Vec<Arc<u64>>>::new());
    let writer_mailbox = Arc::clone(&mailbox);
    let writer = thread::spawn(move || {
        let mut span = span(16, 64);
        let mut reading = 1;
        while reading <= 100_000 {
            if push(&mut span, reading) {
                writer_mailbox.put(Arc::new(view(&span)));
                reading += 1;
            } else {
                thread::yield_now();
            }
        }
    });

    let mut checked = 0_u64;
    while !writer.is_finished() {
        let Some(taken) = mailbox.take() else {
            thread::yield_now();
            continue;
        };
        let first: Vec<u64> = taken.iter().map(|reading| **reading).collect();
        thread::yield_now();
        let second: Vec<u64> = taken.iter().map(|reading| **reading).collect();

        assert_eq!(first, second);
        assert!(
            first.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "{first:?}"
        );
        checked += 1;
    }
    writer.join().unwrap();

    assert!(checked > 0);
}
