//! App row storage keeps its admission until the allocation is replaced or dropped.
use std::ops::Deref;
use std::sync::Arc;

use super::{APPS_PER_SECOND_MAX, AppRow, Missing};
use crate::byte_budget::{ByteBudget, ByteReservation};

/// Bounded App rows whose admission follows their allocation through reuse and transfer.
#[derive(Debug, Default)]
pub struct AppRows {
    rows: Vec<AppRow>,
    reservation: Option<ByteReservation>,
}

impl AppRows {
    pub(in crate::store) fn with_capacity(count: usize) -> Self {
        assert!(count <= APPS_PER_SECOND_MAX);
        Self {
            rows: Vec::with_capacity(count),
            reservation: None,
        }
    }

    pub(in crate::store) fn replace_admitted(
        &mut self,
        count: usize,
        budget: &Arc<ByteBudget>,
    ) -> Result<(), Missing> {
        if count > APPS_PER_SECOND_MAX {
            return Err(Missing::TooMany);
        }

        let reservation = budget
            .reserve(count * size_of::<AppRow>())
            .ok_or(Missing::Budget)?;
        let rows = Vec::with_capacity(count);
        assert_eq!(rows.capacity(), count);
        *self = Self {
            rows,
            reservation: Some(reservation),
        };
        Ok(())
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.rows.capacity()
    }

    #[must_use]
    pub fn reserved_bytes(&self) -> usize {
        self.reservation.as_ref().map_or(0, ByteReservation::bytes)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[AppRow] {
        &self.rows
    }

    pub(in crate::store) fn clear(&mut self) {
        self.rows.clear();
    }

    pub(in crate::store) fn push(&mut self, row: AppRow) {
        assert!(self.rows.len() < self.rows.capacity());
        self.rows.push(row);
    }

    pub(in crate::store) fn rows_mut(&mut self) -> &mut [AppRow] {
        &mut self.rows
    }

    pub(in crate::store) fn append(&mut self, other: &mut Self) {
        assert!(self.reservation.is_none());
        assert!(other.reservation.is_none());
        assert!(self.rows.capacity() >= self.rows.len() + other.rows.len());
        self.rows.append(&mut other.rows);
    }
}

impl TryFrom<Vec<AppRow>> for AppRows {
    type Error = Missing;

    fn try_from(rows: Vec<AppRow>) -> Result<Self, Self::Error> {
        if rows.len() > APPS_PER_SECOND_MAX {
            return Err(Missing::TooMany);
        }
        if rows.capacity() > APPS_PER_SECOND_MAX {
            return Err(Missing::TooMany);
        }
        Ok(Self {
            rows,
            reservation: None,
        })
    }
}

impl PartialEq for AppRows {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
    }
}

impl Deref for AppRows {
    type Target = [AppRow];

    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl<'rows> IntoIterator for &'rows AppRows {
    type Item = &'rows AppRow;
    type IntoIter = std::slice::Iter<'rows, AppRow>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}
