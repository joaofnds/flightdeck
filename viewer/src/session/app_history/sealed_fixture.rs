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

/// Rewrites the only sealed window's derived table `name` in place through `change`.
pub fn rewrite(session: &Session, name: &'static str, change: impl FnOnce(&mut DataFrame)) {
    let path = session
        .root
        .join(&session.windows[0].data_directory)
        .join(format!("{name}.parquet"));
    let mut frame = persistence::engine::scan(&path).unwrap().collect().unwrap();
    change(&mut frame);
    let staging = session.root.with_extension("rewrite");
    std::fs::create_dir(&staging).unwrap();

    persistence::encode_derived(name, &mut frame, &staging).unwrap();

    std::fs::copy(staging.join(format!("{name}.parquet")), &path).unwrap();
    std::fs::remove_dir_all(&staging).unwrap();
}
