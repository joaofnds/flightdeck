use std::ffi::{CStr, c_char, c_ulong, c_void};
use std::ptr;

use super::{DISKS_MAX, DiskReading, DiskSnapshot, DiskSource, Error, ScanState};

pub(super) struct Source<R: Registry = NativeRegistry>(Box<Storage<R>>);

struct Storage<R: Registry> {
    registry: R,
    keys: [Owned; KEY_NAMES.len()],
    services: [Option<R::Service>; DISKS_MAX],
}

pub(super) trait Registry {
    type Service;
    type Cursor;

    fn cursor(&self) -> Result<Self::Cursor, Error>;
    fn next(cursor: &mut Self::Cursor) -> Option<Self::Service>;
    fn cursor_valid(cursor: &Self::Cursor) -> bool;
    fn parent(service: &Self::Service) -> Result<Self::Service, Error>;
    fn conforms(service: &Self::Service, class: &CStr) -> bool;
    fn identity(service: &Self::Service) -> Result<u64, Error>;
    fn properties(&self, service: &Self::Service) -> Result<Owned, Error>;
}

pub(super) struct NativeRegistry {
    matching: Owned,
}

const CANDIDATES_MAX: usize = 256;
const STATISTICS: usize = 0;
const PROTOCOL: usize = 1;
const INTERCONNECT: usize = 2;
const NAME: usize = 3;
const FIRST_COUNTER: usize = 4;
const KEY_NAMES: [&CStr; 12] = [
    c"Statistics",
    c"Protocol Characteristics",
    c"Physical Interconnect",
    c"BSD Name",
    c"Bytes (Read)",
    c"Bytes (Write)",
    c"Operations (Read)",
    c"Operations (Write)",
    c"Errors (Read)",
    c"Errors (Write)",
    c"Total Time (Read)",
    c"Total Time (Write)",
];
const PHYSICAL_BUSES: [&CStr; 12] = [
    c"ATA",
    c"SATA",
    c"SAS",
    c"ATAPI",
    c"USB",
    c"FireWire",
    c"Secure Digital",
    c"SCSI Parallel Interface",
    c"Fibre Channel Interface",
    c"PCI",
    c"PCI-Express",
    c"Apple Fabric",
];

impl Source {
    pub(super) fn open() -> Result<Self, Error> {
        Self::from_registry(NativeRegistry::open()?)
    }
}

impl<R: Registry> Source<R> {
    fn from_registry(registry: R) -> Result<Self, Error> {
        let mut source = Self(Box::new(Storage {
            registry,
            keys: std::array::from_fn(|_| Owned(ptr::null())),
            services: std::array::from_fn(|_| None),
        }));
        for (index, name) in KEY_NAMES.iter().enumerate() {
            let key = unsafe { CFStringCreateWithCString(ptr::null(), name.as_ptr(), 0x0800_0100) };

            source.0.keys[index] = Owned(key);
            if key.is_null() {
                return Err(Error::Allocation);
            }
        }

        Ok(source)
    }

    fn read_into_scan(&mut self, snapshot: &mut DiskSnapshot) -> Result<(), Error> {
        let mut cursor = self.0.registry.cursor()?;

        snapshot.state = ScanState::Complete;
        for _ in 0..CANDIDATES_MAX {
            let Some(media) = R::next(&mut cursor) else {
                if !R::cursor_valid(&cursor) {
                    return Err(Error::InvalidSnapshot);
                }

                return Ok(());
            };

            if let Err(error) = self.read_into_scan_media(&media, snapshot) {
                if snapshot.count < DISKS_MAX {
                    snapshot.records[snapshot.count] = DiskReading::default();
                }

                eprintln!("collector: disk candidate failed: {error:?}");
                if error == Error::Capacity {
                    snapshot.state = ScanState::Exhausted;
                } else {
                    if snapshot.state != ScanState::Exhausted {
                        snapshot.state = ScanState::Partial;
                    }
                }
            }
        }

        snapshot.state = ScanState::Exhausted;

        Ok(())
    }

    fn read_into_scan_media(
        &mut self,
        media: &R::Service,
        snapshot: &mut DiskSnapshot,
    ) -> Result<(), Error> {
        let driver = R::parent(media)?;

        if !R::conforms(&driver, c"IOBlockStorageDriver") {
            return Ok(());
        }

        let provider = R::parent(&driver)?;

        if !R::conforms(&provider, c"IOBlockStorageDevice") {
            return Err(Error::InvalidSnapshot);
        }

        if !self.read_into_scan_media_physical(&provider)? {
            return Ok(());
        }

        let driver_id = R::identity(&driver)?;
        let media_id = R::identity(media)?;

        for previous in &snapshot.records[..snapshot.count] {
            if previous.driver_id == driver_id {
                return Err(Error::InvalidSnapshot);
            }

            if previous.media_id == media_id {
                return Err(Error::InvalidSnapshot);
            }
        }

        if snapshot.count == DISKS_MAX {
            snapshot.state = ScanState::Exhausted;
            return Ok(());
        }

        let record = &mut snapshot.records[snapshot.count];
        record.driver_id = driver_id;
        record.media_id = media_id;

        self.read_into_scan_media_name(media, record)?;
        if let Err(error) = self.read_into_scan_media_counters(&driver, record) {
            eprintln!(
                "collector: disk {} statistics failed: {error:?}",
                record.driver_id
            );
            record.failed = true;
            if snapshot.state != ScanState::Exhausted {
                snapshot.state = ScanState::Partial;
            }
        }

        self.0.services[snapshot.count] = Some(driver);
        snapshot.count += 1;

        Ok(())
    }

