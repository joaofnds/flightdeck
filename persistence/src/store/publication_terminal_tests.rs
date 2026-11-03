use super::*;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn completion_rechecks_a_final_publication_after_nothing_without_advancing_loss() {
    // Goal: Nothing before completion cannot certify a stale final publication end.
    // Method: retain an old frontier, observe Nothing, then publish a final next-window tick.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let (stale, _) = settle_latest_ordinary(&mut fixture, &store);
    let mut reader = fixture.reader();
    reader.seek_publication(3).unwrap();
    assert!(matches!(
        reader
            .read_timing_into(&mut collector::ring::PublicationTiming::default())
            .unwrap(),
        collector::ring::Found::Nothing
    ));
    publish_at(&mut fixture.writer, 2);
    fixture.writer.finish();
    reader.seek_publication(4).unwrap();
    let before = fixture.slot();
    assert_eq!(
        store
            .register_attempt(&stale, &mut reader, &registration_budget())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fixture.slot(), before);
    let (fixed, ordinary) = settle_latest_ordinary(&mut fixture, &store);
    assert_eq!(ordinary.publication_first, 3);
    assert_eq!(fixed.losses.publication_inherited_end, 4);
    drop(
        store
            .register_attempt(&fixed, &mut fixture.reader(), &registration_budget())
            .unwrap(),
    );
}

