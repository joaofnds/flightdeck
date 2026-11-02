//! Read-only compatibility and writer admission for immutable recording root formats.
use std::io::{self, Read};
use std::path::Path;

use super::{identity_text, parse_identity, private};
use crate::ring::Identity;

const HEADER: &[u8] = b"flightdeck-recording-1\n";
const REQUIREMENT_BYTES: usize = 9;
const IDENTITY_BYTES: usize = 33;
const BYTES: usize = HEADER.len() + 2 * REQUIREMENT_BYTES + IDENTITY_BYTES;
pub const READER_PROTOCOL: u32 = 1;
pub const WRITER_PROTOCOL: u32 = 1;
const _: () = assert!(BYTES <= 128);

struct Format {
    identity: Identity,
    reader: u32,
    writer: u32,
}

pub(super) fn read_identity(root: &Path) -> io::Result<Identity> {
    Ok(read(root)?.identity)
}

pub(super) fn read_writer_identity(root: &Path) -> io::Result<Identity> {
    let format = read(root)?;
    if format.writer != WRITER_PROTOCOL {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "recording requires a different writer protocol; use a new recording root",
        ));
    }

    Ok(format.identity)
}

pub(super) fn create(root: &Path, identity: Identity) -> io::Result<()> {
    let header = std::str::from_utf8(HEADER).map_err(|_| invalid())?;
    let text = format!(
        "{header}{READER_PROTOCOL:08x}\n{WRITER_PROTOCOL:08x}\n{}\n",
        identity_text(identity)
    );
    assert_eq!(text.len(), BYTES);
    private::atomic_write(root, "identity", text.as_bytes())
}

fn read(root: &Path) -> io::Result<Format> {
    let directory = private::PrivateDirectory::open(root)?;
    let mut file = directory.open_file("identity")?;
    let mut bytes = [0; BYTES + 1];
    let mut length = 0;
    while length < bytes.len() {
        let count = file.read(&mut bytes[length..])?;
        if count == 0 {
            break;
        }
        length += count;
    }
    let format = parse(&bytes[..length])?;
    if format.reader > READER_PROTOCOL {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }

    Ok(format)
}

fn parse(bytes: &[u8]) -> io::Result<Format> {
    if bytes.len() == IDENTITY_BYTES {
        return Ok(Format {
            identity: parse_identity_line(bytes)?,
            reader: 0,
            writer: 0,
        });
    }

    if bytes.len() != BYTES || !bytes.starts_with(HEADER) {
        return Err(invalid());
    }

    let reader_start = HEADER.len();
    let writer_start = reader_start + REQUIREMENT_BYTES;
    let identity_start = writer_start + REQUIREMENT_BYTES;
    let reader = parse_requirement(&bytes[reader_start..writer_start])?;
    let writer = parse_requirement(&bytes[writer_start..identity_start])?;
    if reader == 0 || writer == 0 {
        return Err(invalid());
    }

    Ok(Format {
        identity: parse_identity_line(&bytes[identity_start..])?,
        reader,
        writer,
    })
}

fn parse_requirement(bytes: &[u8]) -> io::Result<u32> {
    if bytes.len() != REQUIREMENT_BYTES || bytes[8] != b'\n' {
        return Err(invalid());
    }

    let digits = &bytes[..8];
    if !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid());
    }

    let text = std::str::from_utf8(digits).map_err(|_| invalid())?;
    u32::from_str_radix(text, 16).map_err(|_| invalid())
}

