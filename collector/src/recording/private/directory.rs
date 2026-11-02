//! Single-component I/O anchored to an admitted private directory inode.

use super::{
    NAME_MAX, O_CLOEXEC, O_CREAT, O_EXCL, O_NOFOLLOW, O_RDONLY, O_RDWR, clear_acl, full_sync,
    open_directory_flags, verify_open,
};
use std::ffi::{CStr, c_char, c_int};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;

/// Owns one directory descriptor. Each open adds at most one descriptor, including
/// validation failures; callers budget retained handles within P02's eight namespace slots.
/// No descendant operation reconstructs an absolute path or follows a final symlink.
pub struct PrivateDirectory {
    file: File,
}

const O_DIRECTORY: c_int = 0x0010_0000;
// A substituted FIFO must reach type validation without blocking its reader.
const O_NONBLOCK: c_int = 4;

unsafe extern "C" {
    fn mkdirat(directory: c_int, path: *const c_char, mode: u32) -> c_int;
    fn openat(directory: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn renameat(
        source_directory: c_int,
        source: *const c_char,
        target_directory: c_int,
        target: *const c_char,
    ) -> c_int;
    fn unlinkat(directory: c_int, path: *const c_char, flags: c_int) -> c_int;
}

impl PrivateDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = open_directory_flags(path, O_RDONLY | O_NOFOLLOW | O_DIRECTORY)?;
        verify_open(&file, true)?;

        Ok(Self { file })
    }

    pub fn open_directory(&self, name: &str) -> io::Result<Self> {
        let file = self.open_component(name, O_RDONLY | O_DIRECTORY, 0)?;
        verify_open(&file, true)?;

        Ok(Self { file })
    }

    /// Creates or validates one child. `AlreadyExists` opens and validates without
    /// repairing permissions or ACLs; every other mkdir error is returned.
    /// New children are synced before their parent. A sync failure may leave a
    /// visible child; callers must not infer absence from an error.
    pub fn create_directory(&self, name: &str) -> io::Result<Self> {
        let mut buffer = [0; NAME_MAX + 1];
        let component = component_name(name, &mut buffer)?;
        if unsafe { mkdirat(self.file.as_raw_fd(), component.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                return self.open_directory(name);
            }
            return Err(error);
        }

        let file = self.open_component(name, O_RDONLY | O_DIRECTORY, 0)?;
        clear_acl(&file)?;
        verify_open(&file, true)?;
        full_sync(&file)?;
        self.full_sync()?;

        Ok(Self { file })
    }

    pub fn open_file(&self, name: &str) -> io::Result<File> {
        let file = self.open_component(name, O_RDONLY | O_NONBLOCK, 0)?;
        verify_open(&file, false)?;

        Ok(file)
    }

    /// Creates exclusively. An existing artifact is never reopened for mutation.
    pub fn create_file(&self, name: &str) -> io::Result<File> {
        let file = self.open_component(name, O_RDWR | O_CREAT | O_EXCL, 0o600)?;
        if let Err(error) = clear_acl(&file).and_then(|()| verify_open(&file, false)) {
            self.remove_file(name)?;
            self.full_sync()?;

            return Err(error);
        }

        Ok(file)
    }

    /// Renames within this directory; the publication owner supplies validated contents.
    pub fn rename(&self, source: &str, target: &str) -> io::Result<()> {
        let mut source_buffer = [0; NAME_MAX + 1];
        let source = component_name(source, &mut source_buffer)?;
        let mut target_buffer = [0; NAME_MAX + 1];
        let target = component_name(target, &mut target_buffer)?;
        let descriptor = self.file.as_raw_fd();

        if unsafe { renameat(descriptor, source.as_ptr(), descriptor, target.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        let mut buffer = [0; NAME_MAX + 1];
        let name = component_name(name, &mut buffer)?;

        if unsafe { unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    pub fn full_sync(&self) -> io::Result<()> {
        full_sync(&self.file)
    }

    fn open_component(&self, name: &str, flags: c_int, mode: u32) -> io::Result<File> {
        let mut buffer = [0; NAME_MAX + 1];
        let name = component_name(name, &mut buffer)?;

        let descriptor = unsafe {
            openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | O_NOFOLLOW | O_CLOEXEC,
                mode,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

pub(super) fn component_name<'buffer>(
    name: &str,
    buffer: &'buffer mut [u8; NAME_MAX + 1],
) -> io::Result<&'buffer CStr> {
    if name.is_empty() || name == "." || name == ".." || name.len() > NAME_MAX {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }

    if name.bytes().any(|byte| byte == b'/' || byte == 0) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }

    buffer[..name.len()].copy_from_slice(name.as_bytes());
    buffer[name.len()] = 0;

    CStr::from_bytes_with_nul(&buffer[..=name.len()])
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(test)]
#[path = "descriptor_tests.rs"]
mod tests;

unsafe extern "C" {
    fn __getdirentries64(
        descriptor: c_int,
        buffer: *mut c_char,
        size: usize,
        base: *mut i64,
    ) -> isize;
}

impl PrivateDirectory {
    /// Bounds names, dot entries and native calls without allocating a libc DIR.
    /// One independent directory descriptor and a fixed 1048-byte name buffer
    /// are retained only during this walk. No history collection is constructed.
    pub fn visit_names(
        &self,
        limit: usize,
        mut visit: impl FnMut(&str) -> io::Result<()>,
    ) -> io::Result<()> {
        let descriptor = unsafe {
            openat(
                self.file.as_raw_fd(),
                c".".as_ptr(),
                O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let mut buffer = [0_u8; 1048];
        let mut base = 0_i64;
        let mut count = 0;
        let mut dots = 0;
        let bound = limit
            .checked_add(3)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        for _ in 0..=bound {
            let received = unsafe {
                __getdirentries64(
                    file.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &raw mut base,
                )
            };
            if received < 0 {
                return Err(io::Error::last_os_error());
            }
            if received == 0 {
                return Ok(());
            }
            let length = usize::try_from(received)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
            if length > buffer.len() {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            visit_names_batch(&buffer[..length], limit, &mut count, &mut dots, &mut visit)?;
        }
        Err(io::Error::from(io::ErrorKind::InvalidData))
    }
}

fn visit_names_batch(
    bytes: &[u8],
    limit: usize,
    count: &mut usize,
    dots: &mut usize,
    visit: &mut impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    let mut offset = 0;
    // Each Darwin dirent64 has at least its 21-byte header and a name terminator.
    for _ in 0..48 {
        if offset == bytes.len() {
            return Ok(());
        }
        if bytes.len() - offset < 22 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let record = &bytes[offset..];
        let record_length = usize::from(u16::from_ne_bytes([record[16], record[17]]));
        let name_length = usize::from(u16::from_ne_bytes([record[18], record[19]]));
        if record_length > record.len()
            || record_length < 22
            || name_length == 0
            || name_length > NAME_MAX
            || 21 + name_length >= record_length
        {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        if record[21 + name_length] != 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let name = std::str::from_utf8(&record[21..21 + name_length])
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        if name == "." || name == ".." {
            *dots += 1;
            if *dots > 2 {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
        } else {
            *count += 1;
            if *count > limit {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            visit(name)?;
        }
        offset += record_length;
    }
    Err(io::Error::from(io::ErrorKind::InvalidData))
}
