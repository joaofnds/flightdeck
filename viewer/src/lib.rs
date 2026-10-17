//! Viewer-only recording interpretation. No collector calls this allocating adapter.
mod bridge;
mod cancellation;
mod data;
mod live;
mod metrics;
mod session;

pub use cancellation::Cancellation;
pub use live::LiveSession;
pub use session::Session;
