//! The range and the rewrite the tests of a sealed window's derived tables share.
use polars::prelude::{Column, DataFrame, DataType};

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

    /// Rewrites the parts each sealed window keeps to version 2, the last that kept no GPU
    /// time, so a build that reads it as its own fails on the columns it lacks.
    pub(crate) fn previous_version_kept_parts(&self) {
        const { assert!(VERSION > 2) };
        rewrite(self, APP_PARTS, |frame| {
            let version = vec![2_u8; frame.height()];
            frame
                .replace("version", Column::new("version".into(), version))
                .unwrap();
            let gpu: Vec<String> = frame
                .get_column_names()
                .into_iter()
                .filter(|name| name.contains("gpu_ns"))
                .map(ToString::to_string)
                .collect();
            assert!(!gpu.is_empty());
            for name in gpu {
                frame.drop_in_place(&name).unwrap();
            }
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

    /// Rewrites the parts each sealed window keeps so every cell of column `name` is null, in
    /// the type the column holds.
    pub(crate) fn null_kept_column(&self, name: &str) {
        rewrite(self, APP_PARTS, |frame| {
            let column = frame.column(name).unwrap();
            let nulls = Column::full_null(name.into(), frame.height(), column.dtype());
            frame.replace(name, nulls).unwrap();
        });
    }

    /// Rewrites the parts each sealed window keeps so column `name` holds its values as 64-bit
    /// integers, a type no build writes it in.
    pub(crate) fn widen_kept_column(&self, name: &str) {
        rewrite(self, APP_PARTS, |frame| {
            let widened = frame.column(name).unwrap().cast(&DataType::UInt64).unwrap();
            frame.replace(name, widened).unwrap();
        });
    }

    /// Rewrites the parts each sealed window keeps so each row's App key ends in zero bytes up
    /// to `length` bytes, which keeps the keys apart and in their order.
    pub(crate) fn pad_kept_app_keys(&self, length: usize) {
        rewrite(self, APP_PARTS, |frame| {
            let padded: Vec<Option<Vec<u8>>> = frame
                .column("app_key")
                .unwrap()
                .binary()
                .unwrap()
                .iter()
                .map(|key| {
                    key.map(|key| {
                        assert!(key.len() <= length);
                        let mut padded = key.to_vec();
                        padded.resize(length, 0);
                        padded
                    })
                })
                .collect();
            let padded: Vec<Option<&[u8]>> = padded.iter().map(Option::as_deref).collect();
            frame
                .replace("app_key", Column::new("app_key".into(), padded))
                .unwrap();
        });
    }

    /// Rewrites the parts each sealed window keeps so each App row names the place after its
    /// own among its observation's Apps.
    pub(crate) fn misorder_kept_apps(&self) {
        rewrite(self, APP_PARTS, |frame| {
            let shifted: Vec<Option<u32>> = frame
                .column("app_index")
                .unwrap()
                .u32()
                .unwrap()
                .iter()
                .map(|index| index.map(|index| index + 1))
                .collect();
            frame
                .replace("app_index", Column::new("app_index".into(), shifted))
                .unwrap();
        });
    }

    /// Rewrites the parts each sealed window keeps so each App row says the opposite of what
    /// it said of whether the App's membership is complete.
    pub(crate) fn invert_kept_memberships(&self) {
        rewrite(self, APP_PARTS, |frame| {
            let inverted: Vec<Option<bool>> = frame
                .column("app_membership_complete")
                .unwrap()
                .bool()
                .unwrap()
                .iter()
                .map(|complete| complete.map(|complete| !complete))
                .collect();
            let column = Column::new("app_membership_complete".into(), inverted);
            frame.replace("app_membership_complete", column).unwrap();
        });
    }
}