    fn read_into_scan_media_physical(&self, provider: &R::Service) -> Result<bool, Error> {
        let properties = self.0.registry.properties(provider)?;

        let protocol = dictionary(self.value(properties.0, PROTOCOL))?;
        let interconnect = self.value(protocol, INTERCONNECT);
        let mut bytes = [0_i8; 64];
        string_into(interconnect, &mut bytes)?;
        let name = unsafe { CStr::from_ptr(bytes.as_ptr()) };

        read_into_scan_media_physical_bus(name)
    }

    fn read_into_scan_media_name(
        &self,
        media: &R::Service,
        record: &mut DiskReading,
    ) -> Result<(), Error> {
        let properties = self.0.registry.properties(media)?;

        let mut bytes = [0_i8; super::NAME_BYTES];
        string_into(self.value(properties.0, NAME), &mut bytes)?;
        for (target, source) in record.name.iter_mut().zip(bytes) {
            *target = source.to_ne_bytes()[0];
        }

        Ok(())
    }

    fn read_into_scan_media_counters(
        &self,
        driver: &R::Service,
        record: &mut DiskReading,
    ) -> Result<(), Error> {
        let properties = self.0.registry.properties(driver)?;

        let statistics = dictionary(self.value(properties.0, STATISTICS))?;
        record.read_bytes = self.read_into_scan_media_counters_counter(statistics, 0);
        record.write_bytes = self.read_into_scan_media_counters_counter(statistics, 1);
        record.read_operations = self.read_into_scan_media_counters_counter(statistics, 2);
        record.write_operations = self.read_into_scan_media_counters_counter(statistics, 3);
        record.read_errors = self.read_into_scan_media_counters_counter(statistics, 4);
        record.write_errors = self.read_into_scan_media_counters_counter(statistics, 5);
        record.read_time_ns = self.read_into_scan_media_counters_counter(statistics, 6);
        record.write_time_ns = self.read_into_scan_media_counters_counter(statistics, 7);

        Ok(())
    }

    fn read_into_scan_media_counters_counter(
        &self,
        statistics: *const c_void,
        index: usize,
    ) -> Option<u64> {
        let value = self.value(statistics, FIRST_COUNTER + index);
        if value.is_null() {
            return None;
        }

        if unsafe { CFGetTypeID(value) } != unsafe { CFNumberGetTypeID() } {
            eprintln!("collector: disk counter {index} has a non-number CF type");
            return None;
        }

        if unsafe { CFNumberIsFloatType(value) } != 0 {
            eprintln!("collector: disk counter {index} has a floating CF number");
            return None;
        }

        let mut number = 0_i64;
        if unsafe { CFNumberGetValue(value, 4, ptr::from_mut(&mut number).cast()) } == 0 {
            eprintln!("collector: disk counter {index} cannot fit 64 bits");
            return None;
        }

        // IOKit's unserializer puts unsigned OSNumber bits into a signed CFNumber.
        Some(u64::from_ne_bytes(number.to_ne_bytes()))
    }

    fn value(&self, dictionary: *const c_void, index: usize) -> *const c_void {
        assert!(index < KEY_NAMES.len());

        unsafe { CFDictionaryGetValue(dictionary, self.0.keys[index].0) }
    }
}

impl<R: Registry> DiskSource for Source<R> {
    fn read_into(&mut self, now_ns: u64, snapshot: &mut DiskSnapshot) -> Result<(), Error> {
        *snapshot = DiskSnapshot {
            source_continuous_ns: now_ns,
            ..DiskSnapshot::default()
        };
        for service in &mut self.0.services {
            *service = None;
        }

        self.read_into_scan(snapshot)
    }
}

impl NativeRegistry {
    fn open() -> Result<Self, Error> {
        let matching = Owned(unsafe { IOServiceMatching(c"IOMedia".as_ptr()) });

        if matching.0.is_null() {
            return Err(Error::Allocation);
        }

        let whole = Owned(unsafe {
            CFStringCreateWithCString(ptr::null(), c"Whole".as_ptr(), 0x0800_0100)
        });

        if whole.0.is_null() {
            return Err(Error::Allocation);
        }

        unsafe { CFDictionarySetValue(matching.0, whole.0, kCFBooleanTrue) };

        Ok(Self { matching })
    }
}

impl Registry for NativeRegistry {
    type Service = Service;
    type Cursor = Service;

