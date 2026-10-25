//! Isolated correctness observation; native wrappers remain outside the input reservation.
use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use persistence::engine::{AdmittedFile, AdmittedTable};
use persistence::resources::ByteBudget;
use polars::prelude::{Column, DataFrame, DslPlan, ParquetWriter, ScanSources};

#[path = "admitted_buffer_observation/recorder.rs"]
mod recorder;
use recorder::{Capture, Event, Kind, Ledger};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    // Goal: one thread's wrapping trace detects invalid observations and matches all owners.
    // Method: use a separate harness-free test process, never native schema or collection.
    transition_checks();
    invalidation_checks()?;
    wrapping_observation()?;
    println!("isolated wrapping/owner matching and recorder invalidations passed");

    Ok(())
}

fn wrapping_observation() -> Result<()> {
    let fixture = Fixture::new()?;
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let file = File::open(&fixture.path)?;

    let length = usize::try_from(file.metadata()?.len())?;
    let capture = Capture::start(&budget);
    let table = AdmittedTable::from_file_budgeted(file, &budget)
        .map_err(|error| format!("admission: {error:?}"))?;
    let wrapping = capture.count();
    let charge = budget.used();
    let (plan, names) = table.into_scan()?;
    let constructed = capture.count();
    let DslPlan::Scan {
        sources: ScanSources::Buffers(buffers),
        ..
    } = &plan.logical_plan
    else {
        return Err("expected buffer scan".into());
    };
    let pointer = buffers[0].as_ptr().addr();

    drop(plan);
    drop(names);
    let trace = capture.finish();

    report(&trace, wrapping, constructed);
    assert_eq!(trace.validity(), 0);
    let mut ledger = Ledger::EMPTY;
    for event in &trace.events[..wrapping] {
        ledger.apply(*event);
    }
    assert_eq!(ledger.faults, 0);
    assert_eq!(ledger.live(), 2);
    assert!(ledger.matches(pointer, length, 1));
    // One field retains the pinned ColumnName layout: 128 bytes plus its length byte.
    assert_eq!(charge, length + 129);
    assert_wrappers(&trace.events[wrapping..constructed]);
    for event in &trace.events[wrapping..trace.count] {
        ledger.apply(*event);
    }
    assert_eq!(ledger.complete(), 0);
    let backing: Vec<_> = trace.events[constructed..trace.count]
        .iter()
        .filter(|event| event.pointer == pointer)
        .collect();
    assert_eq!(backing.len(), 1);
    assert_eq!(backing[0].kind, Kind::Free);
    assert_eq!(backing[0].bytes, length);
    assert!(backing[0].charge >= length);
    assert_eq!(budget.used(), 0);
    println!(
        "C={length}; matched {} events through destruction; wrapping/plan={} events",
        trace.count,
        trace.count - wrapping
    );

    Ok(())
}

fn report(trace: &recorder::Trace, wrapping: usize, constructed: usize) {
    println!(
        "trace attempts={} faults={} wrapping={wrapping} constructed={constructed}",
        trace.attempts, trace.faults
    );
    for (index, event) in trace.events[..trace.count].iter().enumerate() {
        println!("event {index}: {event:?}");
    }
}

fn assert_wrappers(events: &[Event]) {
    assert!(events.len() >= 3);
    // Foreign-owner box, SharedStorageInner (three words plus tag/two-word payload), source Arc.
    let expected = [
        size_of::<AdmittedFile>(),
        6 * size_of::<usize>(),
        2 * size_of::<usize>() + size_of::<polars_buffer::Buffer<u8>>(),
    ];
    for (event, bytes) in events[..3].iter().zip(expected) {
        assert_eq!(event.kind, Kind::Allocate);
        assert_ne!(event.pointer, 0);
        assert_eq!(event.bytes, bytes);
        assert_eq!(event.alignment, align_of::<usize>());
    }
}

fn transition_checks() {
    // Failed resize preserves old storage; successful resize can stay in place or move.
    let allocation = Event {
        pointer: 16,
        bytes: 8,
        alignment: 8,
        ..Event::EMPTY
    };
    let mut failed = Ledger::EMPTY;
    failed.apply(allocation);
    failed.apply(Event {
        kind: Kind::Resize,
        old_pointer: 16,
        old_bytes: 8,
        pointer: 0,
        bytes: 32,
        alignment: 8,
        ..Event::EMPTY
    });
    assert!(failed.matches(16, 8, 8));
    assert_eq!(failed.faults, recorder::FAILED);
    let mut resized = Ledger::EMPTY;
    resized.apply(allocation);
    resized.apply(Event {
        kind: Kind::Resize,
        old_pointer: 16,
        old_bytes: 8,
        pointer: 16,
        bytes: 16,
        alignment: 8,
        ..Event::EMPTY
    });
    assert!(resized.matches(16, 16, 8));
    resized.apply(Event {
        kind: Kind::Resize,
        old_pointer: 16,
        old_bytes: 16,
        pointer: 32,
        bytes: 32,
        alignment: 8,
        ..Event::EMPTY
    });
    assert!(!resized.matches(16, 16, 8));
    assert!(resized.matches(32, 32, 8));
    resized.apply(Event {
        kind: Kind::Free,
        pointer: 32,
        bytes: 32,
        alignment: 8,
        ..Event::EMPTY
    });
    assert_eq!(resized.complete(), 0);
    invalid_owner_checks(allocation);
}

