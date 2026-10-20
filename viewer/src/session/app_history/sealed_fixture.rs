//! The range and the rewrite the tests of a sealed window's derived tables share.
use polars::prelude::DataFrame;

use super::{ContextRange, Session};

/// The window of the context of second `second`.
pub fn range(second: u64) -> ContextRange {
    ContextRange {
        first_ns: (second - 899) * 1_000_000_000,
        end_ns: (second + 1) * 1_000_000_000,
    }
}

/// Rewrites the derived table `name` of each sealed window that holds it in place through
/// `change`.
pub fn rewrite(session: &Session, name: &'static str, mut change: impl FnMut(&mut DataFrame)) {
    for manifest in &session.windows {
        if manifest.tables.iter().any(|table| table.name == name) {
            let path = session
                .root
                .join(&manifest.data_directory)
                .join(format!("{name}.parquet"));
            let mut frame = persistence::engine::scan(&path).unwrap().collect().unwrap();
            change(&mut frame);
            let staging = session.root.with_extension("rewrite");
            std::fs::create_dir(&staging).unwrap();

            persistence::encode_derived(name, &mut frame, &staging).unwrap();

            // Writing into the sealed file keeps the private mode a session checks on open.
            let encoded = std::fs::read(staging.join(format!("{name}.parquet"))).unwrap();
            std::fs::write(&path, encoded).unwrap();
            std::fs::remove_dir_all(&staging).unwrap();
        }
    }
}
