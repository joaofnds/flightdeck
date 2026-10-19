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

    /// Calls `visit` with each child of this entry in the service plane that conforms to
    /// class `class`, at most `limit` of them, and returns how many it visited. User clients
    /// are children of the service they opened and are not matched as services themselves.
    pub(crate) fn each_child_of_class(
        &self,
        class: &CStr,
        limit: usize,
        mut visit: impl FnMut(&Self) -> io::Result<()>,
    ) -> io::Result<usize> {
        let mut iterator = 0_u32;
        let status = unsafe {
            IORegistryEntryGetChildIterator(self.0, c"IOService".as_ptr(), &raw mut iterator)
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "IORegistryEntryGetChildIterator failed: {status:#x}"
            )));
        }
        let iterator = Self(iterator);

        let mut visited = 0;
        while visited < limit {
            let child = Self(unsafe { IOIteratorNext(iterator.0) });
            if child.0 == 0 {
                break;
            }
            if unsafe { IOObjectConformsTo(child.0, class.as_ptr()) } == 0 {
                continue;
            }
            visit(&child)?;
            visited += 1;
        }
        Ok(visited)
    }

    /// The property `key`, or none where the entry has no such property.
    fn property(&self, key: &Key) -> Option<Owned> {
        let value = Owned(unsafe {
            IORegistryEntryCreateCFProperty(self.0, (key.0).0, std::ptr::null(), 0)
        });

        if value.0.is_null() {
            return None;
        }
        Some(value)
    }

    /// The string property `key` written into `text`, or false where the entry has no such
    /// property, it holds something other than a string, or it does not fit `text`.
    pub(crate) fn string(&self, key: &Key, text: &mut [u8]) -> bool {
        let Some(value) = self.property(key) else {
            return false;
        };
        if unsafe { CFGetTypeID(value.0) } != unsafe { CFStringGetTypeID() } {
            return false;
        }
        let Ok(length) = isize::try_from(text.len()) else {
            return false;
        };
        let written =
            unsafe { CFStringGetCString(value.0, text.as_mut_ptr().cast(), length, UTF8_ENCODING) };
        written != 0
    }

    /// The sum of the whole numbers that each dictionary of the array property `key` holds
    /// under `name`, or none where the entry has no such array or the sum overflows. A
    /// dictionary without a whole number under `name` adds nothing.
    pub(crate) fn array_sum(&self, key: &Key, name: &Key) -> Option<u64> {
        let array = self.property(key)?;
        if unsafe { CFGetTypeID(array.0) } != unsafe { CFArrayGetTypeID() } {
            return None;
        }
        let count = unsafe { CFArrayGetCount(array.0) };
        let mut sum = 0_u64;
        for index in 0..count {
            // The array keeps each element alive, and none is ours to release.
            let dictionary = unsafe { CFArrayGetValueAtIndex(array.0, index) };
            if let Some(value) = dictionary_count(dictionary, name) {
                sum = sum.checked_add(value)?;
            }
        }
        Some(sum)
    }

    /// The bytes of the data property `key`, or none where the entry has no such property or
    /// it holds something other than data.
    pub(crate) fn data(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        let Some(value) = self.property(&Key::new(key)?) else {
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

    /// The whole number the property `key` holds, or none where the entry has no such
    /// property, it holds no number, or it holds a negative one.
    pub(crate) fn count(&self, key: &str) -> io::Result<Option<u64>> {
        let Some(value) = self.property(&Key::new(key)?) else {
            return Ok(None);
        };
        Ok(number_count(value.0))
    }

    /// The whole-number values the dictionary property `key` holds under each of `names`,
    /// each none where the dictionary lacks it, holds no number there, or holds a negative
    /// one. The whole array is none where the entry has no such dictionary.
    pub(crate) fn dictionary_counts<const N: usize>(
        &self,
        key: &str,
        names: [&str; N],
    ) -> io::Result<Option<[Option<u64>; N]>> {
        let Some(dictionary) = self.property(&Key::new(key)?) else {
            return Ok(None);
        };
        if unsafe { CFGetTypeID(dictionary.0) } != unsafe { CFDictionaryGetTypeID() } {
            return Ok(None);
        }
        let mut counts = [None; N];
        for (name, count) in names.into_iter().zip(counts.iter_mut()) {
            *count = dictionary_count(dictionary.0, &Key::new(name)?);
        }
        Ok(Some(counts))
    }
}

/// The whole number `dictionary` holds under `name`, or none where it is not a dictionary,
/// lacks the name, holds no number there, or holds a negative one.
fn dictionary_count(dictionary: *const c_void, name: &Key) -> Option<u64> {
    if dictionary.is_null() {
        return None;
    }
    if unsafe { CFGetTypeID(dictionary) } != unsafe { CFDictionaryGetTypeID() } {
        return None;
    }
    // The dictionary keeps the value alive, and the value is not ours to release.
    let value = unsafe { CFDictionaryGetValue(dictionary, (name.0).0) };
    if value.is_null() {
        return None;
    }
    number_count(value)
}

/// The whole number `value` holds, or none where it is no number or a negative one.
fn number_count(value: *const c_void) -> Option<u64> {
    if unsafe { CFGetTypeID(value) } != unsafe { CFNumberGetTypeID() } {
        return None;
    }
    let mut number: i64 = 0;
    let exact =
        unsafe { CFNumberGetValue(value, NUMBER_SINT64, std::ptr::from_mut(&mut number).cast()) };
    if exact == 0 {
        return None;
    }
    u64::try_from(number).ok()
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

/// A property name, made once so a reader that runs each second does not remake it.
pub(crate) struct Key(Owned);

impl Key {
    pub(crate) fn new(name: &str) -> io::Result<Self> {
        Ok(Self(Owned::string(name)?))
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
    fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, iterator: *mut u32)
    -> i32;
    fn IOObjectConformsTo(object: u32, class: *const c_char) -> u32;
    fn IOIteratorNext(iterator: u32) -> u32;
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
    fn CFStringGetTypeID() -> c_ulong;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFArrayGetTypeID() -> c_ulong;
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, kind: isize, value: *mut c_void) -> u8;
}
