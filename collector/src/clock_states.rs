//! The clock frequency of each performance state a CPU core reports through `IOReport`, read
//! from the voltage-state tables the power manager publishes in the I/O Registry.

use std::ffi::{CString, c_char, c_ulong, c_void};
use std::io;

/// The voltage-state tables of the power manager, each one's frequencies in kilohertz,
/// rising, and none of them twice.
#[derive(Debug, Default)]
pub struct ClockTables {
    tables: Vec<Vec<u32>>,
}

impl ClockTables {
    /// Tables already read, each in kilohertz and rising, for a test of what reads them.
    #[must_use]
    pub fn from_tables(tables: Vec<Vec<u32>>) -> Self {
        for table in &tables {
            for pair in table.windows(2) {
                assert!(pair[0] < pair[1]);
            }
        }
        Self { tables }
    }

    /// The frequencies of a core whose `IOReport` channel reports `count` active states, the
    /// slowest first, or none where no table, or more than one, has that many entries. A
    /// core's active states rise with its clock, so the table with the same count is read as
    /// that core's table, which is inference from the state names and not a documented
    /// mapping.
    #[must_use]
    pub fn for_states(&self, count: usize) -> Option<&[u32]> {
        let mut found: Option<&[u32]> = None;
        for table in &self.tables {
            if table.len() == count {
                if found.is_some() {
                    return None;
                }
                found = Some(table);
            }
        }
        found
    }
}

/// The most voltage-state tables read, past the 31 the power manager of an M5 Pro names.
const TABLES_MAX: u32 = 64;

/// Each entry of a voltage-state table is a frequency and a voltage, two 32-bit words.
const ENTRY_BYTES: usize = 8;

/// Reads the power manager's tables, or an error where the registry has no power manager.
/// It allocates, so a program reads them once, at startup.
pub fn read() -> io::Result<ClockTables> {
    let service = Service::power_manager()?;

    let mut tables = ClockTables::default();
    for index in 0..TABLES_MAX {
        for suffix in ["", "-sram"] {
            let Some(bytes) = service.data(&format!("voltage-states{index}{suffix}"))? else {
                continue;
            };
            let Some(table) = frequencies_khz(&bytes) else {
                continue;
            };
            if !tables.tables.contains(&table) {
                tables.tables.push(table);
            }
        }
    }
    Ok(tables)
}

/// The frequencies of one voltage-state table in kilohertz, or none where they are not a
/// clock table: entries that do not rise, an entry of 0, or a top frequency outside what a
/// clock in hertz or in kilohertz would read. The power manager of an M5 Pro gives its CPU
/// tables in kilohertz and older chips give theirs in hertz.
fn frequencies_khz(bytes: &[u8]) -> Option<Vec<u32>> {
    if bytes.is_empty() {
        return None;
    }
    if !bytes.len().is_multiple_of(ENTRY_BYTES) {
        return None;
    }

    let (entries, rest) = bytes.as_chunks::<ENTRY_BYTES>();
    assert!(rest.is_empty());
    let frequencies: Vec<u32> = entries
        .iter()
        .map(|entry| u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]))
        .collect();
    for pair in frequencies.windows(2) {
        if pair[0] >= pair[1] {
            return None;
        }
    }
    if frequencies[0] == 0 {
        return None;
    }

    let top = frequencies[frequencies.len() - 1];
    if (500_000..=7_000_000).contains(&top) {
        return Some(frequencies);
    }
    if top >= 500_000_000 {
        // This floors, though every hertz table read so far holds whole kilohertz.
        return Some(frequencies.iter().map(|hertz| hertz / 1000).collect());
    }
    None
}

/// An I/O Registry entry this module owns a reference to.
struct Service(u32);

