//! Private recording artifacts and their durability boundary.
use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

pub struct FileLock {
    file: File,
}

const O_NOFOLLOW: c_int = 0x0100;
const O_RDONLY: c_int = 0;
const O_RDWR: c_int = 2;
const O_CREAT: c_int = 0x0200;
const O_EXCL: c_int = 0x0800;
const O_CLOEXEC: c_int = 0x0100_0000;
const PATH_MAX: usize = 1024;
const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const F_FULLFSYNC: c_int = 51;

unsafe extern "C" {
    fn geteuid() -> u32;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn mkdir(path: *const c_char, mode: u32) -> c_int;
    fn flock(file: c_int, operation: c_int) -> c_int;
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
const FILESEC_WORDS: usize = 11;
const ACE_WORDS: usize = 6;
const ACL_BUFFER_WORDS: usize = 1024;
const _: () = assert!(3 + FILESEC_WORDS + KAUTH_ACL_MAX_ENTRIES * ACE_WORDS < ACL_BUFFER_WORDS);

fn security_attributes() -> Attrlist {
    Attrlist {
        bitmapcount: 5,
        reserved: 0,
        commonattr: 0x0040_0000,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

impl FileLock {
    pub fn try_lock(path: &Path) -> io::Result<Self> {
        let file = open_private_file(path, true)?;
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { file })
    }

    #[must_use]
    pub fn file(&self) -> &File {
        &self.file
    }
}

pub fn create_private_dir(path: &Path) -> io::Result<()> {
    match create_directory(path) {
        Ok(()) => {
            let directory = open_directory(path)?;
            clear_acl(&directory)?;
            verify_open(&directory, true)?;
            full_sync(&directory)?;
            let parent = path.parent().ok_or_else(|| invalid("directory parent"))?;
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

pub fn make_file_private(path: &Path) -> io::Result<()> {
    let file = open_file(path, O_RDONLY | O_NOFOLLOW, 0)?;
    let metadata = file.metadata()?;
    if metadata.uid() != unsafe { geteuid() } {
        return Err(invalid("foreign owner"));
    }
    if !metadata.is_file() {
        return Err(invalid("regular file required"));
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
        return Err(invalid("directory required"));
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
        return Err(invalid("foreign owner"));
    }
    if metadata.is_dir() != directory {
        return Err(invalid("artifact kind"));
    }
    if !directory {
        if !metadata.is_file() {
            return Err(invalid("regular file required"));
        }
    }
    let expected = if directory { 0o700 } else { 0o600 };
    if metadata.mode() & 0o7777 != expected {
        return Err(invalid("private mode required"));
    }
    if read_acl_entries(file)? != 0 {
        return Err(invalid("extended ACL present"));
    }
    Ok(())
}

fn clear_acl(file: &File) -> io::Result<()> {
    let mut attributes = security_attributes();
    let mut buffer = [0_u32; 2 + FILESEC_WORDS];
    buffer[0] = 8;
    buffer[1] = 44;
    buffer[2] = KAUTH_FILESEC_MAGIC;
    buffer[11] = KAUTH_FILESEC_NOACL;
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
            4,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }

    let bytes = usize::try_from(buffer[0]).map_err(|_| invalid("security length"))?;
    if bytes > size_of_val(&buffer) {
        return Err(invalid("security too large"));
    }
    if bytes < 12 {
        return Err(invalid("security too short"));
    }
    let length = usize::try_from(buffer[2]).map_err(|_| invalid("security length"))?;
    if length == 0 {
        return Ok(0);
    }
    let offset = i32::from_ne_bytes(buffer[1].to_ne_bytes());
    let start =
        usize::try_from(4_i64 + i64::from(offset)).map_err(|_| invalid("security offset"))?;
    if start < 12 {
        return Err(invalid("security offset"));
    }
    if start % 4 != 0 {
        return Err(invalid("security alignment"));
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| invalid("security range"))?;
    if end > bytes {
        return Err(invalid("security range"));
    }
    if length < 44 {
        return Err(invalid("security header"));
    }
    let security = &buffer[start / 4..end / 4];
    if security[0] != KAUTH_FILESEC_MAGIC {
        return Err(invalid("security magic"));
    }
    let entries = security[9];
    if entries == KAUTH_FILESEC_NOACL {
        return Ok(0);
    }
    if entries > 128 {
        return Err(invalid("security count"));
    }
    Ok(entries)
}

fn invalid(_message: &'static str) -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Command;

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
