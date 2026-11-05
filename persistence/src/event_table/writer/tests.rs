use collector::recording::private::{self, PrivateDirectory};
use collector::ring::{Identity, Place, RingReader, RunIdentity, Shape};

use super::{PrefixSource, write_batch};
use crate::event_recovery::batch::{self, Row};
use crate::event_recovery::{Batch, batch_name};
use crate::event_table::index::Index;
use crate::schema::{availability, event_kind};
use crate::window::EventRecord;
use crate::{Error, WindowIdentity};

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const RUN: RunIdentity = RunIdentity {
    recording: Identity([1, 2]),
    run: Identity([3, 4]),
    boot: Identity([5, 6]),
};

/// A ring under a path of its own, removed with the directory beside it when dropped.
struct Ring {
    base: std::path::PathBuf,
    writer: collector::ring::Writer,
}

impl Ring {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("fd-writer-{}-{serial}", std::process::id()));
        private::create_private_dir(&base).unwrap();
        let place = Place {
            lock_path: base.join("ring.lock"),
            name_prefix: format!("/fdv{:x}{serial:x}", std::process::id()),
        };
        let shape = Shape {
            slot_count: 8,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 8,
        };
        let writer = collector::ring::Writer::create_recording(&place, shape, &RUN, 1).unwrap();
        Self { base, writer }
    }

    fn source(&self) -> PrefixSource {
        PrefixSource::of(&RingReader::open_generation(self.writer.name()).unwrap())
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.base).unwrap();
    }
}

#[test]
fn a_source_read_again_opens_as_it_was() {
    // Goal: the thread reading ahead reads the ring the seal registered.
    // Method: open a source from a reader of the ring, and read the identity it finds.
    let ring = Ring::new();

    let reader = ring.source().open().unwrap();

    assert_eq!(reader.identity(), Some(RUN));
}

#[test]
fn a_source_with_another_identity_is_refused() {
    // Goal: rows of two runs never mix in one minute, should a ring change under the seal.
    // Method: open a source that expects another run.
    let ring = Ring::new();
    let source = PrefixSource {
        identity: Some(RunIdentity {
            run: Identity([7, 8]),
            ..RUN
        }),
        ..ring.source()
    };

    let result = source.open();

    assert!(matches!(
        result,
        Err(Error::Bound("event stream source changed"))
    ));
}

#[test]
fn a_source_with_another_layout_is_refused() {
    // Goal: a ring of another layout never reads as the one the seal registered.
    // Method: open a source that expects a layout one newer than the ring's.
    let ring = Ring::new();
    let expected = ring.source();
    let source = PrefixSource {
        layout_version: expected.layout_version + 1,
        ..expected
    };

    let result = source.open();

    assert!(matches!(
        result,
        Err(Error::Bound("event stream source changed"))
    ));
}

#[test]
fn a_batch_past_the_most_an_index_holds_is_refused_before_its_file() {
    // Goal: a minute with too many batches stops before it changes any file.
    // Method: hand a batch to an index that already holds the most, and look for its file.
    let ring = Ring::new();
    let directory = PrivateDirectory::open(&ring.base).unwrap();
    let saved = Batch {
        first: 1,
        end: 2,
        rows: 1,
    };
    let mut index = Index {
        identity: WindowIdentity {
            run: RUN,
            window: 0,
        },
        first: 1,
        end: 2,
        rows: 1,
        identities: false,
        batches: vec![saved; batch::BATCHES_MAX],
    };
    let row = Row {
        publication: None,
        event: EventRecord {
            number: 2,
            tick: 1,
            pid: 41,
            kind: event_kind::EXEC,
            coalition: None,
            usage: None,
            availability: availability::NOT_APPLICABLE,
            noticed: None,
        },
        identity: None,
        name: None,
        app: None,
    };
    let framed = batch::framed(&[row], false).unwrap();

    let result = write_batch(&directory, &mut index, framed);

    assert!(matches!(result, Err(Error::Bound("event stream batches"))));
    assert!(!ring.base.join(batch_name(batch::BATCHES_MAX)).exists());
}
