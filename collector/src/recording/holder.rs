//! The collector holding a recording. Its lifetime lock says whether one holds it, and its
//! announcement says which run it writes and what code built it.

use std::io::{self, Read};
use std::path::Path;

use super::{BUILD, identity_text, parse_identity, private};
use crate::ring::Identity;

const ANNOUNCEMENT: &str = "collector.announcement";
/// The start every version of the header shares, so a reader tells a format it cannot read
/// from a damaged file.
const HEADER_FAMILY: &str = "flightdeck-collector-";
const HEADER: &str = "flightdeck-collector-1";
const RUN_DIGITS: usize = 32;
const BUILD_DIGITS: usize = 16;
const RUN_START: usize = HEADER.len() + 1;
const BUILD_START: usize = RUN_START + RUN_DIGITS + 1;
/// The header, the run's identity and the build, each on a line of its own.
const ANNOUNCEMENT_BYTES: usize = BUILD_START + BUILD_DIGITS + 1;

const _: () = assert!(HEADER.len() > HEADER_FAMILY.len());
const _: () = assert!(RUN_DIGITS == 2 * size_of::<Identity>());
const _: () = assert!(BUILD_DIGITS == 2 * size_of::<u64>());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    Absent,
    /// A collector holds the recording but names no run, since it is starting, could not
    /// register its run, or predates announcements.
    Unannounced,
    Announced {
        run: Identity,
        build: u64,
    },
    /// A collector holds the recording and announces it in a format another version of
    /// Flightdeck writes, so it is another build.
    Unrecognized,
}

/// Reads who holds the recording at `root` without taking its lock.
pub fn holder(root: &Path) -> io::Result<Holder> {
    let mut lock_path = [0; private::PATH_MAX];
    let lock = private::join_path(root, "collector.lock", &mut lock_path)?;
    if !private::FileLock::held(lock)? {
        return Ok(Holder::Absent);
    }

    let mut path_buffer = [0; private::PATH_MAX];
    let path = private::join_path(root, ANNOUNCEMENT, &mut path_buffer)?;
    let mut file = match private::open_private_file(path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Holder::Unannounced);
        }
        Err(error) => return Err(error),
    };
    let mut bytes = [0; ANNOUNCEMENT_BYTES + 1];
    let mut length = 0;
    while length < bytes.len() {
        let count = file.read(&mut bytes[length..])?;
        if count == 0 {
            break;
        }
        length += count;
    }
    let bytes = &bytes[..length];

    if !bytes.starts_with(HEADER_FAMILY.as_bytes()) {
        return Err(invalid());
    }

    if !bytes.starts_with(HEADER.as_bytes()) {
        return Ok(Holder::Unrecognized);
    }

    // A later header that begins with this one, such as version 10, is still another format.
    if bytes.get(HEADER.len()) != Some(&b'\n') {
        return Ok(Holder::Unrecognized);
    }

    if length != ANNOUNCEMENT_BYTES {
        return Err(invalid());
    }

    holder_parse(bytes)
}

/// Parses an announcement of this format, whose every field lies at a fixed offset.
fn holder_parse(bytes: &[u8]) -> io::Result<Holder> {
    assert_eq!(ANNOUNCEMENT_BYTES, bytes.len());
    if bytes[BUILD_START - 1] != b'\n' {
        return Err(invalid());
    }

    if bytes[ANNOUNCEMENT_BYTES - 1] != b'\n' {
        return Err(invalid());
    }

    let run = std::str::from_utf8(&bytes[RUN_START..BUILD_START - 1]).map_err(|_| invalid())?;
    let run = parse_identity(run)?;
    let build = &bytes[BUILD_START..ANNOUNCEMENT_BYTES - 1];
    if !build.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid());
    }

    let build = std::str::from_utf8(build).map_err(|_| invalid())?;
    let build = u64::from_str_radix(build, 16).map_err(|_| invalid())?;
    Ok(Holder::Announced { run, build })
}

