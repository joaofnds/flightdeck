//! Viewer-only recording interpretation. No collector calls this allocating adapter.
pub mod app_process_groups_history;
pub mod apps;
mod byte_display;
mod cancellation;
mod cpu_detail;
mod cpu_history;
mod cpu_history_sample;
mod data;
mod live;
mod live_app_history;
pub mod live_app_response;
pub mod live_apps;
mod live_fan_history;
mod metrics;
mod session;

pub use cancellation::Cancellation;
pub use live::LiveSession;
pub use live::app_parts::WindowParts;
pub use live::app_ranks::WindowRanks;
pub(crate) use live::attribution_history as app_attribution_history;
pub use live::attribution_history::{
    APP_ATTRIBUTION_HISTORIES_RESPONSE_BYTES_MAX, AppAttributionHistories, AppAttributionMetric,
};
pub use live::window::{ObservationKey, Window};
pub use session::{SealedApps, Session};
