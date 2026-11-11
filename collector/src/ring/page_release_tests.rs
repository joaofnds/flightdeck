use super::timing_tests::Source;
use super::*;

#[test]
fn a_reader_that_gave_back_its_pages_reads_as_it_did_before() {
    // Goal: giving back the ring's pages changes nothing a reader sees, so a reader may do it
    // after any read.
    // Method: read a publication, give the pages back, then read that publication again and
    // one the writer published after.
    let mut source = Source::new(6);
    source.publish(73, None);
    let mut reader = RingReader::open(&source.place.lock_path).unwrap();
    let mut tick = Tick::new(&source.writer.layout);
    let first = reader.read_into(&mut tick).unwrap();

    reader.release_pages().unwrap();

    source.publish(74, None);
    reader.seek_publication(1).unwrap();
    assert_eq!(reader.read_into(&mut tick).unwrap(), first);
    assert_eq!(tick.run_tick, 73);
    let second = Found::Publication {
        publication: 2,
        tick: 74,
    };
    assert_eq!(reader.read_into(&mut tick).unwrap(), second);
    assert_eq!(tick.run_tick, 74);
}