fn invalid_owner_checks(allocation: Event) {
    let mut unknown = Ledger::EMPTY;
    unknown.apply(Event {
        kind: Kind::Free,
        ..allocation
    });
    assert_eq!(unknown.faults, recorder::UNMATCHED);
    let mut duplicate = Ledger::EMPTY;
    duplicate.apply(allocation);
    duplicate.apply(allocation);
    assert_eq!(duplicate.faults, recorder::UNMATCHED);
    let mut wrong_layout = Ledger::EMPTY;
    wrong_layout.apply(allocation);
    wrong_layout.apply(Event {
        kind: Kind::Free,
        bytes: 16,
        ..allocation
    });
    assert_eq!(wrong_layout.faults, recorder::UNMATCHED);
    assert!(wrong_layout.matches(16, 8, 8));
    let mut wrong_alignment = Ledger::EMPTY;
    wrong_alignment.apply(allocation);
    wrong_alignment.apply(Event {
        kind: Kind::Free,
        alignment: 1,
        ..allocation
    });
    assert_eq!(wrong_alignment.faults, recorder::UNMATCHED);
    let mut unmatched = Ledger::EMPTY;
    unmatched.apply(allocation);
    assert_eq!(unmatched.complete(), recorder::UNMATCHED);
    let mut failed = Ledger::EMPTY;
    failed.apply(Event {
        pointer: 0,
        ..allocation
    });
    assert_eq!(failed.complete(), recorder::FAILED);
    let mut zeroed = Ledger::EMPTY;
    zeroed.apply(Event {
        kind: Kind::Zeroed,
        ..allocation
    });
    zeroed.apply(Event {
        kind: Kind::Free,
        ..allocation
    });
    assert_eq!(zeroed.complete(), 0);
}

fn invalidation_checks() -> Result<()> {
    let budget = ByteBudget::new(0);
    let lost = recorder::reentrant_events(&budget);
    assert_ne!(lost.faults & recorder::LOST, 0);
    assert_ne!(lost.validity() & recorder::LOST, 0);
    let outstanding = recorder::outstanding_call_events(&budget);
    assert_ne!(outstanding.faults & recorder::LOST, 0);
    let capture = Capture::start(&budget);
    for _index in 0..=recorder::EVENTS_MAX {
        drop(std::hint::black_box(Box::new(11_u64)));
    }
    let overflow = capture.finish();
    assert_ne!(overflow.validity() & recorder::OVERFLOW, 0);
    let mut missing = overflow;
    missing.faults = 0;
    assert_ne!(missing.validity() & recorder::LOST, 0);
    foreign_events(&budget)?;

    Ok(())
}

fn foreign_events(budget: &ByteBudget) -> Result<()> {
    let start = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let finish = Arc::new(AtomicBool::new(false));
    let worker = (Arc::clone(&start), Arc::clone(&done), Arc::clone(&finish));
    let child = std::thread::spawn(move || {
        wait(&worker.0);
        drop(std::hint::black_box(Box::new(19_u64)));
        worker.1.store(true, Ordering::SeqCst);
        wait(&worker.2);
    });
    let capture = Capture::start(budget);
    start.store(true, Ordering::SeqCst);
    wait(&done);
    let foreign = capture.finish();
    finish.store(true, Ordering::SeqCst);
    child.join().map_err(|_| "foreign-event worker panicked")?;
    assert_ne!(foreign.validity() & recorder::FOREIGN, 0);

    Ok(())
}

fn wait(signal: &AtomicBool) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(2) {
        if signal.load(Ordering::SeqCst) {
            return;
        }
        std::thread::yield_now();
    }
    panic!("allocation observation handshake timed out");
}

struct Fixture {
    path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let serial = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "flightdeck-buffer-trace-{}-{serial}.parquet",
            std::process::id()
        ));

        let file = File::create_new(&path)?;

        let fixture = Self { path };
        let mut frame = DataFrame::new(1, vec![Column::new("counter".into(), [42_u64])])?;
        ParquetWriter::new(file)
            .with_compression(polars::prelude::ParquetCompression::Uncompressed)
            .with_statistics(polars::prelude::StatisticsOptions {
                min_value: true,
                max_value: true,
                distinct_count: false,
                null_count: true,
                binary_statistics_truncate_length: None,
            })
            .with_row_group_size(Some(16_384))
            .with_data_page_size(None)
            .set_parallel(false)
            .with_key_value_metadata(None)
            .with_context_info(None)
            .finish(&mut frame)?;

        Ok(fixture)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            panic!("cannot remove {}: {error}", self.path.display());
        }
    }
}
