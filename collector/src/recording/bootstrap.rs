//! Registered protocol-two foundation. Production format selection remains in `format`.
pub mod codec;
#[cfg(test)]
mod tests;

mod observation;
mod root;
pub use observation::StateObservation;
pub use root::{Classification, classify, classify_settled, initialize};

#[cfg(test)]
mod classification_tests;
