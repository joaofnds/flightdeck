use std::cell::Cell;
use std::rc::Rc;
use std::thread;

use core_foundation::runloop::{
    CFRunLoopGetCurrent, CFRunLoopRunInMode, kCFRunLoopDefaultMode, kCFRunLoopRunFinished,
    kCFRunLoopRunHandledSource,
};

use super::Wake;

/// The longest a test waits for its run loop to handle a signal, far past the microseconds
/// a signal takes, so only a signal that never arrives reaches it.
const SIGNAL_WAIT_SECONDS: f64 = 5.0;
/// The longest a test runs its run loop to see that a signal runs nothing, many times the
/// microseconds a signal that did run would take.
const NOTHING_WAIT_SECONDS: f64 = 0.2;

#[test]
fn a_signal_from_another_thread_runs_the_callback_on_the_run_loop() {
    // Goal: the store's thread wakes the page's run loop, which runs the page's tick, so a View
    // is drawn as it arrives rather than at the page's next timer tick. Method: a wake on this
    // thread's run loop, a signal from another thread, and the run loop run until it handles
    // a source.
    let runs = Rc::new(Cell::new(0_u32));
    let counter = Rc::clone(&runs);
    let wake = unsafe {
        Wake::on_run_loop(CFRunLoopGetCurrent(), kCFRunLoopDefaultMode, move || {
            counter.set(counter.get() + 1);
        })
    }
    .unwrap();
    let signal = wake.signal();

    let signaller = thread::spawn(move || signal.signal());
    let handled = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, SIGNAL_WAIT_SECONDS, 1) };
    signaller.join().unwrap();

    assert_eq!(handled, kCFRunLoopRunHandledSource);
    assert_eq!(runs.get(), 1);
}

#[test]
fn a_signal_after_the_wake_is_dropped_runs_nothing() {
    // Goal: quitting drops the page's wake, so a publication that arrives after it ticks no
    // page. Method: a wake on this thread's run loop, dropped, then a signal from another thread
    // through the handle the store keeps, and the run loop run, which finds no source left.
    let runs = Rc::new(Cell::new(0_u32));
    let counter = Rc::clone(&runs);
    let wake = unsafe {
        Wake::on_run_loop(CFRunLoopGetCurrent(), kCFRunLoopDefaultMode, move || {
            counter.set(counter.get() + 1);
        })
    }
    .unwrap();
    let signal = wake.signal();
    drop(wake);

    let signaller = thread::spawn(move || signal.signal());
    signaller.join().unwrap();
    let handled = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, NOTHING_WAIT_SECONDS, 1) };

    assert_eq!(handled, kCFRunLoopRunFinished);
    assert_eq!(runs.get(), 0);
}