/// Removes the announcement a previous holder left, once the caller holds the lock.
pub(super) fn withdraw(root: &Path) -> io::Result<()> {
    match std::fs::remove_file(root.join(ANNOUNCEMENT)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Announces the registered `run` of the collector that holds the lock.
pub(super) fn announce(root: &Path, run: Identity) -> io::Result<()> {
    let text = format!(
        "{HEADER}\n{}\n{BUILD:0digits$x}\n",
        identity_text(run),
        digits = BUILD_DIGITS
    );
    assert_eq!(ANNOUNCEMENT_BYTES, text.len());
    private::atomic_write(root, ANNOUNCEMENT, text.as_bytes())
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;
    use crate::recording::{Registration, private};
    use crate::ring::{Place, Shape, Writer, unlink_generation};

    /// A run identity and a build in this format, which tests corrupt one field at a time.
    const RUN: &str = "00000000000000010000000000000002";
    const BUILD_TEXT: &str = "0000000000000003";

    #[test]
    fn a_started_recording_announces_its_run_and_build() {
        // Goal: a reader learns which run the holding collector writes and what code built it.
        // Method: start a real registered recording and read its holder.
        let fixture = Fixture::new("started");
        let writer = fixture.start();
        let run = writer.identity().unwrap().run;

        let holder = holder(&fixture.root).unwrap();

        assert_eq!(Holder::Announced { run, build: BUILD }, holder);
    }

    #[test]
    fn a_recording_whose_collector_exited_has_no_holder() {
        // Goal: a pid macOS gave another process cannot pass for the exited collector.
        // Method: stop a registered collector, whose registry entry and announcement stay.
        let fixture = Fixture::new("exited");
        drop(fixture.start());

        let holder = holder(&fixture.root).unwrap();

        assert_eq!(Holder::Absent, holder);
    }

    #[test]
    fn a_collector_holding_the_recording_without_announcing_is_unannounced() {
        // Goal: a collector that is starting, or predates announcing, is told apart from none.
        // Method: hold the recording's lifetime lock with no announcement written.
        let fixture = Fixture::new("unannounced");
        let lock = private::FileLock::try_lock(&fixture.root.join("collector.lock")).unwrap();

        let holder = holder(&fixture.root).unwrap();

        assert_eq!(Holder::Unannounced, holder);
        drop(lock);
    }

    #[test]
    fn a_collector_that_cannot_register_withdraws_the_last_announcement() {
        // Goal: a new holder is never read as the run its predecessor announced.
        // Method: stop one collector, then start another whose registration fails after it
        // takes the recording, since the recording's identity no longer reads.
        let fixture = Fixture::new("withdrawn");
        drop(fixture.start());
        private::atomic_write(&fixture.root, "identity", b"damaged\n").unwrap();

        let (writer, registered) = fixture.started();

        assert!(registered.is_err());
        assert_eq!(Holder::Unannounced, holder(&fixture.root).unwrap());
        drop(writer);
    }

    #[test]
    fn reads_the_holder_without_allocating() {
        // Goal: a reader can poll the holder from fixed storage.
        // Method: count allocations while reading a registered recording's holder.
        let fixture = Fixture::new("allocations");
        let _writer = fixture.start();

        let calls = allocator_calls_in(|| {
            assert!(matches!(
                holder(&fixture.root).unwrap(),
                Holder::Announced { .. }
            ));
        });

        assert_eq!(0, calls);
    }

    #[test]
    fn reads_an_announcement_of_this_format() {
        // Goal: the fields a collector writes come back as it wrote them.
        // Method: hold the lock beside an announcement written by hand in this format.
        let fixture = Fixture::new("format");
        let _lock = fixture.announced(&format!("{HEADER}\n{RUN}\n{BUILD_TEXT}\n"));

        let holder = holder(&fixture.root).unwrap();

        let run = parse_identity(RUN).unwrap();
        assert_eq!(Holder::Announced { run, build: 3 }, holder);
    }

    #[test]
    fn an_announcement_in_another_format_names_another_build() {
        // Goal: a later format reads as another build, never as a damaged file.
        // Method: announce under the next header version, with lines this format lacks.
        let fixture = Fixture::new("unrecognized");
        let _lock = fixture.announced(&format!("{HEADER_FAMILY}2\n{RUN}\nmore\nlines\n"));

        let holder = holder(&fixture.root).unwrap();

        assert_eq!(Holder::Unrecognized, holder);
    }

    #[test]
    fn refuses_a_build_that_is_not_hexadecimal() {
        // Goal: an announcement no collector wrote is never read as a build.
        let fixture = Fixture::new("not-hex");
        let _lock = fixture.announced(&format!("{HEADER}\n{RUN}\n000000000000000x\n"));

        assert_eq!(
            io::ErrorKind::InvalidData,
            holder(&fixture.root).unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_a_signed_build() {
        // Goal: only the digits a collector writes are a build, though Rust's parse takes a
        // sign too.
        let fixture = Fixture::new("signed");
        let _lock = fixture.announced(&format!("{HEADER}\n{RUN}\n+00000000000000f\n"));

        assert_eq!(
            io::ErrorKind::InvalidData,
            holder(&fixture.root).unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_an_announcement_with_another_line() {
        // Goal: a file longer than this format is not read as its first three lines.
        let fixture = Fixture::new("long");
        let _lock = fixture.announced(&format!("{HEADER}\n{RUN}\n{BUILD_TEXT}\nmore\n"));

        assert_eq!(
            io::ErrorKind::InvalidData,
            holder(&fixture.root).unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_an_announcement_with_a_carriage_return() {
        // Goal: only the line ending a collector writes separates the fields.
        // Method: end the run with a carriage return and drop the last newline, keeping the
        // length of this format.
        let fixture = Fixture::new("carriage");
        let _lock = fixture.announced(&format!("{HEADER}\n{RUN}\r\n{BUILD_TEXT}"));

        assert_eq!(
            io::ErrorKind::InvalidData,
            holder(&fixture.root).unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_a_file_that_is_no_announcement() {
        // Goal: a damaged file is reported, never read as another build.
        let fixture = Fixture::new("damaged");
        let _lock = fixture.announced("damaged\n");

        assert_eq!(
            io::ErrorKind::InvalidData,
            holder(&fixture.root).unwrap_err().kind()
        );
    }

    struct Fixture {
        root: std::path::PathBuf,
        place: Place,
        rings: RefCell<Vec<String>>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("flightdeck-holder-{name}-{}", std::process::id()));
            match std::fs::remove_dir_all(&root) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("Fixture cleanup failed: {error}"),
            }
            private::create_private_dir(&root).unwrap();
            let place = Place {
                lock_path: root.join("ring.lock"),
                name_prefix: "/fdh.".into(),
            };
            Self {
                root,
                place,
                rings: RefCell::new(Vec::new()),
            }
        }

        fn started(&self) -> (Writer, io::Result<()>) {
            let shape = Shape {
                slot_count: 2,
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 2,
            };
            let started = Registration::start(&self.root, &self.place, shape, 0).unwrap();
            let (writer, registered) = started.into_parts();
            self.rings.borrow_mut().push(writer.name().to_owned());
            (writer, registered)
        }

        fn start(&self) -> Writer {
            let (writer, registered) = self.started();
            registered.unwrap();
            writer
        }

        /// Holds the recording's lock beside an announcement of exactly `text`.
        fn announced(&self, text: &str) -> private::FileLock {
            let lock = private::FileLock::try_lock(&self.root.join("collector.lock")).unwrap();
            private::atomic_write(&self.root, ANNOUNCEMENT, text.as_bytes()).unwrap();
            lock
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for ring in self.rings.borrow().iter() {
                match unlink_generation(ring) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => panic!("Fixture cleanup failed: {error}"),
                }
            }
            match std::fs::remove_dir_all(&self.root) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("Fixture cleanup failed: {error}"),
            }
        }
    }
}