    fn cursor(&self) -> Result<Self::Cursor, Error> {
        // Matching serialization and property snapshots use Tiger's OS-call departure.
        // The caller-buffer getter rejects the required Statistics dictionary.
        unsafe { CFRetain(self.matching.0) };
        let mut iterator = 0;
        let status = unsafe {
            IOServiceGetMatchingServices(0, self.matching.0.cast_mut(), &raw mut iterator)
        };

        let iterator = Service(iterator);

        if status != 0 {
            return Err(Error::Native(status));
        }

        Ok(iterator)
    }

    fn next(cursor: &mut Self::Cursor) -> Option<Service> {
        if cursor.0 == 0 {
            return None;
        }

        let media = Service(unsafe { IOIteratorNext(cursor.0) });

        if media.0 != 0 {
            return Some(media);
        }

        None
    }

    fn cursor_valid(cursor: &Self::Cursor) -> bool {
        if cursor.0 == 0 {
            return true;
        }

        unsafe { IOIteratorIsValid(cursor.0) != 0 }
    }

    fn conforms(service: &Service, class: &CStr) -> bool {
        unsafe { IOObjectConformsTo(service.0, class.as_ptr()) != 0 }
    }

    fn parent(service: &Service) -> Result<Service, Error> {
        let mut parent = 0;
        let status = unsafe {
            IORegistryEntryGetParentEntry(service.0, c"IOService".as_ptr(), &raw mut parent)
        };

        let parent = Service(parent);

        if status != 0 {
            return Err(Error::Native(status));
        }

        if parent.0 == 0 {
            return Err(Error::InvalidSnapshot);
        }

        Ok(parent)
    }

    fn identity(service: &Service) -> Result<u64, Error> {
        let mut identity = 0;
        let status = unsafe { IORegistryEntryGetRegistryEntryID(service.0, &raw mut identity) };
        if status != 0 {
            return Err(Error::Native(status));
        }

        if identity == 0 {
            return Err(Error::InvalidSnapshot);
        }

        Ok(identity)
    }

    fn properties(&self, service: &Service) -> Result<Owned, Error> {
        let mut properties = ptr::null();
        let status = unsafe {
            IORegistryEntryCreateCFProperties(service.0, &raw mut properties, ptr::null(), 0)
        };

        let properties = Owned(properties);

        if status != 0 {
            return Err(Error::Native(status));
        }

        dictionary(properties.0)?;

        Ok(properties)
    }
}

fn dictionary(value: *const c_void) -> Result<*const c_void, Error> {
    if value.is_null() {
        return Err(Error::InvalidSnapshot);
    }

    if unsafe { CFGetTypeID(value) } != unsafe { CFDictionaryGetTypeID() } {
        return Err(Error::InvalidSnapshot);
    }

    Ok(value)
}

fn string_into(value: *const c_void, bytes: &mut [c_char]) -> Result<(), Error> {
    if value.is_null() {
        return Err(Error::InvalidSnapshot);
    }

    if unsafe { CFGetTypeID(value) } != unsafe { CFStringGetTypeID() } {
        return Err(Error::InvalidSnapshot);
    }

    let count = isize::try_from(bytes.len()).map_err(|_| Error::Capacity)?;
    if unsafe { CFStringGetCString(value, bytes.as_mut_ptr(), count, 0x0800_0100) } == 0 {
        return Err(Error::Capacity);
    }

    Ok(())
}

pub(super) struct Owned(*const c_void);

fn read_into_scan_media_physical_bus(name: &CStr) -> Result<bool, Error> {
    if name == c"Virtual Interface" {
        return Ok(false);
    }

    if PHYSICAL_BUSES.contains(&name) {
        return Ok(true);
    }

    Err(Error::InvalidSnapshot)
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

pub(super) struct Service(u32);

impl Drop for Service {
    fn drop(&mut self) {
        if self.0 != 0 {
            let status = unsafe { IOObjectRelease(self.0) };
            if status != 0 {
                eprintln!("collector: disk service release failed: {status:#x}");
            }
        }
    }
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(port: u32, matching: *mut c_void, result: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOIteratorIsValid(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOObjectConformsTo(object: u32, class: *const c_char) -> u32;
    fn IORegistryEntryGetParentEntry(entry: u32, plane: *const c_char, parent: *mut u32) -> i32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, identity: *mut u64) -> i32;
    fn IORegistryEntryCreateCFProperties(
        entry: u32,
        properties: *mut *const c_void,
        allocator: *const c_void,
        options: u32,
    ) -> i32;
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        value: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(value: *const c_void);
    fn CFRetain(value: *const c_void) -> *const c_void;
    fn CFGetTypeID(value: *const c_void) -> c_ulong;
    fn CFDictionaryGetTypeID() -> c_ulong;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFDictionarySetValue(dictionary: *const c_void, key: *const c_void, value: *const c_void);
    fn CFNumberGetTypeID() -> c_ulong;
    fn CFNumberIsFloatType(value: *const c_void) -> u8;
    fn CFNumberGetValue(value: *const c_void, kind: isize, result: *mut c_void) -> u8;
    fn CFStringGetTypeID() -> c_ulong;
    fn CFStringGetCString(
        value: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
}