#[test]
fn empty_generation_uses_declared_boundaries_and_publishes_lifecycle_without_a_tick() {
    // Goal: an actual empty identified generation completes without a fabricated ordinary row.
    // Method: finish a producer before its first publication, then publish terminal through Store.
    let mut fixture = Fixture::new_source(false, false, 0, false);
    fixture.writer.finish();
    assert_eq!(
        fixture.reader().publication_newest().unwrap() + 1,
        fixture.reader().publication_first()
    );
    let input = fixture.input(true);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    drop(
        finish_proof(
            attempt
                .prove_terminal(
                    &crate::event_recovery::Recovery {
                        run: fixture.run,
                        events_first: 1,
                        events_end: 1,
                        rows: 0,
                        losses: crate::Losses {
                            publications: 0,
                            events: 0,
                            unknown: true,
                        },
                    },
                    &[],
                    fixture.reader(),
                    &registration_budget(),
                )
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    drop(publication);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &registration_budget(),
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 1);
    assert_eq!(frontier.event_end(), 1);
    assert!(frontier.complete());
    PrivateDirectory::open(&fixture.root)
        .unwrap()
        .open_directory("ordinary")
        .unwrap()
        .visit_names(0, |_| panic!("invented tick"))
        .unwrap();
}
#[test]
fn non_one_empty_generation_retains_prior_canonical_ends_after_transport_disappears() {
    // Goal: a replacement empty generation carries non-1 counters and the same source origin.
    // Method: save the old tick, restart the producer in its retained place, then remove transport.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    fixture.writer.finish();
    settle_latest_ordinary(&mut fixture, &store);
    restart_empty(&mut fixture);
    fixture.writer.finish();
    let reader = fixture.reader();
    assert_eq!(reader.publication_first(), 2);
    assert_eq!(reader.publication_newest().unwrap(), 1);
    let input = terminal_from_frontier(&fixture, &store);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    drop(
        finish_proof(
            attempt
                .prove_terminal(
                    &crate::event_recovery::Recovery {
                        run: fixture.run,
                        events_first: 1,
                        events_end: 1,
                        rows: 0,
                        losses: crate::Losses::default(),
                    },
                    &[],
                    fixture.reader(),
                    &registration_budget(),
                )
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    for _ in 0..128 {
        publication.step().unwrap();
        if phase(&fixture) == Some(Phase::Ready) {
            break;
        }
    }
    assert_eq!(phase(&fixture), Some(Phase::Ready));
    drop(publication);
    drop(store);
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    let store = Store::open(&fixture.root).unwrap();
    let mut publication = store.resume_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    drop(publication);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &registration_budget(),
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.event_end(), 1);
    assert!(frontier.complete());
}
#[test]
fn final_header_overflow_and_event_mismatch_refuse_before_slot_or_raw_mutation() {
    // Goal: neither final header can wrap and mismatched fixed event coverage cannot register.
    // Method: mutate each actual mapped header only after opening its admitted reader, restoring
    // it.
    for position in [4, 7] {
        let mut fixture = Fixture::new(false);
        fixture.writer.finish();
        let store = Store::open(&fixture.root).unwrap();
        let before = fixture.slot();
        let mut header = WritableHeader::open(&fixture);
        let mut reader = fixture.reader();
        let previous = header.word(position).swap(u64::MAX, Ordering::Release);
        let result =
            store.register_attempt(&fixture.input(true), &mut reader, &registration_budget());
        header.word(position).store(previous, Ordering::Release);
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fixture.slot(), before);
        PrivateDirectory::open(&fixture.root)
            .unwrap()
            .open_directory("recovery-data")
            .unwrap()
            .visit_names(0, |_| panic!("artifact before endpoint proof"))
            .unwrap();
    }
    let mut fixture = Fixture::new(false);
    fixture.writer.finish();
    let store = Store::open(&fixture.root).unwrap();
    let (mut input, _) = settle_latest_ordinary(&mut fixture, &store);
    input.operation = AttemptOperation::Terminal { event_end: 2 };
    let before = fixture.slot();
    assert_eq!(
        store
            .register_attempt(&input, &mut fixture.reader(), &registration_budget())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fixture.slot(), before);
}
struct WritableHeader {
    pointer: *mut AtomicU64,
    length: usize,
    _file: File,
}
unsafe extern "C" {
    fn shm_open(name: *const std::ffi::c_char, flags: i32, ...) -> i32;
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
}
impl WritableHeader {
    fn open(fixture: &Fixture) -> Self {
        let name = std::ffi::CString::new(fixture.writer.name()).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), 0) };
        assert!(descriptor >= 0);
        let old = unsafe { File::from_raw_fd(descriptor) };
        let length = usize::try_from(old.metadata().unwrap().len()).unwrap();
        assert!(length <= 1024 * 1024);
        let original = unsafe { mmap(std::ptr::null_mut(), length, 1, 1, old.as_raw_fd(), 0) };
        assert_ne!(original.addr(), usize::MAX);
        let bytes = unsafe { std::slice::from_raw_parts(original.cast::<u8>(), length) }.to_vec();
        assert_eq!(unsafe { munmap(original, length) }, 0);
        drop(old);
        // Source objects deliberately refuse new writable opens. As in the existing worker
        // forged-source fixture, recreate only this owned name with an initial writable fd.
        collector::ring::unlink_generation(fixture.writer.name()).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), 0xa02, 0o400_u32) };
        assert!(descriptor >= 0);
        let file = unsafe { File::from_raw_fd(descriptor) };
        file.set_len(u64::try_from(length).unwrap()).unwrap();
        let pointer = unsafe { mmap(std::ptr::null_mut(), length, 3, 1, file.as_raw_fd(), 0) };
        assert_ne!(pointer.addr(), usize::MAX);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), length);
        }
        Self {
            pointer: pointer.cast(),
            length,
            _file: file,
        }
    }
    fn word(&mut self, position: usize) -> &AtomicU64 {
        assert!(position < 8);
        unsafe { &*self.pointer.add(position) }
    }
}
impl Drop for WritableHeader {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.pointer.cast(), self.length) }, 0);
    }
}

fn restart_empty(fixture: &mut Fixture) {
    let shape = fixture.reader().shape();
    let old_name = fixture.writer.name().to_owned();
    let temporary_place = collector::ring::Place {
        lock_path: fixture.base.join("temporary.lock"),
        name_prefix: format!(
            "/fde{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let temporary =
        collector::ring::Writer::create_recording(&temporary_place, shape, &fixture.run, 1)
            .unwrap();
    let old = std::mem::replace(&mut fixture.writer, temporary);
    drop(old);
    let next =
        collector::ring::Writer::create_recording(&fixture.place, shape, &fixture.run, 1).unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = next;
    fixture.binding = fixture.reader().object_binding().unwrap();
    collector::ring::unlink_generation(&old_name).unwrap();
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
}
