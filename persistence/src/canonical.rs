//! Descriptor-relative canonical metadata and snapshot-bounded source lookup.
mod frontier;
mod metadata;
mod rows;
mod tree;
pub use frontier::{FrontierStep, SourceFrontier, SourceFrontierRead};
pub use metadata::{
    CanonicalLoss, CanonicalMetadata, EndpointEvidence, Lifecycle, RawBase, RawChild,
};
pub use tree::{
    CanonicalCursor, CanonicalKey, Direction, Namespace, NodeLocator, ReadStep, ReadWork, SeekRange,
};

#[cfg(test)]
mod tests;
