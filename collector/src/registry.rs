//! Reading I/O Registry entries and their CoreFoundation properties.

use std::ffi::{CStr, CString, c_char, c_ulong, c_void};
use std::io;

/// An I/O Registry entry this module owns a reference to.
pub(crate) struct Entry(u32);

impl Entry {
    /// The first entry named `name`, or an error naming `what` where the registry has none.
    pub(crate) fn named(name: &CStr, what: &str) -> io::Result<Self> {
        Self::matching(unsafe { IOServiceNameMatching(name.as_ptr()) }, what)
    }

    /// The first entry of class `class`, or an error naming `what` where the registry has none.
    pub(crate) fn of_class(class: &CStr, what: &str) -> io::Result<Self> {
        Self::matching(unsafe { IOServiceMatching(class.as_ptr()) }, what)
    }

    fn matching(matching: *mut c_void, what: &str) -> io::Result<Self> {
        if matching.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "the registry returned no matching dictionary",
            ));
        }

        // IOServiceGetMatchingService consumes the matching dictionary.
        let entry = Self(unsafe { IOServiceGetMatchingService(0, matching) });

        if entry.0 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("the I/O Registry has no {what}"),
            ));
        }
        Ok(entry)
    }

    /// The property `key`, or none where the entry has no such property.
    fn property(&self, key: &str) -> io::Result<Option<Owned>> {
        let name = Owned::string(key)?;
        let value =
            Owned(unsafe { IORegistryEntryCreateCFProperty(self.0, name.0, std::ptr::null(), 0) });

        if value.0.is_null() {
            return Ok(None);
        }
        Ok(Some(value))
    }

    /// The bytes of the data property `key`, or none where the entry has no such property or
    /// it holds something other than data.
    pub(crate) fn data(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        let Some(value) = self.property(key)? else {
            return Ok(None);
        };
        if unsafe { CFGetTypeID(value.0) } != unsafe { CFDataGetTypeID() } {
            return Ok(None);
        }
        let length = usize::try_from(unsafe { CFDataGetLength(value.0) })
            .map_err(|error| io::Error::other(error.to_string()))?;
        if length == 0 {
            return Ok(Some(Vec::new()));
        }
        let pointer = unsafe { CFDataGetBytePtr(value.0) };
        if pointer.is_null() {
            return Ok(None);
        }
        // CFData holds `length` bytes at `pointer` for as long as `value` keeps it alive.
        Ok(Some(
            unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec(),
        ))
    }

    /// The whole-number values the dictionary property `key` holds under each of `names`,
    /// each none where the dictionary lacks it, holds no number there, or holds a negative
    /// one. The whole array is none where the entry has no such dictionary.
    pub(crate) fn dictionary_counts<const N: usize>(
        &self,
        key: &str,
        names: [&str; N],
    ) -> io::Result<Option<[Option<u64>; N]>> {
        let Some(dictionary) = self.property(key)? else {
            return Ok(None);
        };
        if unsafe { CFGetTypeID(dictionary.0) } != unsafe { CFDictionaryGetTypeID() } {
            return Ok(None);
        }
        let mut counts = [None; N];
        for (name, count) in names.into_iter().zip(counts.iter_mut()) {
            let name = Owned::string(name)?;
            // The dictionary keeps the value alive, and the value is not ours to release.
            let value = unsafe { CFDictionaryGetValue(dictionary.0, name.0) };
            if value.is_null() {
                continue;
            }
            if unsafe { CFGetTypeID(value) } != unsafe { CFNumberGetTypeID() } {
                continue;
            }
            let mut number: i64 = 0;
            let exact = unsafe {
                CFNumberGetValue(value, NUMBER_SINT64, std::ptr::from_mut(&mut number).cast())
            };
            if exact != 0 {
                if let Ok(number) = u64::try_from(number) {
                    *count = Some(number);
                }
            }
        }
        Ok(Some(counts))
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        if self.0 != 0 {
            let status = unsafe { IOObjectRelease(self.0) };
            if status != 0 {
                eprintln!("collector: registry entry release failed: {status:#x}");
            }
        }
    }
}

/// A CoreFoundation object this module owns a reference to.
struct Owned(*const c_void);

impl Owned {
    fn string(value: &str) -> io::Result<Self> {
        let value = CString::new(value).map_err(|error| io::Error::other(error.to_string()))?;
        let string = Self(unsafe {
            CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), UTF8_ENCODING)
        });

        if string.0.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "CFStringCreateWithCString returned no string",
            ));
        }
        Ok(string)
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

const UTF8_ENCODING: u32 = 0x0800_0100;
const NUMBER_SINT64: isize = 4;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceNameMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: *const c_void,
        allocator: *const c_void,
        options: u32,
    ) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        value: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(value: *const c_void);
    fn CFGetTypeID(value: *const c_void) -> c_ulong;
    fn CFDataGetTypeID() -> c_ulong;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFDictionaryGetTypeID() -> c_ulong;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetTypeID() -> c_ulong;
    fn CFNumberGetValue(number: *const c_void, kind: isize, value: *mut c_void) -> u8;
}
