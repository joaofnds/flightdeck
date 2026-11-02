use super::super::*;
use crate::recording::discovery::tests::allocator_calls_in;
use std::io::Read;
use std::process::Command;

unsafe extern "C" {
    fn openat(directory: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn mkfifo(path: *const c_char, mode: u32) -> c_int;
    #[cfg_attr(target_arch = "x86_64", link_name = "fstat$INODE64")]
    fn fstat(descriptor: c_int, metadata: *mut Stat) -> c_int;
}

// Mirrors the 64-bit-inode struct stat in the macOS SDK's sys/stat.h.
#[repr(C)]
struct Stat {
    dev: i32,
    mode: u16,
    nlink: u16,
    ino: u64,
    uid: u32,
    gid: u32,
    rdev: i32,
    atimespec: [i64; 2],
    mtimespec: [i64; 2],
    ctimespec: [i64; 2],
    birthtimespec: [i64; 2],
    size: i64,
    blocks: i64,
    blksize: i32,
    flags: u32,
    r#gen: u32,
    lspare: i32,
    qspare: [i64; 2],
}

const _: () = assert!(size_of::<Stat>() == 144);
const _: () = assert!(std::mem::offset_of!(Stat, ino) == 8);

#[test]
fn publishes_and_replaces_beyond_the_combined_path_limit() {
    // Goal: admitted roots retain their full path coverage when metadata names are added.
    // Method: exceed PATH_MAX only in the combined path, publish twice, then read by descriptor.
    let fixture = DirectoryFixture::new("long-root");
    let root = fixture.long_root();
    assert_eq!(PATH_MAX - 1, root.as_os_str().len());
    verify_directory(&root).unwrap();
    let name = "metadata-with-a-long-single-component";
    let mut buffer = [0; PATH_MAX];
    assert!(join_path(&root, name, &mut buffer).is_err());
    assert_eq!(
        Some(63),
        File::open(root.join(name)).unwrap_err().raw_os_error()
    );

    let calls = allocator_calls_in(|| {
        atomic_write(&root, name, b"first publication\n").unwrap();
        atomic_write(&root, name, b"replacement\n").unwrap();
    });

    let directory = open_directory(&root).unwrap();
    let mut file = open_relative(&directory, c"metadata-with-a-long-single-component").unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(b"replacement\n", bytes.as_slice());
    verify_open(&file, false).unwrap();
    assert_eq!(0, calls);
    assert_eq!(
        io::ErrorKind::NotFound,
        open_relative(&directory, c"metadata-with-a-long-single-component.pending")
            .unwrap_err()
            .kind()
    );
}

#[test]
fn failed_rename_keeps_the_target_and_removes_pending_bytes() {
    // Goal: failed publication exposes no partial target and leaves no pending artifact.
    // Method: replace a nonempty directory, forcing a native rename refusal after file sync.
    let fixture = DirectoryFixture::new("rename-failure");
    let target = fixture.0.join("metadata");
    create_private_dir(&target).unwrap();
    fs::write(target.join("sentinel"), b"unchanged").unwrap();

    let calls = allocator_calls_in(|| {
        assert!(atomic_write(&fixture.0, "metadata", b"unpublished").is_err());
    });

    assert_eq!(
        b"unchanged",
        fs::read(target.join("sentinel")).unwrap().as_slice()
    );
    assert_eq!(
        io::ErrorKind::NotFound,
        fs::symlink_metadata(fixture.0.join("metadata.pending"))
            .unwrap_err()
            .kind()
    );
    assert_eq!(0, calls);
}

#[test]
fn replaces_a_stale_private_pending_file_without_changing_open_readers() {
    // Goal: retry discards stale staging, while a reader retains the prior complete inode.
    // Method: pin the target, seed a longer pending file, and publish shorter replacement bytes.
    let fixture = DirectoryFixture::new("stale-pending");
    atomic_write(&fixture.0, "metadata", b"prior").unwrap();
    let mut prior = open_private_file(&fixture.0.join("metadata"), false).unwrap();
    let mut pending = open_private_file(&fixture.0.join("metadata.pending"), true).unwrap();
    pending.write_all(b"stale trailing bytes").unwrap();
    drop(pending);

    let calls = allocator_calls_in(|| atomic_write(&fixture.0, "metadata", b"new").unwrap());

    let mut bytes = Vec::new();
    prior.read_to_end(&mut bytes).unwrap();
    assert_eq!(b"prior", bytes.as_slice());
    assert_eq!(
        b"new",
        fs::read(fixture.0.join("metadata")).unwrap().as_slice()
    );
    assert!(!fixture.0.join("metadata.pending").exists());
    assert_eq!(0, calls);
}

#[test]
fn component_and_pending_name_limits_are_independent() {
    // Goal: each component can use NAME_MAX, while publication reserves the pending suffix.
    // Method: create a 255-byte file, publish a 247-byte name, and refuse one byte more.
    let fixture = DirectoryFixture::new("name-limits");
    let directory = PrivateDirectory::open(&fixture.0).unwrap();
    let component = "a".repeat(NAME_MAX);
    let published = "b".repeat(NAME_MAX - ".pending".len());
    let refused = "c".repeat(published.len() + 1);

    let calls = allocator_calls_in(|| {
        drop(directory.create_file(&component).unwrap());
        drop(directory.open_file(&component).unwrap());
        atomic_write(&fixture.0, &published, b"maximum").unwrap();
        assert_eq!(
            io::ErrorKind::InvalidInput,
            atomic_write(&fixture.0, &refused, b"refused")
                .unwrap_err()
                .kind()
        );
    });

    assert_eq!(
        b"maximum",
        fs::read(fixture.0.join(published)).unwrap().as_slice()
    );
    assert_eq!(2, fs::read_dir(&fixture.0).unwrap().count());
    assert_eq!(0, calls);
}

macro_rules! invalid_component {
    ($test:ident, $name:expr) => {
        #[test]
        fn $test() {
            // Goal: invalid components cannot escape or mutate the admitted namespace.
            // Method: try the same invalid name through every exposed component operation.
            let fixture = DirectoryFixture::new(stringify!($test));
            let directory = PrivateDirectory::open(&fixture.0).unwrap();
            let name = $name;
            drop(directory.create_file("sentinel").unwrap());

            let calls = allocator_calls_in(|| {
                assert_eq!(
                    io::ErrorKind::InvalidInput,
                    directory.open_file(&name).err().unwrap().kind()
                );
                assert_eq!(
                    io::ErrorKind::InvalidInput,
                    directory.create_file(&name).err().unwrap().kind()
                );
                assert_eq!(
                    io::ErrorKind::InvalidInput,
                    directory.open_directory(&name).err().unwrap().kind()
                );
                assert_eq!(
                    io::ErrorKind::InvalidInput,
                    directory.remove_file(&name).unwrap_err().kind()
                );
                assert_eq!(
                    io::ErrorKind::InvalidInput,
                    directory.rename(&name, "other").unwrap_err().kind()
                );
                assert_eq!(
                    io::ErrorKind::InvalidInput,
                    directory.rename("sentinel", &name).unwrap_err().kind()
                );
                assert_eq!(
                    io::ErrorKind::InvalidInput,
                    atomic_write(&fixture.0, &name, b"bad").unwrap_err().kind()
                );
            });

            assert_eq!(1, fs::read_dir(&fixture.0).unwrap().count());
            assert!(fixture.0.join("sentinel").is_file());
            assert_eq!(0, calls);
        }
    };
}

invalid_component!(refuses_empty_names, "");
invalid_component!(refuses_dot_names, ".");
invalid_component!(refuses_parent_names, "..");
invalid_component!(refuses_separators, "child/other");
invalid_component!(refuses_absolute_names, "/other");
invalid_component!(refuses_nul_names, "child\0other");
invalid_component!(refuses_overlength_names, "x".repeat(NAME_MAX + 1));

#[test]
fn an_admitted_directory_survives_renaming_and_path_substitution() {
    // Goal: child opens and publication operations stay anchored to the admitted inode.
    // Method: move its path, substitute another directory, then use the original handle.
    let fixture = DirectoryFixture::new("anchored");
    let original = fixture.0.join("original");
    create_private_dir(&original).unwrap();
    create_private_dir(&original.join("child")).unwrap();
    let directory = PrivateDirectory::open(&original).unwrap();
    let moved = fixture.0.join("moved");
    fs::rename(&original, &moved).unwrap();
    create_private_dir(&original).unwrap();

    let calls = allocator_calls_in(|| {
        let child = directory.open_directory("child").unwrap();
        let mut file = child.create_file("pending").unwrap();
        file.write_all(b"anchored").unwrap();
        full_sync(&file).unwrap();
        child.rename("pending", "published").unwrap();
        child.full_sync().unwrap();
    });

    assert_eq!(
        b"anchored",
        fs::read(moved.join("child/published")).unwrap().as_slice()
    );
    assert_eq!(0, fs::read_dir(original).unwrap().count());
    assert_eq!(0, calls);
}

#[test]
fn exclusive_creation_preserves_an_existing_file() {
    // Goal: creation never silently opens an existing artifact for mutation.
    // Method: create twice and check the first descriptor and bytes remain intact.
    let fixture = DirectoryFixture::new("exclusive");
    let directory = PrivateDirectory::open(&fixture.0).unwrap();
    let mut file = directory.create_file("existing").unwrap();
    file.write_all(b"existing").unwrap();

    assert_eq!(
        io::ErrorKind::AlreadyExists,
        directory.create_file("existing").unwrap_err().kind()
    );

    assert_eq!(
        b"existing",
        fs::read(fixture.0.join("existing")).unwrap().as_slice()
    );
}

#[test]
fn refuses_nonprivate_roots_before_creating_pending_files() {
    // Goal: atomic publication requires a private admitted directory before any mutation.
    // Method: loosen root permissions and confirm native publication refuses it untouched.
    let fixture = DirectoryFixture::new("root-mode");
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();

    let calls = allocator_calls_in(|| {
        assert_eq!(
            io::ErrorKind::PermissionDenied,
            atomic_write(&fixture.0, "metadata", b"refused")
                .unwrap_err()
                .kind()
        );
    });

    assert_eq!(0, fs::read_dir(&fixture.0).unwrap().count());
    assert_eq!(0, calls);
}

#[test]
fn refuses_a_final_root_symlink() {
    // Goal: publication never admits a final symlink as its root.
    // Method: publish through an alias and verify the real root is untouched.
    let fixture = DirectoryFixture::new("root-symlink");
    let root = fixture.0.join("root");
    create_private_dir(&root).unwrap();
    let alias = fixture.0.join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();

    assert!(atomic_write(&alias, "metadata", b"refused").is_err());

    assert_eq!(0, fs::read_dir(root).unwrap().count());
}

#[test]
fn refuses_pending_symlinks_without_changing_their_target() {
    // Goal: recovery of stale pending metadata cannot follow or remove a substituted link.
    // Method: place a real pending symlink and check both link and target survive refusal.
    let fixture = DirectoryFixture::new("pending-symlink");
    fs::write(fixture.0.join("target"), b"unchanged").unwrap();
    let pending = fixture.0.join("metadata.pending");
    std::os::unix::fs::symlink("target", &pending).unwrap();

    assert_eq!(
        Some(62),
        atomic_write(&fixture.0, "metadata", b"bad")
            .unwrap_err()
            .raw_os_error()
    );

    assert!(fs::symlink_metadata(pending).unwrap().is_symlink());
    assert_eq!(
        b"unchanged",
        fs::read(fixture.0.join("target")).unwrap().as_slice()
    );
    assert!(!fixture.0.join("metadata").exists());
}

#[test]
fn refuses_pending_directories_without_deleting_them() {
    // Goal: stale pending handling refuses a directory rather than treating it as a file.
    // Method: seed a private pending directory and attempt the production publication.
    let fixture = DirectoryFixture::new("pending-directory");
    let pending = fixture.0.join("metadata.pending");
    create_private_dir(&pending).unwrap();

    assert_eq!(
        io::ErrorKind::InvalidData,
        atomic_write(&fixture.0, "metadata", b"bad")
            .unwrap_err()
            .kind()
    );

    assert!(pending.is_dir());
    assert!(!fixture.0.join("metadata").exists());
}

#[test]
fn refuses_pending_fifos_without_waiting_for_a_writer() {
    // Goal: native type substitution cannot block pending-file validation.
    // Method: present a FIFO with no writer, which must open nonblocking and fail file checks.
    let fixture = DirectoryFixture::new("pending-fifo");
    let pending = fixture.0.join("metadata.pending");
    let mut buffer = [0; PATH_MAX];
    let name = path_name(&pending, &mut buffer).unwrap();
    assert_eq!(0, unsafe { mkfifo(name.as_ptr(), 0o600) });

    assert_eq!(
        io::ErrorKind::InvalidData,
        atomic_write(&fixture.0, "metadata", b"bad")
            .unwrap_err()
            .kind()
    );

    assert!(fs::symlink_metadata(pending).is_ok());
    assert!(!fixture.0.join("metadata").exists());
}

#[test]
fn refuses_pending_modes_without_repairing_or_deleting_them() {
    // Goal: existing nonprivate staging is refused before mutation.
    // Method: loosen pending permissions and verify bytes and mode survive publication refusal.
    let fixture = DirectoryFixture::new("pending-mode");
    let pending = fixture.0.join("metadata.pending");
    let mut file = open_private_file(&pending, true).unwrap();
    file.write_all(b"unchanged").unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o644))
        .unwrap();

    assert_eq!(
        io::ErrorKind::PermissionDenied,
        atomic_write(&fixture.0, "metadata", b"bad")
            .unwrap_err()
            .kind()
    );

    assert_eq!(b"unchanged", fs::read(&pending).unwrap().as_slice());
    assert_eq!(0o644, fs::metadata(pending).unwrap().mode() & 0o7777);
    assert!(!fixture.0.join("metadata").exists());
}

