//! The Apps a stretch adds up, kept with the closing number that names them, so that no code
//! outside this module changes the Apps without the number changing with them.

use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{STRETCH_APPS_MAX, StretchRow};
use crate::live_app_response::Rankable;
use crate::store::attribution::{PointRow, Totals};
use crate::store::key_order;

/// The closing number the next stretch to close takes, counting every close in the process.
/// Numbers start at one, so zero names Apps no close finished.
static CLOSING_NUMBER_NEXT: AtomicU64 = AtomicU64::new(1);

/// Every App of a stretch's seconds, once each, in key order, what they add up to, and the
/// closing number that names them.
///
/// Two stretches of the same nonzero closing number hold the same Apps, which lets a room that
/// added up a stretch's Apps keep their sums by its number alone. Each close takes a number
/// from one counter of the whole process, which hands no number out twice. Every other change
/// to the Apps zeroes the number, and only this module changes them. A copy takes the number
/// with the very Apps it names.
#[derive(Debug, Default)]
pub(super) struct StretchApps {
    /// Whether the stretch's seconds hold more Apps between them than a stretch holds, so it
    /// holds none.
    too_many: bool,
    /// Every App of the stretch's seconds, once each, in key order.
    rows: Vec<StretchRow>,
    /// Every App of `rows` added up, once the stretch is closed.
    totals: Totals,
    /// The number the last close took, or zero where the Apps changed since.
    closing_number: u64,
}

impl StretchApps {
    /// Room for `STRETCH_APPS_MAX` Apps, holding none.
    pub(super) fn with_room() -> Self {
        Self {
            rows: Vec::with_capacity(STRETCH_APPS_MAX),
            ..Self::default()
        }
    }

    /// Holds no App, keeping the room for them.
    pub(super) fn clear(&mut self) {
        self.too_many = false;
        self.rows.clear();
        self.totals = Totals::default();
        self.closing_number = 0;
    }

    /// Holds the Apps `other` holds, writing them into the room this holds for them, and the
    /// number that names them, since they are its very Apps.
    pub(super) fn copy_from(&mut self, other: &Self) {
        let Self {
            too_many,
            rows,
            totals,
            closing_number,
        } = other;
        self.too_many = *too_many;
        self.rows.clone_from(rows);
        self.totals = *totals;
        self.closing_number = *closing_number;
    }

    /// Adds `other`, the Apps of the same stretch over seconds this does not hold, writing them
    /// into `spare`, which then trades places with them.
    pub(super) fn add_stretch(&mut self, other: &Self, spare: &mut Vec<StretchRow>) {
        self.closing_number = 0;
        if other.too_many {
            self.rows.clear();
            self.too_many = true;
        }

        self.add(&other.rows, spare, StretchRow::add_row);
    }

    /// Adds `incoming`, Apps in key order, to these with `add`, writing them into `spare`, which
    /// then trades places with them. Where they come to more than `STRETCH_APPS_MAX` Apps, this
    /// holds none.
    pub(super) fn add<R: PointRow + Rankable>(
        &mut self,
        incoming: &[R],
        spare: &mut Vec<StretchRow>,
        add: impl Fn(&mut StretchRow, &R),
    ) {
        self.closing_number = 0;
        if self.too_many {
            return;
        }

        let capacity = spare.capacity();
        spare.clear();
        let fits = key_order::add_merged(&mut self.rows, incoming, spare, STRETCH_APPS_MAX, add);
        mem::swap(&mut self.rows, spare);
        if !fits {
            self.rows.clear();
            self.too_many = true;
        }
        assert_eq!(spare.capacity(), capacity);
    }

    /// Finds each App's resident memory over the stretch's `seconds` that hold their Apps, adds
    /// the Apps up, and takes the next closing number, once the store adds no more seconds.
    pub(super) fn close(&mut self, seconds: u32) {
        let seconds = u64::from(seconds);
        for row in &mut self.rows {
            row.close(seconds);
        }
        self.totals = Totals::of_rows(&self.rows);
        self.closing_number = CLOSING_NUMBER_NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(self.closing_number > 0);
        assert!(self.closing_number < u64::MAX);
    }

    /// The number the last close took, which names these Apps, as the type's doc says, or zero
    /// where no close finished them.
    pub(super) fn closing_number(&self) -> u64 {
        self.closing_number
    }

    /// Whether the stretch's seconds held more Apps between them than a stretch holds.
    pub(super) fn too_many(&self) -> bool {
        self.too_many
    }

    /// Every App of the stretch's seconds, once each, in key order, which is none where they
    /// held too many.
    pub(super) fn rows(&self) -> &[StretchRow] {
        &self.rows
    }

    /// Every App added up, once the stretch is closed.
    pub(super) fn totals(&self) -> &Totals {
        &self.totals
    }
}