fn parse_identity_line(bytes: &[u8]) -> io::Result<Identity> {
    if bytes.len() != IDENTITY_BYTES || bytes[32] != b'\n' {
        return Err(invalid());
    }

    parse_identity(std::str::from_utf8(&bytes[..32]).map_err(|_| invalid())?)
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Root(PathBuf);

    impl Root {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "fd-root-format-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            private::create_private_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn production_identity_read_uses_the_full_admitted_root() {
        // Goal: identity reads retain the publisher's full admitted root-path coverage.
        // Method: publish at 1023 bytes, then exercise the actual production entrypoints.
        let fixture = Root::new();
        let mut root = fixture.0.canonicalize().unwrap();
        for _ in 0..8 {
            let remaining = (private::PATH_MAX - 1) - root.as_os_str().len();
            if remaining == 0 {
                break;
            }
            root.push("x".repeat((remaining - 1).min(200)));
            private::create_private_dir(&root).unwrap();
        }
        assert_eq!(root.as_os_str().len(), 1023);
        assert!(root.join("identity").as_os_str().len() >= private::PATH_MAX);
        create(&root, Identity([1, 2])).unwrap();
        let directory = private::PrivateDirectory::open(&root).unwrap();
        let mut before = Vec::new();
        directory
            .open_file("identity")
            .unwrap()
            .read_to_end(&mut before)
            .unwrap();
        let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
            assert_eq!(read_identity(&root).unwrap(), Identity([1, 2]));
            assert_eq!(read_writer_identity(&root).unwrap(), Identity([1, 2]));
            assert_eq!(
                crate::recording::read_identity(&root).unwrap(),
                Identity([1, 2])
            );
            crate::recording::check_writer(&root).unwrap();
            assert_eq!(
                crate::recording::recording_identity(&root).unwrap(),
                Identity([1, 2])
            );
        });
        assert_eq!(calls, 0);
        let mut after = Vec::new();
        directory
            .open_file("identity")
            .unwrap()
            .read_to_end(&mut after)
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn identity_read_preserves_root_admission_bounds() {
        // Goal: descriptor-relative reads must not widen root admission.
        // Method: refuse an overlong root, a final root symlink and a public root mode.
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = Root::new();
        create(&root.0, Identity([1, 2])).unwrap();
        let alias = root.0.join("alias");
        symlink(&root.0, &alias).unwrap();
        let overlong = PathBuf::from("x".repeat(private::PATH_MAX));
        let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
            assert!(read_identity(&alias).is_err());
            assert_eq!(
                read_identity(&overlong).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        });
        assert_eq!(calls, 0);
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
            assert_eq!(
                read_identity(&root.0).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        });
        assert_eq!(calls, 0);
        assert_eq!(
            std::fs::metadata(&root.0).unwrap().permissions().mode() & 0o7777,
            0o755
        );
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(read_identity(&root.0).unwrap(), Identity([1, 2]));
    }

    #[test]
    fn refused_identity_reads_allocate_nothing_and_preserve_artifacts() {
        // Goal: production readers refuse substituted or invalid identities without mutation.
        // Method: exercise real files, a final symlink and a directory under the allocator guard.
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

        let root = Root::new();
        let identity = root.0.join("identity");
        for bytes in [
            b"malformed".as_slice(),
            b"flightdeck-recording-1\n00000002\n00000001\n00000000000000010000000000000002\n",
            &[b'x'; BYTES + 1],
        ] {
            private::atomic_write(&root.0, "identity", bytes).unwrap();
            let before = std::fs::metadata(&identity).unwrap();
            let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
                assert!(read_identity(&root.0).is_err());
                assert!(crate::recording::check_writer(&root.0).is_err());
                assert!(crate::recording::recording_identity(&root.0).is_err());
            });
            assert_eq!(calls, 0);
            assert_eq!(std::fs::read(&identity).unwrap(), bytes);
            assert_eq!(std::fs::metadata(&identity).unwrap().ino(), before.ino());
            assert_eq!(std::fs::metadata(&identity).unwrap().mode(), before.mode());
        }
        create(&root.0, Identity([1, 2])).unwrap();
        let target = root.0.join("target");
        std::fs::rename(&identity, &target).unwrap();
        let before = std::fs::read(&target).unwrap();
        symlink("target", &identity).unwrap();
        let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
            assert!(read_identity(&root.0).is_err());
            assert!(crate::recording::recording_identity(&root.0).is_err());
        });
        assert_eq!(calls, 0);
        assert_eq!(std::fs::read_link(&identity).unwrap(), Path::new("target"));
        assert_eq!(std::fs::read(&target).unwrap(), before);
        std::fs::remove_file(&identity).unwrap();
        private::create_private_dir(&identity).unwrap();
        let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
            assert!(read_identity(&root.0).is_err());
        });
        assert_eq!(calls, 0);
        assert_eq!(std::fs::read_dir(&identity).unwrap().count(), 0);
        std::fs::remove_dir(&identity).unwrap();
        std::fs::rename(&target, &identity).unwrap();
        std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o644)).unwrap();
        let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
            assert_eq!(
                read_identity(&root.0).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        });
        assert_eq!(calls, 0);
        assert_eq!(std::fs::read(&identity).unwrap(), before);
        assert_eq!(std::fs::metadata(&identity).unwrap().mode() & 0o7777, 0o644);
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn reader_and_writer_requirements_are_independent_and_all_reads_are_bounded() {
        // Goal: a compatible reader may inspect a future writer's root without acquiring it.
        // Method: validate each required-version pair and malformed fixed envelope in place.
        let root = Root::new();
        for (reader, writer, read_ok, write_ok) in [
            (1, 1, true, true),
            (1, 2, true, false),
            (2, 1, false, false),
            (0, 1, false, false),
            (1, 0, false, false),
        ] {
            let text = format!(
                "flightdeck-recording-1\n{reader:08x}\n{writer:08x}\n\
                 00000000000000010000000000000002\n"
            );
            private::atomic_write(&root.0, "identity", text.as_bytes()).unwrap();
            assert_eq!(read_identity(&root.0).is_ok(), read_ok);
            assert_eq!(read_writer_identity(&root.0).is_ok(), write_ok);
            assert_eq!(
                std::fs::read(root.0.join("identity")).unwrap(),
                text.as_bytes()
            );
        }
        create(&root.0, Identity([1, 2])).unwrap();
        let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
            assert_eq!(read_identity(&root.0).unwrap(), Identity([1, 2]));
        });
        assert_eq!(calls, 0);
        let valid = std::fs::read(root.0.join("identity")).unwrap();
        for bytes in [&valid[..valid.len() - 1], &[b'x'; BYTES + 1]] {
            private::atomic_write(&root.0, "identity", bytes).unwrap();
            assert_eq!(
                read_identity(&root.0).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn concurrent_new_root_initialization_keeps_one_versioned_identity() {
        // Goal: two creating writers cannot publish distinct identities for the same root.
        // Method: race the production registration lock and initialization in two threads.
        let root = Root::new();
        let barrier = std::sync::Barrier::new(2);
        let identities = std::thread::scope(|scope| {
            let mut children = Vec::new();
            for _writer in 0..2 {
                children.push(scope.spawn(|| {
                    barrier.wait();
                    let _registration =
                        private::FileLock::wait_short(&root.0.join("registration.lock")).unwrap();
                    crate::recording::recording_identity(&root.0).unwrap()
                }));
            }
            children
                .into_iter()
                .map(|child| child.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(identities[0], identities[1]);
        assert_eq!(read_writer_identity(&root.0).unwrap(), identities[0]);
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 2);
    }
}
