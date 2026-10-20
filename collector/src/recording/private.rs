//! Private recording artifacts and their durability boundary.
use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

pub struct FileLock {
    file: File,
}

const O_NOFOLLOW: c_int = 0x0100;
const O_RDONLY: c_int = 0;
const O_RDWR: c_int = 2;
const O_CREAT: c_int = 0x0200;
const O_EXCL: c_int = 0x0800;
const O_CLOEXEC: c_int = 0x0100_0000;
pub const PATH_MAX: usize = 1024;
const NAME_MAX: usize = 255;
const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const F_FULLFSYNC: c_int = 51;
const SHORT_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const SHORT_LOCK_RETRY: Duration = Duration::from_millis(20);
const SHORT_LOCK_ATTEMPTS_MAX: usize = 101;

pub fn join_path<'buffer>(
    root: &Path,
    name: &str,
    buffer: &'buffer mut [u8; PATH_MAX],
) -> io::Result<&'buffer Path> {
    let root = root.as_os_str().as_bytes();
    let length = root
        .len()
        .checked_add(1)
        .and_then(|length| length.checked_add(name.len()))
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    if length >= PATH_MAX {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    buffer[..root.len()].copy_from_slice(root);
    buffer[root.len()] = b'/';
    buffer[root.len() + 1..length].copy_from_slice(name.as_bytes());
    Ok(Path::new(std::ffi::OsStr::from_bytes(&buffer[..length])))
}

unsafe extern "C" {
    fn geteuid() -> u32;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn mkdir(path: *const c_char, mode: u32) -> c_int;
    fn flock(file: c_int, operation: c_int) -> c_int;
    fn rename(source: *const c_char, target: *const c_char) -> c_int;
    fn unlink(path: *const c_char) -> c_int;
    fn fcntl(file: c_int, command: c_int, ...) -> c_int;
    fn fgetattrlist(
        file: c_int,
        attributes: *mut Attrlist,
        buffer: *mut c_void,
        size: usize,
        options: u64,
    ) -> c_int;
    fn fsetattrlist(
        file: c_int,
        attributes: *mut Attrlist,
        buffer: *mut c_void,
        size: usize,
        options: u64,
    ) -> c_int;
}

