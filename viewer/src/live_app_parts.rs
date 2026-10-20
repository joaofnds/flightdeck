//! The selected App's part of every observation in the selected window, so a hover on the Apps
//! page shows any second's App detail at once.
use std::sync::Arc;

use persistence::{Error, Result};

use super::window::{self, Window, WindowValue};
use crate::LiveSession;
use crate::live_app_response::{AppPart, KeyKind, Observation};
use crate::session::ContextRange;

pub type WindowParts = Window<AppPart>;

/// The App whose parts the session keeps.
pub struct PartsApp {
    kind: KeyKind,
    bytes: Box<[u8]>,
}

impl WindowValue for AppPart {
    /// About 280 bytes a process, so an App of about 250 processes at every second of a full
    /// window. A larger App keeps a part for only some of the window's seconds, and a hover
    /// reads the others.
    const BYTES_MAX: usize = 64 * 1024 * 1024;

    type Selection = PartsApp;

    fn selected(&self) -> &Observation {
        AppPart::selected(self)
    }

    fn bytes(&self) -> usize {
        AppPart::bytes(self)
    }

    /// No sealed minute keeps an App's parts, so the replay gives them all.
    fn visit_kept(
        _history: &crate::Session,
        _app: &PartsApp,
        _range: ContextRange,
        _consume: impl FnMut(Self) -> Result<()>,
    ) -> Result<()> {
        Ok(())
    }
}

impl LiveSession {
    /// The selected App's parts of the selected window's observations, as far as they are kept.
    #[must_use]
    pub fn app_parts(&self) -> Arc<WindowParts> {
        Arc::clone(&self.app_parts)
    }

    /// Keeps the part of the App with this key for every observation of the selected window
    /// that has none yet. Keeping another App's parts drops those of the App kept before.
    pub fn part_window(&mut self, kind: KeyKind, bytes: &[u8]) -> Result<()> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| {
            let app = PartsApp {
                kind,
                bytes: bytes.into(),
            };
            let mut parts = std::mem::take(&mut self.app_parts);
            if !self
                .parts_app
                .as_ref()
                .is_some_and(|kept| kept.is(kind, bytes))
            {
                parts = Arc::default();
            }

            let parted = self.fill_window(&mut parts, &app, |sample| sample.app_part(kind, bytes));
            self.app_parts = parts;
            self.parts_app = Some(app);

            parted
        })
    }

    /// Keeps the kept App's part of the observation the session polled last, and drops those
    /// older than its window.
    pub(super) fn poll_parts(&mut self, first_ns: u64) -> Result<()> {
        let Some(app) = &self.parts_app else {
            return Ok(());
        };
        let current = self
            .observations
            .back()
            .ok_or(Error::Bound("App part live observation"))?;
        let part = current
            .app_sample
            .as_deref()
            .ok_or(Error::Bound("App part live sample"))?
            .app_part(app.kind, &app.bytes)?;

        window::poll(&mut self.app_parts, first_ns, part)
    }
}

impl PartsApp {
    fn is(&self, kind: KeyKind, bytes: &[u8]) -> bool {
        if self.kind != kind {
            return false;
        }

        *self.bytes == *bytes
    }
}

#[cfg(test)]
#[path = "live_app_parts_tests.rs"]
mod tests;
