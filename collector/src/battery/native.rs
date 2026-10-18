use std::ffi::{CStr, c_char, c_ulong, c_void};
use std::ptr;

use super::{BatteryReading, BatterySource, Error};

pub(super) struct Source(Box<Storage>);

const _: () = assert!(size_of::<Source>() <= 16);

struct Storage {
    service: Service,
    keys: [Owned; KEY_NAMES.len()],
}

const INSTALLED: usize = 0;
const CURRENT: usize = 1;
const MAXIMUM: usize = 2;
const FULL: usize = 3;
const DESIGN: usize = 4;
const CYCLES: usize = 5;
const CHARGING: usize = 6;
const EXTERNAL: usize = 7;
const VOLTAGE: usize = 8;
const AMPERAGE: usize = 9;
const BATTERY_DATA: usize = 10;
const NESTED_FULL: usize = 11;
const ADAPTER: usize = 12;
const WATTS: usize = 13;

const KEY_NAMES: [&CStr; 14] = [
    c"BatteryInstalled",
    c"CurrentCapacity",
    c"MaxCapacity",
    c"AppleRawMaxCapacity",
    c"DesignCapacity",
    c"CycleCount",
    c"IsCharging",
    c"ExternalConnected",
    c"Voltage",
    c"Amperage",
    c"BatteryData",
    c"FullChargeCapacity",
    c"AdapterDetails",
    c"Watts",
];

const _: () = assert!(WATTS + 1 == KEY_NAMES.len());

impl Source {
    pub(super) fn open() -> Result<Self, Error> {
        let mut source = Self::keys()?;
        let matching = unsafe { IOServiceMatching(c"AppleSmartBattery".as_ptr()) };
        if matching.is_null() {
            return Err(Error::Allocation);
        }

        let mut iterator = 0;
        let status = unsafe { IOServiceGetMatchingServices(0, matching, &raw mut iterator) };

        let iterator = Service(iterator);
        if status != 0 {
            return Err(Error::Native(status));
        }

        if iterator.0 == 0 {
            return Err(Error::InvalidSnapshot);
        }

        source.0.service = Service(unsafe { IOIteratorNext(iterator.0) });

        let extra = Service(unsafe { IOIteratorNext(iterator.0) });
        if extra.0 != 0 {
            return Err(Error::MultipleBatteries);
        }

        Ok(source)
    }