#[repr(C)]
struct Attrlist {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

const _: () = assert!(size_of::<Attrlist>() == 24);
const KAUTH_FILESEC_MAGIC: u32 = 0x012c_c16d;
const KAUTH_FILESEC_NOACL: u32 = u32::MAX;
const KAUTH_ACL_MAX_ENTRIES: usize = 128;
const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_EXTENDED_SECURITY: u32 = 0x0040_0000;
const FSOPT_PACK_INVAL_ATTRS: u64 = 4;
const WORD_BYTES: usize = size_of::<u32>();
const ATTR_LENGTH_WORDS: usize = 1;
const ATTR_REFERENCE_WORDS: usize = 2;
const ATTR_REFERENCE_BYTES: u32 = 8;
const ATTR_REFERENCE_OFFSET_BASE: i64 = 4;
const FILESEC_WORDS: usize = 11;
const FILESEC_BYTES: u32 = 44;
const FILESEC_ACL_ENTRYCOUNT_WORD: usize = 9;
const ACE_WORDS: usize = 6;
const ACL_BUFFER_WORDS: usize = 1024;
const _: () = assert!(3 + FILESEC_WORDS + KAUTH_ACL_MAX_ENTRIES * ACE_WORDS < ACL_BUFFER_WORDS);
const _: () = assert!(ATTR_REFERENCE_WORDS * WORD_BYTES == 8);
const _: () = assert!(FILESEC_WORDS * WORD_BYTES == 44);

fn security_attributes() -> Attrlist {
    Attrlist {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_EXTENDED_SECURITY,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

impl FileLock {
    /// Bounded waiting is reserved for short registry transactions, never lifetime ownership.
    pub fn wait_short(path: &Path) -> io::Result<Self> {
        let start = Instant::now();
        for attempt in 0..SHORT_LOCK_ATTEMPTS_MAX {
            match Self::try_lock(path) {
                Ok(lock) => return Ok(lock),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if start.elapsed() >= SHORT_LOCK_TIMEOUT {
                        return Err(error);
                    }
                    if attempt + 1 == SHORT_LOCK_ATTEMPTS_MAX {
                        return Err(error);
                    }
                    std::thread::sleep(SHORT_LOCK_RETRY);
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("bounded short-lock attempts return on their last attempt");
    }

    pub fn try_lock(path: &Path) -> io::Result<Self> {
        let file = open_private_file(path, true)?;
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { file })
    }

    /// Asks whether another open of `path` holds its lock, without taking the lock, so the
    /// question never keeps a starting collector from taking it.
    pub fn held(path: &Path) -> io::Result<bool> {
        let file = match open_private_file(path, false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut lock = Flock {
            start: 0,
            len: 0,
            pid: 0,
            r#type: F_WRLCK,
            whence: SEEK_SET,
        };

        if unsafe { fcntl(file.as_raw_fd(), F_GETLK, &raw mut lock) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(lock.r#type != F_UNLCK)
    }

    #[must_use]
    pub fn file(&self) -> &File {
        &self.file
    }
}

/// `struct flock` of `<sys/fcntl.h>`, its `l_` prefix dropped. macOS reports a `flock`
/// lock through it too.
#[repr(C)]
struct Flock {
    start: i64,
    len: i64,
    pid: i32,
    r#type: i16,
    whence: i16,
}

const _: () = assert!(size_of::<Flock>() == 24);
const F_GETLK: c_int = 7;
const F_UNLCK: i16 = 2;
const F_WRLCK: i16 = 3;
const SEEK_SET: i16 = 0;

pub fn create_private_dir(path: &Path) -> io::Result<()> {
    match create_directory(path) {
        Ok(()) => {
            let directory = open_directory(path)?;
            clear_acl(&directory)?;
            verify_open(&directory, true)?;
            full_sync(&directory)?;
            let parent = path.parent().ok_or_else(invalid)?;
            full_sync(&open_directory_flags(parent, O_RDONLY)?)?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => verify_directory(path)?,
        Err(error) => return Err(error),
    }

    Ok(())
}

pub fn open_private_file(path: &Path, create: bool) -> io::Result<File> {
    if create {
        match open_file(path, O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600) {
            Ok(file) => {
                clear_acl(&file)?;
                verify_open(&file, false)?;
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }

    let file = open_file(path, O_RDONLY | O_NOFOLLOW, 0)?;
    verify_open(&file, false)?;
    Ok(file)
}

pub fn verify_directory(path: &Path) -> io::Result<()> {
    verify_open(&open_directory(path)?, true)
}

pub fn verify_file(path: &Path) -> io::Result<()> {
    drop(open_private_file(path, false)?);
    Ok(())
}

pub fn full_sync(file: &File) -> io::Result<()> {
    file.sync_all()?;
    if unsafe { fcntl(file.as_raw_fd(), F_FULLFSYNC) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

pub fn sync_directory(path: &Path) -> io::Result<()> {
    full_sync(&open_directory(path)?)
}

/// Replaces bounded private metadata; the caller holds its publication lock.
pub fn atomic_write(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    if name.is_empty() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    if name.bytes().any(|byte| byte == b'/' || byte == 0) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut pending = [0; NAME_MAX];
    let length = name.len() + ".pending".len();
    if length > pending.len() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    pending[..name.len()].copy_from_slice(name.as_bytes());
    pending[name.len()..length].copy_from_slice(b".pending");
    let pending = std::str::from_utf8(&pending[..length])
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut source_path = [0; PATH_MAX];
    let source = join_path(root, pending, &mut source_path)?;
    match verify_file(source) {
        Ok(()) => atomic_write_remove(source)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut file = open_private_file(source, true)?;
    file.write_all(bytes)?;
    full_sync(&file)?;
    let mut target_path = [0; PATH_MAX];
    let target = join_path(root, name, &mut target_path)?;
    let mut source_name = [0; PATH_MAX];
    let mut target_name = [0; PATH_MAX];
    let source = path_name(source, &mut source_name)?;
    let target = path_name(target, &mut target_name)?;
    if unsafe { rename(source.as_ptr(), target.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    sync_directory(root)
}

fn atomic_write_remove(path: &Path) -> io::Result<()> {
    let mut buffer = [0; PATH_MAX];
    let name = path_name(path, &mut buffer)?;
    if unsafe { unlink(name.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn make_file_private(path: &Path) -> io::Result<()> {
    let file = open_file(path, O_RDONLY | O_NOFOLLOW, 0)?;
    let metadata = file.metadata()?;
    if metadata.uid() != unsafe { geteuid() } {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    if !metadata.is_file() {
        return Err(invalid());
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    clear_acl(&file)?;
    verify_open(&file, false)
}

pub(super) fn open_directory(path: &Path) -> io::Result<File> {
    open_directory_flags(path, O_RDONLY | O_NOFOLLOW)
}

fn open_directory_flags(path: &Path, flags: c_int) -> io::Result<File> {
    let file = open_file(path, flags, 0)?;
    if !file.metadata()?.is_dir() {
        return Err(invalid());
    }
    Ok(file)
}

fn open_file(path: &Path, flags: c_int, mode: u32) -> io::Result<File> {
    let mut buffer = [0; PATH_MAX];
    let name = path_name(path, &mut buffer)?;
    let descriptor = unsafe { open(name.as_ptr(), flags | O_CLOEXEC, mode) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }

    let file = unsafe { File::from_raw_fd(descriptor) };

    Ok(file)
}

fn create_directory(path: &Path) -> io::Result<()> {
    let mut buffer = [0; PATH_MAX];
    let name = path_name(path, &mut buffer)?;
    if unsafe { mkdir(name.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn path_name<'buffer>(
    path: &Path,
    buffer: &'buffer mut [u8; PATH_MAX],
) -> io::Result<&'buffer CStr> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= PATH_MAX {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    buffer[..bytes.len()].copy_from_slice(bytes);
    buffer[bytes.len()] = 0;
    CStr::from_bytes_with_nul(&buffer[..=bytes.len()])
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn verify_open(file: &File, directory: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.uid() != unsafe { geteuid() } {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    if metadata.is_dir() != directory {
        return Err(invalid());
    }
    if !directory {
        if !metadata.is_file() {
            return Err(invalid());
        }
    }
    let expected = if directory { 0o700 } else { 0o600 };
    if metadata.mode() & 0o7777 != expected {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    if read_acl_entries(file)? != 0 {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(())
}

fn clear_acl(file: &File) -> io::Result<()> {
    let mut attributes = security_attributes();
    let mut buffer = [0_u32; ATTR_REFERENCE_WORDS + FILESEC_WORDS];
    buffer[0] = ATTR_REFERENCE_BYTES;
    buffer[1] = FILESEC_BYTES;
    buffer[ATTR_REFERENCE_WORDS] = KAUTH_FILESEC_MAGIC;
    buffer[ATTR_REFERENCE_WORDS + FILESEC_ACL_ENTRYCOUNT_WORD] = KAUTH_FILESEC_NOACL;
    if unsafe {
        fsetattrlist(
            file.as_raw_fd(),
            &raw mut attributes,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_acl_entries(file: &File) -> io::Result<u32> {
    let mut attributes = security_attributes();
    let mut buffer = [0_u32; ACL_BUFFER_WORDS];
    if unsafe {
        fgetattrlist(
            file.as_raw_fd(),
            &raw mut attributes,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer),
            FSOPT_PACK_INVAL_ATTRS,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }

    let bytes = usize::try_from(buffer[0]).map_err(|_| invalid())?;
    if bytes > size_of_val(&buffer) {
        return Err(invalid());
    }
    let reference_end = (ATTR_LENGTH_WORDS + ATTR_REFERENCE_WORDS) * WORD_BYTES;
    if bytes < reference_end {
        return Err(invalid());
    }
    let length = usize::try_from(buffer[2]).map_err(|_| invalid())?;
    if length == 0 {
        return Ok(0);
    }
    let offset = i32::from_ne_bytes(buffer[1].to_ne_bytes());
    let start =
        usize::try_from(ATTR_REFERENCE_OFFSET_BASE + i64::from(offset)).map_err(|_| invalid())?;
    if start < reference_end {
        return Err(invalid());
    }
    if start % WORD_BYTES != 0 {
        return Err(invalid());
    }
    let end = start.checked_add(length).ok_or_else(invalid)?;
    if end > bytes {
        return Err(invalid());
    }
    if length < FILESEC_WORDS * WORD_BYTES {
        return Err(invalid());
    }
    let security = &buffer[start / WORD_BYTES..end / WORD_BYTES];
    if security[0] != KAUTH_FILESEC_MAGIC {
        return Err(invalid());
    }
    let entries = security[FILESEC_ACL_ENTRYCOUNT_WORD];
    if entries == KAUTH_FILESEC_NOACL {
        return Ok(0);
    }
    if usize::try_from(entries).map_err(|_| invalid())? > KAUTH_ACL_MAX_ENTRIES {
        return Err(invalid());
    }
    Ok(entries)
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Command;

    #[test]
    fn a_short_lock_can_wait_for_its_owner_without_allocating() {
        // Goal: bounded registry contention stays allocation-free in shared library code.
        // Method: release a held native lock after 100 ms while counting the waiting caller.
        let root = std::env::temp_dir().join(format!(
            "flightdeck-short-lock-fixed-{}",
            std::process::id()
        ));
        create_private_dir(&root).unwrap();
        let path = root.join("registration.lock");
        let lock = FileLock::try_lock(&path).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(lock);
        });

        let calls = allocator_calls_in(|| {
            drop(FileLock::wait_short(&path).unwrap());
        });

        release.join().unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(0, calls);
    }

    #[test]
    fn structural_and_privacy_refusals_keep_their_kinds_without_allocating() {
        // Goal: malformed artifact kinds and denied private access stay distinguishable.
        // Method: count refusals of a directory as a file and of a world-readable file.
        let root = std::env::temp_dir().join(format!(
            "flightdeck-private-refusal-kinds-{}",
            std::process::id()
        ));
        create_private_dir(&root).unwrap();
        let path = root.join("file");
        let file = open_private_file(&path, true).unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .unwrap();

        let calls = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::InvalidData,
                verify_file(&root).unwrap_err().kind()
            );
            assert_eq!(
                io::ErrorKind::InvalidData,
                verify_directory(&path).unwrap_err().kind()
            );
            assert_eq!(
                io::ErrorKind::PermissionDenied,
                verify_file(&path).unwrap_err().kind()
            );
        });

        fs::remove_dir_all(root).unwrap();
        assert_eq!(0, calls);
    }

    #[test]
    fn replaces_private_metadata_without_allocating() {
        // Goal: post-startup retirement updates use only bounded caller and syscall storage.
        // Method: atomically write two versions while counting allocator calls.
        let root =
            std::env::temp_dir().join(format!("flightdeck-atomic-fixed-{}", std::process::id()));
        create_private_dir(&root).unwrap();

        let calls = allocator_calls_in(|| {
            atomic_write(&root, "runs", b"first\n").unwrap();
            atomic_write(&root, "runs", b"second\n").unwrap();
        });

        assert_eq!(b"second\n", fs::read(root.join("runs")).unwrap().as_slice());
        fs::remove_dir_all(root).unwrap();
        assert_eq!(0, calls);
    }

    #[test]
    fn fixed_security_buffers_refuse_and_clear_a_real_extended_acl() {
        // Goal: privacy verification and ACL removal require no allocated ACL object.
        // Method: install a real ACL, inspect and clear it using fixed syscall storage.
        let path =
            std::env::temp_dir().join(format!("flightdeck-fixed-acl-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let status = Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(1, read_acl_entries(&file).unwrap());
        assert!(verify_file(&path).is_err());

        let calls = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::PermissionDenied,
                verify_file(&path).unwrap_err().kind()
            );
            clear_acl(&file).unwrap();
            assert_eq!(0, read_acl_entries(&file).unwrap());
            verify_file(&path).unwrap();
        });

        assert_eq!(0, calls);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn private_open_allocates_nothing_for_a_long_legal_path() {
        // Goal: a user's long home path does not add hidden CString allocations per poll.
        // Method: open an 800-byte path while counting Rust allocator calls.
        let root =
            std::env::temp_dir().join(format!("flightdeck-long-private-{}", std::process::id()));
        let mut directory = root.clone();
        for _ in 0..7 {
            directory.push("x".repeat(100));
        }
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("runs");
        drop(open_private_file(&path, true).unwrap());

        let calls = allocator_calls_in(|| {
            drop(open_private_file(&path, false).unwrap());
        });

        fs::remove_dir_all(root).unwrap();
        assert_eq!(0, calls);
    }

    #[test]
    fn syncs_a_new_private_child_through_a_parent_alias_but_refuses_a_final_alias() {
        // Goal: /tmp-style parent aliases work without accepting a symlink as the artifact.
        // Method: create through a parent alias, then reject an alias of that final child.
        let root = std::env::temp_dir().join(format!(
            "flightdeck-private-parent-alias-{}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        let alias = root.join("parent-alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let child = alias.join("child");
        let mut result = Ok(());
        let created_calls = allocator_calls_in(|| {
            result = create_private_dir(&child);
        });
        if result.is_err() {
            fs::remove_dir_all(&root).unwrap();
        }
        result.unwrap();
        let final_alias = root.join("final-alias");
        std::os::unix::fs::symlink(real.join("child"), &final_alias).unwrap();

        let calls = allocator_calls_in(|| {
            assert_eq!(
                Some(62),
                verify_directory(&final_alias).unwrap_err().raw_os_error()
            );
        });

        fs::remove_dir_all(root).unwrap();
        assert_eq!(0, created_calls);
        assert_eq!(0, calls);
    }
}
