//! A decoded presentation keeps the byte reservation that admitted its source response.
use std::ops::Deref;
use std::sync::Arc;

use serde::{Serialize, Serializer};
use viewer::store::{ContextRanks, Response};

pub enum Presentation<T> {
    Legacy(T),
    Shared(Arc<T>),
    Store(Response<T>),
    Canonical(ContextRanks<T>),
}

impl<T> Presentation<T> {
    /// Keeps admission with a decoded view of the response, including compatibility fallbacks.
    pub fn map_ref<U>(self, map: impl FnOnce(&T) -> U) -> Presentation<U> {
        match self {
            Self::Legacy(value) => Presentation::Legacy(map(&value)),
            Self::Shared(value) => Presentation::Shared(Arc::new(map(&value))),
            Self::Store(value) => Presentation::Store(value.map(|value| map(&value))),
            Self::Canonical(value) => Presentation::Canonical(value.map(|value| map(&value))),
        }
    }

    /// Fixture mutations are allowed only for values created without a Store owner.
    #[cfg(test)]
    pub fn legacy_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Legacy(value) => Some(value),
            Self::Shared(_) | Self::Store(_) | Self::Canonical(_) => None,
        }
    }
}

impl<T> Deref for Presentation<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Legacy(value) => value,
            Self::Shared(value) => value,
            Self::Store(value) => value,
            Self::Canonical(value) => value,
        }
    }
}

impl<T: Serialize> Serialize for Presentation<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Presentation<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(formatter)
    }
}

impl<T: PartialEq> PartialEq for Presentation<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl PartialEq<serde_json::Value> for Presentation<serde_json::Value> {
    fn eq(&self, other: &serde_json::Value) -> bool {
        &**self == other
    }
}
