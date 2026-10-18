use std::path::Path;

use polars::io::HiveOptions;
use polars::prelude::{LazyFrame, ParallelStrategy, PlRefPath, ScanArgsParquet};

use crate::{Error, Result};

pub fn scan(path: &Path) -> Result<LazyFrame> {
    super::admission::validate(path)?;
    let path = path.to_str().ok_or(Error::Bound("query path encoding"))?;
    let options = ScanArgsParquet {
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
    };
    Ok(LazyFrame::scan_parquet(PlRefPath::new(path), options)?)
}
