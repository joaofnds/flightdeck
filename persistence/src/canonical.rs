//! Descriptor-relative canonical metadata and snapshot-bounded source lookup.
pub mod catalog;
mod frontier;
pub(crate) mod insert;
pub(crate) mod metadata;
mod node;
#[cfg(any(test, feature = "fixtures"))]
pub mod ready_fixture;
pub(crate) mod rows;
pub(crate) mod tree;
pub use frontier::{FrontierStep, SourceFrontier, SourceFrontierRead};
pub use metadata::{
    CanonicalLoss, CanonicalMetadata, EndpointEvidence, Lifecycle, RawBase, RawChild,
};
pub use tree::{
    CanonicalCursor, CanonicalKey, Direction, Namespace, NodeLocator, ReadStep, ReadWork, SeekRange,
};

#[cfg(test)]
mod tests;