#[test]
fn refuses_real_extended_acls_on_roots_children_and_pending_files() {
    // Goal: descriptor admission reuses the no-ACL policy without clearing existing ACLs.
    // Method: install native ACL entries on three admission boundaries and verify refusal.
    let fixture = DirectoryFixture::new("acl");
    let child = fixture.0.join("child");
    create_private_dir(&child).unwrap();
    let pending = fixture.0.join("metadata.pending");
    drop(open_private_file(&pending, true).unwrap());
    let directory = PrivateDirectory::open(&fixture.0).unwrap();
    add_acl(&child);
    add_acl(&pending);

    let calls = allocator_calls_in(|| {
        assert_eq!(
            io::ErrorKind::PermissionDenied,
            directory.open_directory("child").err().unwrap().kind()
        );
        assert_eq!(
            io::ErrorKind::PermissionDenied,
            atomic_write(&fixture.0, "metadata", b"bad")
                .unwrap_err()
                .kind()
        );
    });

    assert_eq!(0, calls);
    assert!(!fixture.0.join("metadata").exists());
    assert_eq!(
        1,
        read_acl_entries(&open_directory(&child).unwrap()).unwrap()
    );
    add_acl(&fixture.0);
    assert_eq!(
        io::ErrorKind::PermissionDenied,
        PrivateDirectory::open(&fixture.0).err().unwrap().kind()
    );
    assert_eq!(
        1,
        read_acl_entries(&open_directory(&fixture.0).unwrap()).unwrap()
    );
}

