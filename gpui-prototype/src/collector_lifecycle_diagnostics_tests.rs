use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::{FILE_BYTES_MAX, Output, TAIL_BYTES_MAX};

#[test]
fn writer_survives_parent_reader_departure_without_a_path() {
    // Goal: helper output stays writable after the app releases its diagnostic reader.
    // Method: retain the real writer descriptor, drop the reader, and write to its unlinked inode.
    let output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    let metadata = writer.metadata().unwrap();
    assert_eq!(0, metadata.nlink());
    assert_eq!(0o600, metadata.permissions().mode() & 0o777);
    drop(output);

    writer
        .write_all(b"helper finished after parent departure\n")
        .unwrap();
    assert_eq!(39, writer.metadata().unwrap().len());
}

#[test]
fn positional_reads_preserve_append_writes_and_independent_channels() {
    // Goal: reading diagnostics never moves a helper's output position or joins channels.
    // Method: alternate real file writes and reads while a second channel remains empty.
    let mut standard = Output::new().unwrap();
    let mut diagnostic = Output::new().unwrap();
    let mut writer = standard.writer().unwrap();
    writer.write_all(b"first").unwrap();
    standard.drain().unwrap();
    writer.write_all(b" second").unwrap();
    standard.drain().unwrap();
    diagnostic.drain().unwrap();

    assert_eq!("first second", standard.text());
    assert_eq!("", diagnostic.text());
}

#[test]
fn oversized_output_retains_tail_and_append_resumes_after_truncation() {
    // Goal: diagnostic memory and step-based disk retention have explicit limits.
    // Method: exceed the file limit, drain its last bytes, then append after truncation.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    let size = usize::try_from(FILE_BYTES_MAX + 1).unwrap();
    writer.write_all(&vec![b'x'; size]).unwrap();
    output.drain().unwrap();
    assert_eq!(FILE_BYTES_MAX + 1, output.peak_bytes());
    assert_eq!(0, writer.metadata().unwrap().len());

    writer.write_all(b"done").unwrap();
    output.drain().unwrap();
    assert_eq!(4, writer.metadata().unwrap().len());
    let text = output.text();
    assert_eq!(TAIL_BYTES_MAX, text.len());
    assert!(text.ends_with("done"));
    assert!(text.starts_with('x'));
}
