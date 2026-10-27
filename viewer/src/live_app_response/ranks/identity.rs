use std::ops::Deref;
use std::sync::Arc;

use persistence::resources::{ByteBudget, ByteReservation};

use super::{App, KeyKind, Result};

/// An immutable App identity. A retained copy owns its charge until its last alias drops.
///
/// Its buffers cannot be moved out even after unwrapping its last Arc:
/// ```compile_fail
/// use viewer::live_app_response::{KeyKind, RankedApp};
/// let mut app = RankedApp::named(KeyKind::Executable, b"x".to_vec(), String::from("x"));
/// let bytes = std::mem::take(&mut app.bytes);
/// ```
/// ```compile_fail
/// use viewer::live_app_response::{KeyKind, RankedApp};
/// let app = RankedApp::named(KeyKind::Executable, b"x".to_vec(), String::from("x"));
/// let display = app.display_name;
/// ```
#[derive(Debug)]
pub struct RankedApp {
    identity: AppIdentity,
    reservation: Option<ByteReservation>,
}

/// Borrowed through a ranked App; no mutable access to its backing storage is exposed.
#[derive(Debug, PartialEq)]
pub struct AppIdentity {
    pub kind: KeyKind,
    pub bytes: Vec<u8>,
    pub display_name: String,
}

impl RankedApp {
    /// An App identity supplied by an already captured Sample or presentation fixture.
    #[must_use]
    pub fn named(kind: KeyKind, bytes: Vec<u8>, display_name: String) -> Self {
        Self {
            identity: AppIdentity {
                kind,
                bytes,
                display_name,
            },
            reservation: None,
        }
    }

    pub(crate) fn new(kind: KeyKind, bytes: Vec<u8>) -> Result<Self> {
        let display_name = super::super::capture::display_name(kind, &bytes)?;
        Ok(Self::named(kind, bytes, display_name))
    }

    pub(crate) fn retained(name: &Arc<Self>, budget: &Arc<ByteBudget>) -> Option<Arc<Self>> {
        if name.reservation.is_some() {
            return Some(Arc::clone(name));
        }

        Self::copy(name.kind, &name.bytes, &name.display_name, budget)
    }

    pub(crate) fn captured(app: &App, budget: &Arc<ByteBudget>) -> Option<Arc<Self>> {
        Self::copy(app.key.kind, &app.key.bytes, &app.display_name, budget)
    }

    fn copy(
        kind: KeyKind,
        bytes: &[u8],
        display_name: &str,
        budget: &Arc<ByteBudget>,
    ) -> Option<Arc<Self>> {
        let requested = (2 * size_of::<usize>() + size_of::<Self>())
            .checked_add(bytes.len())?
            .checked_add(display_name.len())?;
        let reservation = budget.reserve(requested)?;

        let mut retained = Self::named(kind, bytes.to_vec(), display_name.to_owned());
        assert!(retained.bytes.capacity() <= bytes.len());
        assert!(retained.display_name.capacity() <= display_name.len());
        retained.reservation = Some(reservation);

        Some(Arc::new(retained))
    }

    /// Charge held by this identity, excluding any enclosing rows, ranks or contexts.
    #[must_use]
    pub fn reserved_bytes(&self) -> usize {
        self.reservation.as_ref().map_or(0, ByteReservation::bytes)
    }

    /// The App's whole key as text, as the worker's App histories display it.
    #[must_use]
    pub fn display(&self) -> String {
        let mut display = String::with_capacity(self.bytes.len() * 4);
        crate::byte_display::append(&mut display, &self.bytes);
        display
    }
}

impl PartialEq for RankedApp {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Deref for RankedApp {
    type Target = AppIdentity;

    fn deref(&self) -> &Self::Target {
        &self.identity
    }
}