#[test]
fn descriptor_ownership_is_bounded_and_failures_release_every_open() {
    // Goal: admitted descriptors are close-on-exec and every failed admission releases its open.
    // Method: count only descriptors of this fixture's inodes across real repeated refusals.
    let fixture = DirectoryFixture::new("descriptor-lifetime");
    let child = fixture.0.join("child");
    create_private_dir(&child).unwrap();
    let denied = fixture.0.join("denied");
    drop(open_private_file(&denied, true).unwrap());
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).unwrap();
    let identities = [
        fs::metadata(&fixture.0).unwrap(),
        fs::metadata(&child).unwrap(),
        fs::metadata(&denied).unwrap(),
    ];
    assert_eq!(0, fixture_descriptor_count(&identities));

    let directory = PrivateDirectory::open(&fixture.0).unwrap();
    assert_close_on_exec(&directory.file);
    assert_eq!(1, fixture_descriptor_count(&identities));
    let calls = allocator_calls_in(|| {
        for _ in 0..32 {
            assert!(directory.open_file("denied").is_err());
            assert!(directory.open_file("child").is_err());
            assert!(directory.open_file("absent").is_err());
            assert!(directory.open_directory("child").is_err());
            assert!(directory.open_directory("denied").is_err());
            assert!(directory.create_directory("child").is_err());
            assert!(directory.create_directory("denied").is_err());
            assert!(PrivateDirectory::open(&child).is_err());
            assert!(atomic_write(&child, "metadata", b"bad").is_err());
        }
    });

    assert_eq!(0, calls);
    assert_eq!(1, fixture_descriptor_count(&identities));
    let file = directory.create_file("created").unwrap();
    assert_close_on_exec(&file);
    let created_identity = [file.metadata().unwrap()];
    assert_eq!(1, fixture_descriptor_count(&created_identity));
    drop(file);
    assert_eq!(0, fixture_descriptor_count(&created_identity));
    drop(directory);
    assert_eq!(0, fixture_descriptor_count(&identities));
}

