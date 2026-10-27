use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::{FILE_BYTES_MAX, LINE_BYTES_MAX, LINES_MAX, Output, TAIL_BYTES_MAX};
use crate::self_reports::{OutputStream, SelfReports};

const TICK: &str = concat!(
    "ticks 51-60: collector CPU 1% of a core, 1 mW; ",
    "ticks late 1 ms on average, 1 ms at most, 0 slips\n",
    "  ms of CPU a second: processes 1\n",
    "  last tick read 1 processes (0 refused, 0 gone), 1 coalitions; ",
    "0 starts, 0 execs and 0 exits dropped\n",
    "minute, ticks 1-60: collector CPU 1% of a core\n",
    "recording minute elapsed_ns=60 collector_cpu_ns=1 persister_cpu_ns=1 combined_percent=1\n",
);
const CUMULATIVE: &str = "recording cumulative current_footprint_bytes=2 \
    lifetime_footprint_bytes_max=3\n";

#[test]
fn skipped_bytes_cannot_complete_an_older_pending_minute() {
    // Goal: a later cumulative line cannot finish a minute whose ending was lost.
    // Method: leave a real report pending, overflow the file tail, then deliver a later ending.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    let mut reports = SelfReports::default();
    writer.write_all(TICK.as_bytes()).unwrap();
    drain_reports(&mut output, &mut reports, OutputStream::Stdout, 1_060);
    writer.write_all(&vec![b'x'; TAIL_BYTES_MAX + 1]).unwrap();
    writer.write_all(b"\n").unwrap();

    drain_reports(&mut output, &mut reports, OutputStream::Stdout, 1_120);
    writer.write_all(CUMULATIVE.as_bytes()).unwrap();
    drain_reports(&mut output, &mut reports, OutputStream::Stdout, 1_121);

    assert_eq!(reports.ticks.len(), 1);
    assert!(reports.minutes.is_empty());
    assert!(reports.incomplete);
}

#[test]
fn a_full_group_after_a_damaged_drain_recovers() {
    // Goal: report parsing resumes after a discontinuity without adopting an unfinished line.
    // Method: reject invalid UTF-8 and a partial line, then append its end and a complete group.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    let mut reports = SelfReports::default();
    writer.write_all(b"\xff\nunfinished").unwrap();
    drain_reports(&mut output, &mut reports, OutputStream::Stdout, 1_060);

    writer.write_all(b" ending\n").unwrap();
    writer.write_all(TICK.as_bytes()).unwrap();
    writer.write_all(CUMULATIVE.as_bytes()).unwrap();
    drain_reports(&mut output, &mut reports, OutputStream::Stdout, 1_120);

    assert_eq!(reports.ticks.len(), 1);
    assert_eq!(reports.minutes.len(), 1);
    assert_eq!(reports.minutes[0].second, 1_120);
    assert_eq!(reports.minutes[0].footprint_bytes, Some(2));
    assert!(reports.incomplete);
}

#[test]
fn stderr_loss_leaves_the_stdout_minute_pending() {
    // Goal: losing persister diagnostics cannot damage an intact collector stdout report.
    // Method: hold a stdout minute pending while a separate real stderr file loses a line.
    let mut standard = Output::new().unwrap();
    let mut diagnostic = Output::new().unwrap();
    let mut writer = standard.writer().unwrap();
    let mut errors = diagnostic.writer().unwrap();
    let mut reports = SelfReports::default();
    writer.write_all(TICK.as_bytes()).unwrap();
    drain_reports(&mut standard, &mut reports, OutputStream::Stdout, 1_060);

    errors.write_all(&vec![b'x'; LINE_BYTES_MAX + 1]).unwrap();
    errors.write_all(b"\n").unwrap();
    drain_reports(&mut diagnostic, &mut reports, OutputStream::Stderr, 1_120);
    writer.write_all(CUMULATIVE.as_bytes()).unwrap();
    drain_reports(&mut standard, &mut reports, OutputStream::Stdout, 1_121);

    assert_eq!(reports.minutes.len(), 1);
    assert_eq!(reports.minutes[0].second, 1_060);
    assert_eq!(reports.minutes[0].footprint_bytes, Some(2));
    assert!(reports.incomplete);
}

#[test]
fn oversized_lines_discard_the_batch_and_its_unfinished_line() {
    // Goal: overflow does not expose surviving lines or a truncated unfinished report.
    // Method: exceed only the line limit, then end its partial successor in another drain.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    writer.write_all(b"first\n").unwrap();
    writer.write_all(&vec![b'x'; LINE_BYTES_MAX + 1]).unwrap();
    writer.write_all(b"\nlast\nunfinished").unwrap();

    output.drain().unwrap();
    assert!(output.take_lines().is_none());
    writer.write_all(b" ending\nwhole\n").unwrap();
    output.drain().unwrap();

    assert_eq!(output.take_lines().unwrap(), ["whole"]);
}

#[test]
fn too_many_buffered_lines_discard_the_batch() {
    // Goal: bounded line retention reports a discontinuity instead of keeping a suffix.
    // Method: fill the line queue without exceeding the byte-tail or individual-line limit.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    writer.write_all(&b"x\n".repeat(LINES_MAX + 1)).unwrap();

    output.drain().unwrap();

    assert!(output.take_lines().is_none());
}

#[test]
fn file_truncation_discards_the_affected_drain() {
    // Goal: a truncated descriptor cannot join its replacement bytes to pending output.
    // Method: drain a partial line, truncate the same file, then append a short replacement.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    writer.write_all(b"an unfinished earlier report").unwrap();
    output.drain().unwrap();
    assert!(output.take_lines().unwrap().is_empty());

    writer.set_len(0).unwrap();
    writer.write_all(b"cut\nwhole\n").unwrap();
    output.drain().unwrap();

    assert!(output.take_lines().is_none());
}

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

#[test]
fn whole_lines_are_taken_once_and_a_partial_line_waits_for_its_end() {
    // Goal: a reader of the collector's reports sees each finished line exactly once.
    // Method: write two lines and half a third, take the lines, finish the third, take again.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    writer.write_all(b"first\nsecond\nthi").unwrap();
    output.drain().unwrap();

    let before = output.take_lines().unwrap();
    writer.write_all(b"rd\n").unwrap();
    output.drain().unwrap();
    let after = output.take_lines().unwrap();

    assert_eq!(before, ["first", "second"]);
    assert_eq!(after, ["third"]);
}

#[test]
fn a_line_cut_by_skipping_invalidates_the_entire_drain() {
    // Goal: a skipped prefix cannot leave a suffix that finishes an earlier report.
    // Method: write beyond the tail bound, then a whole line, and inspect the batch.
    let mut output = Output::new().unwrap();
    let mut writer = output.writer().unwrap();
    writer.write_all(&vec![b'x'; TAIL_BYTES_MAX + 1]).unwrap();
    writer.write_all(b"\nwhole\n").unwrap();

    output.drain().unwrap();

    assert!(output.take_lines().is_none());
}

fn drain_reports(
    output: &mut Output,
    reports: &mut SelfReports,
    stream: OutputStream,
    second: u64,
) {
    output.drain().unwrap();
    reports.output(output.take_lines(), stream, second);
}