impl Service {
    fn power_manager() -> io::Result<Self> {
        let matching = unsafe { IOServiceNameMatching(c"pmgr".as_ptr()) };
        if matching.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "IOServiceNameMatching returned no dictionary",
            ));
        }

        // IOServiceGetMatchingService consumes the matching dictionary.
        let service = Self(unsafe { IOServiceGetMatchingService(0, matching) });

        if service.0 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the I/O Registry has no power manager",
            ));
        }
        Ok(service)
    }

    /// The bytes of the data property `key`, or none where the entry has no such property or
    /// it holds something other than data.
    fn data(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        let key = CString::new(key).map_err(|error| io::Error::other(error.to_string()))?;
        let name = Owned(unsafe {
            CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8_ENCODING)
        });

        if name.0.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "CFStringCreateWithCString returned no string",
            ));
        }
        let value =
            Owned(unsafe { IORegistryEntryCreateCFProperty(self.0, name.0, std::ptr::null(), 0) });

        if value.0.is_null() {
            return Ok(None);
        }
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
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.0 != 0 {
            let status = unsafe { IOObjectRelease(self.0) };
            if status != 0 {
                eprintln!("collector: power manager release failed: {status:#x}");
            }
        }
    }
}

/// A CoreFoundation object this module owns a reference to.
struct Owned(*const c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

const UTF8_ENCODING: u32 = 0x0800_0100;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceNameMatching(name: *const c_char) -> *mut c_void;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(frequencies: &[u32]) -> Vec<u8> {
        frequencies
            .iter()
            .flat_map(|frequency| {
                let mut entry = frequency.to_le_bytes().to_vec();
                entry.extend_from_slice(&900_u32.to_le_bytes());
                entry
            })
            .collect()
    }

    #[test]
    fn frequencies_khz_keeps_a_kilohertz_table() {
        // Goal: a table already in kilohertz, as an M5 Pro gives its CPU tables, reads as is.
        // Method: three rising entries topping out at 4.608 GHz.
        let bytes = table(&[1_308_000, 2_580_000, 4_608_000]);

        let frequencies = frequencies_khz(&bytes);

        assert_eq!(frequencies, Some(vec![1_308_000, 2_580_000, 4_608_000]));
    }

    #[test]
    fn frequencies_khz_converts_a_hertz_table() {
        // Goal: a table in hertz, as older chips give theirs, reads in kilohertz.
        // Method: three rising entries topping out at 3.2 GHz.
        let bytes = table(&[600_000_000, 1_200_000_000, 3_204_000_000]);

        let frequencies = frequencies_khz(&bytes);

        assert_eq!(frequencies, Some(vec![600_000, 1_200_000, 3_204_000]));
    }

    #[test]
    fn frequencies_khz_refuses_what_is_not_a_clock_table() {
        // Goal: tables that cannot be a clock's states read as none, so no core is given them.
        // Method: falling entries, a repeated entry, a leading 0, a top below any clock, a
        // ragged length and no bytes, each from tables the power manager of an M5 Pro holds.
        let refused = [
            table(&[50_103, 40_454, 33_098]),
            table(&[801_000_000, 2_004_000_000, 2_004_000_000]),
            table(&[0, 338_000_000, 1_620_000_000]),
            table(&[1, 2, 3]),
            vec![1, 2, 3],
            Vec::new(),
        ];

        let frequencies = refused.map(|bytes| frequencies_khz(&bytes));

        assert_eq!(frequencies, [None, None, None, None, None, None]);
    }

    #[test]
    fn for_states_finds_the_one_table_of_that_count() {
        // Goal: a core's states take the only table with as many entries, and a count two
        // tables share, or none has, takes no table.
        // Method: tables of two, three and three entries, asked for each count.
        let tables = ClockTables {
            tables: vec![vec![1, 2], vec![1, 2, 3], vec![4, 5, 6]],
        };

        let found = [2, 3, 4].map(|count| tables.for_states(count));

        assert_eq!(found, [Some(&[1_u32, 2][..]), None, None]);
    }

    #[test]
    fn reads_the_tables_of_this_mac() {
        // Goal: the registry of an Apple silicon Mac yields at least one clock table.
        // Method: read this Mac's power manager.
        let tables = read().unwrap();

        assert!(!tables.tables.is_empty());
    }
}