#[test]
fn child_directory_opens_refuse_final_symlinks() {
    // Goal: component traversal never admits a symlink as a child directory.
    // Method: substitute a child alias and verify native open refusal leaves its target intact.
    let fixture = DirectoryFixture::new("child-symlink");
    create_private_dir(&fixture.0.join("child")).unwrap();
    std::os::unix::fs::symlink("child", fixture.0.join("alias")).unwrap();
    let directory = PrivateDirectory::open(&fixture.0).unwrap();

    assert!(directory.open_directory("alias").is_err());

    assert!(fixture.0.join("child").is_dir());
}

fn assert_close_on_exec(file: &File) {
    const F_GETFD: c_int = 1;
    const FD_CLOEXEC: c_int = 1;
    let flags = unsafe { fcntl(file.as_raw_fd(), F_GETFD) };
    assert!(flags >= 0);
    assert_eq!(FD_CLOEXEC, flags & FD_CLOEXEC);
}

fn fixture_descriptor_count(identities: &[fs::Metadata]) -> usize {
    // Inspect without duplicating: a duplicate could prolong another test's flock ownership.
    let entries = fs::read_dir("/dev/fd")
        .unwrap()
        .take(65_536)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut count = 0;
    for entry in entries {
        let descriptor = entry
            .file_name()
            .to_str()
            .unwrap()
            .parse::<c_int>()
            .unwrap();
        let mut metadata = std::mem::MaybeUninit::<Stat>::zeroed();
        if unsafe { fstat(descriptor, metadata.as_mut_ptr()) } != 0 {
            let error = io::Error::last_os_error();
            // The enumeration itself and concurrent unrelated tests can close descriptors.
            assert_eq!(Some(9), error.raw_os_error());
            continue;
        }
        let metadata = unsafe { metadata.assume_init() };
        if identities.iter().any(|identity| {
            i32::try_from(identity.dev()).unwrap() == metadata.dev && identity.ino() == metadata.ino
        }) {
            count += 1;
        }
    }
    count
}

