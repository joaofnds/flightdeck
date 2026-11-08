//! Compact App facts a session retains for its App histories.
use std::cell::RefCell;

use crate::apps::{self, AppKey};
use crate::live_app_response::{KeyKind, Observation};
use crate::session::OBSERVATIONS_MAX;

mod attribution;
mod compact;
mod merge;
mod process;
pub(crate) use process::add_process_view;
pub(crate) use process::cohort;
pub(crate) use process::{GroupTarget, Selection};
mod resident;
mod resident_cache;
pub(crate) use merge::Merge;
pub(crate) use resident_cache::{
    PIN_BYTES_MAX, ResidentCache, ResidentFile, ResidentPin, ResidentPoint,
};
mod trace;
pub(crate) use compact::CompactMeasure;
use compact::count;
pub(crate) use trace::TracePoint;

pub const BYTES_MAX: usize = 72 * 1024 * 1024;
const _: () = assert!(BYTES_MAX > crate::live_app_response::SAMPLE_BYTES_MAX);

/// What a session keeps of the App histories it read from the saved minutes.
pub(super) struct History {
    saved_residents: RefCell<ResidentCache>,
}

pub(super) struct Target<'bytes> {
    pub kind: KeyKind,
    pub bytes: &'bytes [u8],
}

impl History {
    pub(super) fn total_bytes(&self) -> usize {
        self.saved_residents.borrow().bytes()
    }

    pub(super) fn new() -> Self {
        Self {
            saved_residents: RefCell::new(ResidentCache::new()),
        }
    }
}

fn key(kind: KeyKind, bytes: &[u8]) -> AppKey<'_> {
    match kind {
        KeyKind::Bundle => AppKey::Bundle(bytes),
        KeyKind::Executable => AppKey::Executable(bytes),
    }
}

#[cfg(test)]
mod merge_tests;
