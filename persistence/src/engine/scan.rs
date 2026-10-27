use std::path::Path;

use polars::io::HiveOptions;
use polars::prelude::{LazyFrame, ParallelStrategy, PlRefPath, ScanArgsParquet, ScanSources};
use polars_buffer::Buffer;
use std::sync::Arc;

use crate::{Error, Result};

pub fn scan(path: &Path) -> Result<LazyFrame> {
    crate::work::ingestion::record_content(crate::work::ingestion::Kind::NativeScan, 0);
    super::admission::validate(path)?;
    let path = path.to_str().ok_or(Error::Bound("query path encoding"))?;
    Ok(LazyFrame::scan_parquet(PlRefPath::new(path), options())?)
}

#[repr(transparent)]
struct InputOwner(super::AdmittedFile);

impl AsRef<[u8]> for InputOwner {
    fn as_ref(&self) -> &[u8] {
        &self.0.bytes
    }
}

pub(super) fn buffer_sources(input: super::AdmittedFile) -> ScanSources {
    // These three native wrappers are outside the compressed-byte reservation.
    // The foreign owner keeps the original Vec before its reservation in drop order.
    let buffer = Buffer::from_owner(InputOwner(input));

    ScanSources::Buffers(Arc::from([buffer]))
}

pub(super) fn scan_sources(sources: ScanSources) -> Result<LazyFrame> {
    crate::work::ingestion::record_content(crate::work::ingestion::Kind::BufferedScan, 0);
    Ok(LazyFrame::scan_parquet_sources(sources, options())?)
}

fn options() -> ScanArgsParquet {
    ScanArgsParquet {
        n_rows: None,
        parallel: ParallelStrategy::None,
        row_index: None,
        cloud_options: None,
        hive_options: HiveOptions {
            enabled: Some(false),
            hive_start_idx: 0,
            schema: None,
            try_parse_dates: false,
        },
        use_statistics: true,
        schema: None,
        low_memory: true,
        rechunk: false,
        cache: false,
        glob: false,
        include_file_paths: None,
        allow_missing_columns: false,
    }
}
