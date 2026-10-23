//! Bounded, separate-process persistence of the collector's published measurements.
pub mod app_tables;
mod battery;
pub mod disks;
mod encoder;
pub mod engine;
pub mod identity;
mod identity_encoder;
pub mod layout;
mod metadata;
mod optional_column;
mod owned_identity;
pub mod query;
mod reader;
pub mod resources;
pub mod schema;
pub mod sealed_apps;
mod store;
mod system;
mod temperatures;
mod window;
pub mod window_ticks;
pub mod worker;

pub use owned_identity::IdentitySource;

pub use metadata::{Losses, Manifest, TimeBounds};
pub use reader::{require_numeric_source, tick_buffer};
pub use store::{
    Begin, CommitBoundary, PendingWindow, Store, list_committed, ownership_contended,
    read_committed, reread_committed,
};

pub use encoder::{
    Encoder, TableManifest, WRITE_BUFFER_BYTES, encode_derived, frame, frame_bytes, table_columns,
};
pub use schema::SCHEMA_VERSION;
pub use window::{Limits, NumberedEvent, Observation, STAGED_BYTES_MAX, Window, WindowIdentity};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Bound(&'static str),
    Engine(polars::error::PolarsError),
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
impl From<polars::error::PolarsError> for Error {
    fn from(error: polars::error::PolarsError) -> Self {
        Self::Engine(error)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
