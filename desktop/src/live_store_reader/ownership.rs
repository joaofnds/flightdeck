//! Hover windows keep the reservation owner of whichever reader supplied their facts.
use std::ops::Deref;
use std::sync::Arc;

use serde_json::Value;
use viewer::live_app_response::{AppPart, KeyKind, Page};
use viewer::store::{ContextPart, ContextParts, ContextRanks};
use viewer::{ObservationKey, WindowParts, WindowRanks};

use super::presentation::Presentation;

pub enum RanksWindow {
    Legacy(Arc<WindowRanks>),
    Store(Arc<ContextRanks<WindowRanks>>),
}

pub enum PartsWindow {
    Legacy(Arc<WindowParts>),
    Store(Arc<ContextParts>),
}

pub enum Part<'context> {
    Legacy(&'context AppPart),
    Store(ContextPart<'context>),
}

impl Deref for RanksWindow {
    type Target = WindowRanks;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Legacy(window) => window,
            Self::Store(window) => window,
        }
    }
}

impl PartsWindow {
    pub fn get(&self, key: &ObservationKey) -> Option<Part<'_>> {
        match self {
            Self::Legacy(window) => window.get(key).map(Part::Legacy),
            Self::Store(window) => window.get(key).map(Part::Store),
        }
    }
}

impl Part<'_> {
    pub fn detail(&self, page: Page) -> persistence::Result<Presentation<Value>> {
        match self {
            Self::Legacy(part) => part.detail(page).map(Presentation::Legacy),
            Self::Store(part) => part.detail(page).map(Presentation::Store),
        }
    }

    pub fn apps_paging(&self, page: Page) -> persistence::Result<Presentation<Value>> {
        match self {
            Self::Legacy(part) => part.apps_paging(page).map(Presentation::Legacy),
            Self::Store(part) => part.apps_paging(page).map(Presentation::Store),
        }
    }

    pub fn is_of(&self, kind: KeyKind, bytes: &[u8]) -> bool {
        match self {
            Self::Legacy(part) => part.is_of(kind, bytes),
            Self::Store(part) => part.is_of(kind, bytes),
        }
    }
}
