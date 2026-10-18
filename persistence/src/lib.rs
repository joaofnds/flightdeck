//! Bounded, separate-process persistence of the collector's published measurements.
mod battery;
mod encoder;
mod identity;
mod identity_encoder;
pub mod layout;
mod metadata;
mod owned_identity;
pub mod query;
mod reader;
pub mod resources;
pub mod schema;
mod store;
mod window;
pub mod worker;

pub use owned_identity::IdentitySource;

pub use metadata::{Losses, Manifest, TimeBounds};
pub use reader::{require_numeric_source, tick_buffer};
pub use store::{Begin, CommitBoundary, PendingWindow, Store, ownership_contended, read_committed};

pub use encoder::{Encoder, TableManifest, append, table_columns};
pub use schema::SCHEMA_VERSION;
pub use window::{Limits, NumberedEvent, Observation, Window, WindowIdentity};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Bound(&'static str),
    Engine(duckdb::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bound(message) => write!(formatter, "persistence bound: {message}"),
            Self::Engine(error) => write!(formatter, "persistence engine: {error}"),
            Self::Io(error) => write!(formatter, "persistence I/O: {error}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<duckdb::Error> for Error {
    fn from(error: duckdb::Error) -> Self {
        Self::Engine(error)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
