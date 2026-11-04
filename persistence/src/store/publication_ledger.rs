//! Caller-retained cumulative accounting, including failed capability construction.
use super::PublicationWork;
use crate::resources::{ByteBudget, ByteReservation};
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct PublicationLedger {
    inner: Arc<LedgerState>,
}
struct LedgerState {
    work: Mutex<PublicationWork>,
    _reservation: ByteReservation,
    budget: Arc<ByteBudget>,
}
const _: () = assert!(size_of::<LedgerState>() + 64 < 1024);
impl PublicationLedger {
    pub fn new(budget: &Arc<ByteBudget>) -> io::Result<Self> {
        let reservation = budget
            .reserve(1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            inner: Arc::new(LedgerState {
                work: Mutex::new(PublicationWork::default()),
                _reservation: reservation,
                budget: Arc::clone(budget),
            }),
        })
    }
    pub(super) fn require_budget(&self, budget: &Arc<ByteBudget>) -> io::Result<()> {
        if !Arc::ptr_eq(budget, &self.inner.budget) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        Ok(())
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, PublicationWork> {
        match self.inner.work.lock() {
            Ok(work) => work,
            Err(error) => panic!("publication accounting poisoned: {error}"),
        }
    }
    #[must_use]
    pub fn work(&self) -> PublicationWork {
        *self.lock()
    }
    pub(super) fn project(
        &self,
        current: &PublicationWork,
        previous: &PublicationWork,
    ) -> PublicationWork {
        let mut work = self.work();
        record_delta(&mut work, current, previous);
        work
    }
    pub(super) fn record(&self, current: &PublicationWork, previous: &mut PublicationWork) {
        let mut total = self.lock();
        record_delta(&mut total, current, previous);
        *previous = *current;
    }
    pub(super) fn update(&self, update: impl FnOnce(&mut PublicationWork)) {
        update(&mut self.lock());
    }
    pub(super) fn namespace_attempt(&self) {
        self.lock().canonical.namespace_attempts += 1;
    }
}
macro_rules! add_fields {
    ($total:expr, $current:expr, $previous:expr; $($field:ident),+ $(,)?) => {
        $(assert!($current.$field >= $previous.$field);
          $total.$field += $current.$field - $previous.$field;)+
    };
}
fn record_delta(
    total: &mut PublicationWork,
    current: &PublicationWork,
    previous: &PublicationWork,
) {
    add_fields!(total, current, previous;
        raw_read_operations, raw_read_requested_bytes,
        raw_write_operations, raw_write_requested_bytes,
        census_write_operations, census_write_requested_bytes,
        decode_admission_bytes, links, controls,
        control_reads, full_syncs,
        source_reads, source_observations, proof_operations, cleanup_positions,
        cleanup_file_attempts,
        cleanup_directory_attempts, cleanup_removals);
    if current.proof_publication_next != 0 {
        total.proof_publication_next = current.proof_publication_next;
    }
    if current.proof_event_next != 0 {
        total.proof_event_next = current.proof_event_next;
    }
    record_control(total, current, previous);
    record_canonical(
        &mut total.canonical,
        &current.canonical,
        &previous.canonical,
    );
    record_inventory(
        &mut total.inventory,
        &current.inventory,
        &previous.inventory,
    );
}
fn record_canonical(
    total: &mut crate::canonical::ReadWork,
    current: &crate::canonical::ReadWork,
    previous: &crate::canonical::ReadWork,
) {
    add_fields!(total, current, previous; namespace_attempts, directory_reads, entries,
        native_entries, metadata_admissions, raw_admissions, node_attempts, links,
        full_syncs, creates, renames, removals);
}
fn record_inventory(
    total: &mut super::InventoryWork,
    current: &super::InventoryWork,
    previous: &super::InventoryWork,
) {
    add_fields!(total, current, previous; positions, open_attempts, reopens, raw_admissions,
        decoded_bytes, index_bytes, raw_file_clones);
    let (total, current, previous) = (
        &mut total.directory,
        &current.directory,
        &previous.directory,
    );
    add_fields!(total, current, previous; open_attempts, read_calls, entries, names);
}

fn record_control(
    total: &mut PublicationWork,
    current: &PublicationWork,
    previous: &PublicationWork,
) {
    add_fields!(total, current, previous; control_file_opens, control_file_creates,
        control_read_operations, control_read_bytes, control_write_operations,
        control_write_requested_bytes, control_rename_attempts, control_remove_attempts,
        control_file_sync_attempts, control_directory_sync_attempts);
}

pub(super) struct LedgerWriter<'file> {
    pub file: &'file mut std::fs::File,
    pub ledger: &'file PublicationLedger,
}
impl io::Write for LedgerWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.ledger.update(|work| {
            work.raw_write_operations += 1;
            work.raw_write_requested_bytes += collector::u64_from_usize(bytes.len());
        });
        io::Write::write(self.file, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(self.file)
    }
}

impl crate::engine::RawTableWork for PublicationLedger {
    fn read_attempt(&self, requested: usize) {
        self.update(|work| {
            work.raw_read_operations += 1;
            work.raw_read_requested_bytes += collector::u64_from_usize(requested);
        });
    }
    fn seek_attempt(&self) {
        self.update(|work| work.proof_operations += 1);
    }
    fn decode_attempt(&self, mapped_bytes: u64) {
        self.update(|work| {
            work.proof_operations += 1;
            work.decode_admission_bytes += mapped_bytes;
        });
    }
}