    pub(super) fn keys() -> Result<Self, Error> {
        let mut source = Self(Box::new(Storage {
            service: Service(0),
            keys: std::array::from_fn(|_| Owned(ptr::null())),
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

    fn read_into_snapshot(&self, now_ns: u64, reading: &mut BatteryReading) -> Result<bool, Error> {
        if self.0.service.0 == 0 {
            return Ok(false);
        }

        // The owned-buffer getter cannot return the required nested capacity pair.
        // This OS snapshot allocation uses Tiger's existing OS-call departure; no Rust
        // collection or serialization allocation occurs after startup.
        let mut dictionary = ptr::null();
        let status = unsafe {
            IORegistryEntryCreateCFProperties(self.0.service.0, &raw mut dictionary, ptr::null(), 0)
        };

        let dictionary = Owned(dictionary);
        if status != 0 {
            return Err(Error::Native(status));
        }

        self.parse_into(&dictionary, now_ns, reading)
    }

    pub(super) fn parse_into(
        &self,
        snapshot: &Owned,
        now_ns: u64,
        reading: &mut BatteryReading,
    ) -> Result<bool, Error> {
        *reading = BatteryReading::default();
        let dictionary = dictionary_ref(snapshot.0).ok_or(Error::InvalidSnapshot)?;
        match boolean(self.value(dictionary, INSTALLED), INSTALLED) {
            Some(true) => {}
            Some(false) => return Ok(false),
            None => return Err(Error::InvalidPresence),
        }

        reading.source_continuous_ns = now_ns;
        reading.charge_current = self.unsigned(dictionary, CURRENT);
        reading.charge_maximum = self.unsigned(dictionary, MAXIMUM);
        self.parse_into_capacity(dictionary, reading);
        reading.cycle_count = self.unsigned(dictionary, CYCLES);
        reading.charging = boolean(self.value(dictionary, CHARGING), CHARGING);
        reading.external_power = boolean(self.value(dictionary, EXTERNAL), EXTERNAL);
        reading.voltage_mv = self.unsigned(dictionary, VOLTAGE);
        reading.current_ma = number(self.value(dictionary, AMPERAGE), AMPERAGE);
        reading.adapter_watts = dictionary_ref(self.value(dictionary, ADAPTER))
            .and_then(|adapter| self.unsigned(adapter, WATTS));

        Ok(true)
    }

    fn parse_into_capacity(&self, snapshot: *const c_void, reading: &mut BatteryReading) {
        let full = self.unsigned(snapshot, FULL);
        let design = self.unsigned(snapshot, DESIGN);

        if full.is_some() {
            if design.is_some() {
                reading.full_charge_capacity = full;
                reading.design_capacity = design;

                return;
            }
        }

        if let Some(nested) = dictionary_ref(self.value(snapshot, BATTERY_DATA)) {
            reading.full_charge_capacity = self.unsigned(nested, NESTED_FULL);
            reading.design_capacity = self.unsigned(nested, DESIGN);
        } else {
            reading.full_charge_capacity = full;
            reading.design_capacity = design;
        }
    }

    fn value(&self, dictionary: *const c_void, index: usize) -> *const c_void {
        assert!(index < KEY_NAMES.len());

        unsafe { CFDictionaryGetValue(dictionary, self.0.keys[index].0) }
    }

    fn unsigned(&self, dictionary: *const c_void, index: usize) -> Option<u64> {
        let value = number(self.value(dictionary, index), index)?;
        if let Ok(value) = u64::try_from(value) {
            Some(value)
        } else {
            eprintln!("collector: battery field {index} is negative: {value}");
            None
        }
    }
}

impl BatterySource for Source {
    fn read_into(&mut self, now_ns: u64, reading: &mut BatteryReading) -> Result<bool, Error> {
        self.read_into_snapshot(now_ns, reading)
    }
}

fn dictionary_ref(value: *const c_void) -> Option<*const c_void> {
    if value.is_null() {
        return None;
    }

    if unsafe { CFGetTypeID(value) } == unsafe { CFDictionaryGetTypeID() } {
        Some(value)
    } else {
        eprintln!("collector: battery snapshot member has a non-dictionary CF type");
        None
    }
}

fn number(value: *const c_void, index: usize) -> Option<i64> {
    if value.is_null() {
        return None;
    }

    if unsafe { CFGetTypeID(value) } != unsafe { CFNumberGetTypeID() } {
        eprintln!("collector: battery field {index} has a non-number CF type");
        return None;
    }

    if unsafe { CFNumberIsFloatType(value) } != 0 {
        eprintln!("collector: battery field {index} has a floating CF number");
        return None;
    }

    let mut result = 0_i64;
    if unsafe { CFNumberGetValue(value, 4, ptr::from_mut(&mut result).cast()) } == 0 {
        eprintln!("collector: battery field {index} cannot fit signed 64 bits");
        return None;
    }

    Some(result)
}

fn boolean(value: *const c_void, index: usize) -> Option<bool> {
    if value.is_null() {
        return None;
    }

    if unsafe { CFGetTypeID(value) } != unsafe { CFBooleanGetTypeID() } {
        eprintln!("collector: battery field {index} has a non-boolean CF type");
        return None;
    }

    Some(unsafe { CFBooleanGetValue(value) } != 0)
}

pub(super) struct Owned(pub(super) *const c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

struct Service(u32);

impl Drop for Service {
    fn drop(&mut self) {
        if self.0 != 0 {
            let status = unsafe { IOObjectRelease(self.0) };
            if status != 0 {
                eprintln!("collector: battery service release failed: {status:#x}");
            }
        }
    }
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(port: u32, matching: *mut c_void, result: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateCFProperties(
        entry: u32,
        properties: *mut *const c_void,
        allocator: *const c_void,
        options: u32,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub(super) fn CFStringCreateWithCString(
        allocator: *const c_void,
        value: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(value: *const c_void);
    fn CFGetTypeID(value: *const c_void) -> c_ulong;
    fn CFDictionaryGetTypeID() -> c_ulong;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetTypeID() -> c_ulong;
    fn CFNumberIsFloatType(value: *const c_void) -> u8;
    fn CFNumberGetValue(value: *const c_void, kind: isize, result: *mut c_void) -> u8;
    fn CFBooleanGetTypeID() -> c_ulong;
    fn CFBooleanGetValue(value: *const c_void) -> u8;
}