fn add_acl(path: &Path) {
    let status = Command::new("/bin/chmod")
        .args(["+a", "everyone allow read"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

struct DirectoryFixture(std::path::PathBuf);

impl DirectoryFixture {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "flightdeck-p02-increment1a-{case}-{}",
            std::process::id()
        ));
        create_private_dir(&root).unwrap();
        Self(root)
    }

    fn long_root(&self) -> std::path::PathBuf {
        // /var's expansion to /private/var consumes native path bytes on macOS.
        let mut root = self.0.canonicalize().unwrap();
        for _ in 0..8 {
            let remaining = (PATH_MAX - 1) - root.as_os_str().len();
            if remaining == 0 {
                return root;
            }
            root.push("x".repeat((remaining - 1).min(200)));
            create_private_dir(&root).unwrap();
        }
        panic!("fixed fixture depth must reach the admitted root length");
    }
}

impl Drop for DirectoryFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn open_relative(directory: &File, name: &CStr) -> io::Result<File> {
    let descriptor = unsafe {
        openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            O_RDONLY | O_NOFOLLOW | O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[test]
fn retained_directory_publishes_and_creates_after_root_substitution() {
    // Goal: retained publication and mkdir affect only the admitted inode.
    // Method: replace its pathname with another private root before both operations.
    let fixture = DirectoryFixture::new("retained-bootstrap");
    let root = fixture.0.join("root");
    create_private_dir(&root).unwrap();
    let directory = PrivateDirectory::open(&root).unwrap();
    let retained = fixture.0.join("retained");
    fs::rename(&root, &retained).unwrap();
    create_private_dir(&root).unwrap();

    let calls = allocator_calls_in(|| {
        atomic_write_in(&directory, "metadata", b"retained").unwrap();
        drop(directory.create_directory("child").unwrap());
        drop(directory.create_directory("child").unwrap());
    });

    assert_eq!(0, calls);
    assert_eq!(
        b"retained",
        fs::read(retained.join("metadata")).unwrap().as_slice()
    );
    verify_directory(&retained.join("child")).unwrap();
    assert_eq!(0, fs::read_dir(root).unwrap().count());
}

#[test]
fn child_creation_refuses_existing_foreign_objects_without_repair() {
    // Goal: create-or-validate never repairs or replaces existing objects.
    // Method: admit a parent, then present nonprivate, ACL, file and symlink children.
    let fixture = DirectoryFixture::new("mkdir-refusal");
    let directory = PrivateDirectory::open(&fixture.0).unwrap();
    let child = fixture.0.join("child");
    create_private_dir(&child).unwrap();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).unwrap();
    drop(open_private_file(&fixture.0.join("file"), true).unwrap());
    std::os::unix::fs::symlink("child", fixture.0.join("alias")).unwrap();
    let acl = fixture.0.join("acl");
    create_private_dir(&acl).unwrap();
    add_acl(&acl);

    let overlong = "x".repeat(NAME_MAX + 1);
    let calls = allocator_calls_in(|| {
        assert_eq!(
            io::ErrorKind::PermissionDenied,
            directory.create_directory("child").err().unwrap().kind()
        );
        assert!(directory.create_directory("file").is_err());
        assert!(directory.create_directory("alias").is_err());
        assert_eq!(
            io::ErrorKind::PermissionDenied,
            directory.create_directory("acl").err().unwrap().kind()
        );
        assert_eq!(
            io::ErrorKind::InvalidInput,
            directory
                .create_directory("../escape")
                .err()
                .unwrap()
                .kind()
        );
        assert_eq!(
            io::ErrorKind::InvalidInput,
            directory.create_directory(&overlong).err().unwrap().kind()
        );
    });

    assert_eq!(0, calls);
    assert_eq!(0o755, fs::metadata(child).unwrap().mode() & 0o7777);
    assert_eq!(1, read_acl_entries(&open_directory(&acl).unwrap()).unwrap());
    assert!(
        fs::symlink_metadata(fixture.0.join("alias"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn child_creation_retains_full_root_and_component_bounds() {
    // Goal: mkdir admits the longest root and child without a combined path buffer.
    // Method: create and reopen a maximum component beneath a 1023-byte root.
    let fixture = DirectoryFixture::new("mkdir-long-root");
    let root = fixture.long_root();
    let directory = PrivateDirectory::open(&root).unwrap();
    let name = "x".repeat(NAME_MAX);

    let calls = allocator_calls_in(|| {
        let child = directory.create_directory(&name).unwrap();
        assert_close_on_exec(&child.file);
        drop(child);
        drop(directory.open_directory(&name).unwrap());
    });

    assert_eq!(0, calls);
    let identities = [directory.file.metadata().unwrap()];
    assert_eq!(1, fixture_descriptor_count(&identities));
    drop(directory);
    assert_eq!(0, fixture_descriptor_count(&identities));
}

#[test]
fn retained_names_retry_the_failed_entry_and_report_stable_eof() {
    // Goal: incremental enumeration never advances past a refused name or reallocates.
    // Method: refuse the first non-dot name, repeat it, then exhaust the retained descriptor.
    let fixture = DirectoryFixture::new("retained-names");
    let directory = PrivateDirectory::open(&fixture.0).unwrap();
    directory.create_file("one").unwrap();
    let mut work = DirectoryWork::default();
    let mut names = directory.names(&mut work).unwrap();
    let mut refused = false;
    for _ in 0..4 {
        let result = names.step(&mut work, |name| {
            if name == "." || name == ".." {
                Ok(())
            } else {
                assert_eq!(name, "one");
                Err(io::Error::from(io::ErrorKind::Other))
            }
        });
        if result.is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused);
    let calls = allocator_calls_in(|| {
        assert!(
            names
                .step(&mut work, |name| {
                    assert_eq!(name, "one");
                    Ok(())
                })
                .unwrap()
        );
        let mut ended = false;
        for _ in 0..4 {
            if !names.step(&mut work, |_| Ok(())).unwrap() {
                ended = true;
                break;
            }
        }
        assert!(ended);
        let before = work.read_calls;
        assert!(
            !names
                .step(&mut work, |_| panic!("entry after EOF"))
                .unwrap()
        );
        assert_eq!(work.read_calls, before);
    });
    assert_eq!(calls, 0);
    assert_eq!(work.names, 2);
}
