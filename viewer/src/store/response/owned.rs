use std::ops::Deref;

use persistence::{Error, Result};
use serde_json::Value;

use crate::store::ByteReservation;

/// A presentation value whose admission remains owned through chart parsing and UI retention.
#[must_use]
pub struct Response<T> {
    value: T,
    reservation: ByteReservation,
}

impl<T> Response<T> {
    pub(crate) fn reserved(reservation: ByteReservation, value: T) -> Self {
        Self { value, reservation }
    }

    /// Transforms a response without releasing the allocation's admission owner.
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Response<U> {
        let Self { value, reservation } = self;

        Response {
            value: map(value),
            reservation,
        }
    }

    /// A failed transformation releases admission; a successful one carries it to the UI value.
    pub fn try_map<U, E>(
        self,
        map: impl FnOnce(T) -> std::result::Result<U, E>,
    ) -> std::result::Result<Response<U>, E> {
        let Self { value, reservation } = self;
        let value = map(value)?;

        Ok(Response { value, reservation })
    }

    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes()
    }
}

impl Response<Value> {
    /// The caller reserves the operation bound before building the JSON value.
    pub(crate) fn json(mut reservation: ByteReservation, value: Value) -> Result<Self> {
        let value = crate::live_app_response::bounded(value)?;
        let bytes = size_of::<Self>() + super::json_charge(&value)?;
        if bytes > reservation.bytes() {
            return Err(Error::Bound("Store presentation admission bytes"));
        }
        reservation.shrink_to(bytes);

        Ok(Self::reserved(reservation, value))
    }
}

impl<T> Deref for Response<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
