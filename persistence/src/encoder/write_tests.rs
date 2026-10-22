use std::io::{self, Cursor, Write};

use polars::prelude::{Column, DataFrame};
use polars_parquet::parquet::read::read_metadata;
use polars_parquet::read::RowGroupMetadata;

use super::{ROW_GROUP_ROWS, WRITE_BUFFER_BYTES, encode_write};

/// A file that counts the writes it is handed and the bytes in them.
struct CountingFile {
    writes: u64,
    bytes: u64,
}

impl Write for CountingFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        self.bytes += collector::u64_from_usize(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A file whose every write fails, as a full disk's does.
struct FullFile;

impl Write for FullFile {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::StorageFull))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Three row groups of three columns, which parquet writes as many small headers and pages.
fn three_row_groups() -> DataFrame {
    let rows: Vec<u64> = (0..40_000).collect();
    let columns = ["publication", "pid", "user_ns"].map(|name| {
        Column::new(
            name.into(),
            rows.iter().map(|row| row * 7).collect::<Vec<_>>(),
        )
    });

    DataFrame::new(rows.len(), columns.to_vec()).unwrap()
}

#[test]
fn hands_a_table_smaller_than_its_buffer_to_the_file_in_one_write() {
    // Goal: a table reaches the file in few large writes, since each write is a system call
    // whose cost grows with the machine's load. Method: write a table of three row groups into
    // a file that counts its writes.
    let mut frame = three_row_groups();
    let mut file = CountingFile {
        writes: 0,
        bytes: 0,
    };

    encode_write(&mut frame, &mut file).unwrap();

    assert!(file.bytes < WRITE_BUFFER_BYTES);
    assert_eq!(1, file.writes);
}

#[test]
fn writes_a_row_group_for_each_slice_of_its_rows() {
    // Goal: a table's rows split into row groups of a fixed count, the last one shorter, so a
    // reader skips the groups a query rules out. Method: write a table of 40,000 rows and read
    // the row counts of its row groups from the file's metadata.
    let mut frame = three_row_groups();
    let mut file = Cursor::new(Vec::new());

    encode_write(&mut frame, &mut file).unwrap();

    let metadata = read_metadata(&mut file).unwrap();
    let groups: Vec<usize> = metadata
        .row_groups
        .iter()
        .map(RowGroupMetadata::num_rows)
        .collect();
    assert_eq!(
        groups,
        [ROW_GROUP_ROWS, ROW_GROUP_ROWS, 40_000 - 2 * ROW_GROUP_ROWS]
    );
}

#[test]
fn reports_a_write_the_file_refuses() {
    // Goal: a write the file refuses fails the table, even one the buffer only makes at its
    // end. Method: write a table smaller than the buffer into a file that refuses every write.
    let mut frame = three_row_groups();

    let written = encode_write(&mut frame, FullFile);

    assert!(written.is_err());
}

#[test]
fn writes_every_column_without_a_dictionary() {
    // Goal: a table's columns are written plain, since building a dictionary for each column
    // cost a fifth of a window's seal CPU and saved no bytes once compressed. Method: write
    // columns of one repeated value, which a dictionary writer encodes as a dictionary, and
    // look for a dictionary page in the file's metadata.
    let mut frame = DataFrame::new(
        3,
        vec![
            Column::new("pid".into(), [7i32, 7, 7]),
            Column::new("executable_name".into(), [b"a".as_slice(), b"a", b"a"]),
        ],
    )
    .unwrap();
    let mut file = Cursor::new(Vec::new());

    encode_write(&mut frame, &mut file).unwrap();

    let metadata = read_metadata(&mut file).unwrap();
    let columns: Vec<_> = metadata
        .row_groups
        .iter()
        .flat_map(RowGroupMetadata::parquet_columns)
        .collect();
    assert_eq!(columns.len(), 2);
    assert!(
        columns
            .iter()
            .all(|column| column.dictionary_page_offset().is_none())
    );
}
