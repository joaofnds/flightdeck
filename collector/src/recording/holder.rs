//! The collector holding a recording. Its lifetime lock says whether one holds it, and its
//! announcement says which run it writes and what code built it.

use std::io::{self, Read};
use std::path::Path;

use super::{BUILD, identity_text, parse_identity, private};
use crate::ring::Identity;

const ANNOUNCEMENT: &str = "collector.announcement";
const HEADER: &str = "flightdeck-collector-1";
/// The header, the run's identity and the build, each on a line of its own.
const ANNOUNCEMENT_BYTES: usize = HEADER.len() + 1 + 32 + 1 + 16 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    Absent,
    /// A collector holds the recording but names no run, since it is starting or predates
    /// announcements.
    Unannounced,
    Announced {
        run: Identity,
        build: u64,
    },
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

    if length != ANNOUNCEMENT_BYTES {
        return Err(invalid());
    }
    holder_parse(&bytes[..length])
}

fn holder_parse(bytes: &[u8]) -> io::Result<Holder> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err(invalid());
    }
    let run = parse_identity(lines.next().ok_or_else(invalid)?)?;
    let build = lines.next().ok_or_else(invalid)?;
    if build.len() != 16 {
        return Err(invalid());
    }
    if !build.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
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
    let text = format!("{HEADER}\n{}\n{BUILD:016x}\n", identity_text(run));
    assert_eq!(ANNOUNCEMENT_BYTES, text.len());
    private::atomic_write(root, ANNOUNCEMENT, text.as_bytes())
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;
    use crate::recording::{Registration, private};
    use crate::ring::{Place, Shape, Writer, unlink_generation};

    #[test]
    fn a_started_recording_announces_its_run_and_build() {
        // Goal: a reader learns which run the holding collector writes and what code built it.
        // Method: start a real registered recording and read its holder.
        let fixture = Fixture::new("started");
        let writer = fixture.start();
        let run = writer.identity().unwrap().run;

        let holder = holder(&fixture.root).unwrap();

        assert_eq!(Holder::Announced { run, build: BUILD }, holder);
        Fixture::finish(writer);
    }

    #[test]
    fn a_recording_whose_collector_exited_has_no_holder() {
        // Goal: a pid macOS gave another process cannot pass for the exited collector.
        // Method: stop a registered collector, whose registry entry and announcement stay.
        let fixture = Fixture::new("exited");
        let writer = fixture.start();
        let name = writer.name().to_owned();
        drop(writer);

        let holder = holder(&fixture.root).unwrap();

        assert_eq!(Holder::Absent, holder);
        unlink_generation(&name).unwrap();
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
    fn taking_the_recording_withdraws_the_last_announcement() {
        // Goal: a new holder is never read as the run its predecessor announced.
        // Method: stop one collector, then take its recording without registering a run.
        let fixture = Fixture::new("withdrawn");
        let writer = fixture.start();
        let name = writer.name().to_owned();
        drop(writer);

        let ownership = Registration::start_ownership(&fixture.root).unwrap();

        assert_eq!(Holder::Unannounced, holder(&fixture.root).unwrap());
        drop(ownership);
        unlink_generation(&name).unwrap();
    }

    #[test]
    fn reads_the_holder_without_allocating() {
        // Goal: a reader can poll the holder from fixed storage.
        // Method: count allocations while reading a registered recording's holder.
        let fixture = Fixture::new("allocations");
        let writer = fixture.start();

        let calls = allocator_calls_in(|| {
            assert!(matches!(
                holder(&fixture.root).unwrap(),
                Holder::Announced { .. }
            ));
        });

        assert_eq!(0, calls);
        Fixture::finish(writer);
    }

    #[test]
    fn refuses_a_malformed_announcement() {
        // Goal: an announcement no collector wrote is never read as a run and build.
        // Method: hold the lock beside an announcement whose build is not hexadecimal.
        let fixture = Fixture::new("malformed");
        let lock = private::FileLock::try_lock(&fixture.root.join("collector.lock")).unwrap();
        let text = concat!(
            "flightdeck-collector-1\n",
            "00000000000000010000000000000002\n",
            "000000000000000x\n"
        );
        private::atomic_write(&fixture.root, "collector.announcement", text.as_bytes()).unwrap();

        let error = holder(&fixture.root).unwrap_err();

        assert_eq!(io::ErrorKind::InvalidData, error.kind());
        drop(lock);
    }

    struct Fixture {
        root: std::path::PathBuf,
        place: Place,
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
            Self { root, place }
        }

        fn start(&self) -> Writer {
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
            let (writer, registered) = Registration::start(&self.root, &self.place, shape, 0)
                .unwrap()
                .into_parts();
            registered.unwrap();
            writer
        }

        fn finish(writer: Writer) {
            let name = writer.name().to_owned();
            drop(writer);
            unlink_generation(&name).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            match std::fs::remove_dir_all(&self.root) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("Fixture cleanup failed: {error}"),
            }
        }
    }
}
