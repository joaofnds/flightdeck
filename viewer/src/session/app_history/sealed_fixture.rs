//! The range and the rewrite the tests of a sealed window's derived tables share.
use polars::prelude::{Column, DataFrame};

use super::super::sealed_parts::VERSION;
use super::{ContextRange, Session};
use persistence::schema::APP_PARTS;

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

impl Session {
    /// Rewrites the parts each sealed window keeps to a version this build does not read, so a
    /// session opened afterwards replays their minutes.
    pub(crate) fn unread_kept_parts(&self) {
        rewrite(self, APP_PARTS, |frame| {
            let version = vec![VERSION + 1; frame.height()];
            frame
                .replace("version", Column::new("version".into(), version))
                .unwrap();
        });
    }

    /// Rewrites the parts each sealed window keeps to the version before this build's, which
    /// kept no count of the live processes outside every App.
    pub(crate) fn previous_version_kept_parts(&self) {
        rewrite(self, APP_PARTS, |frame| {
            let version = vec![VERSION - 1; frame.height()];
            frame
                .replace("version", Column::new("version".into(), version))
                .unwrap();
            frame.drop_in_place("live_ungrouped_members").unwrap();
        });
    }

    /// Rewrites the parts each sealed window keeps so the facts of every observation count one
    /// App more than the window keeps for it.
    pub(crate) fn overcount_kept_apps(&self) {
        rewrite(self, APP_PARTS, |frame| {
            let raised: Vec<Option<u64>> = frame
                .column("app_count")
                .unwrap()
                .u64()
                .unwrap()
                .iter()
                .map(|count| count.map(|count| count + 1))
                .collect();
            frame
                .replace("app_count", Column::new("app_count".into(), raised))
                .unwrap();
        });
    }
}
