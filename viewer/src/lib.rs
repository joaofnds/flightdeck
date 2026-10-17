//! Viewer-only recording interpretation. No collector calls this allocating adapter.
pub mod apps;
mod bridge;
mod byte_display;
mod cancellation;
mod data;
mod live;
mod live_app_history;
pub mod live_app_response;
pub mod live_apps;
mod metrics;
mod session;

pub use cancellation::Cancellation;
pub use live::LiveSession;
pub use session::Session;
