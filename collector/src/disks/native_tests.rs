use super::*;

#[test]
fn preserves_unsigned_counter_bits_in_signed_cf_numbers() {
    // Goal: IOKit's signed CF representation does not discard unsigned OSNumber high bits.
    // Method: give the actual native counter parser a 64-bit CFNumber containing minus one.
    let source = Source::open().unwrap();
    let number = -1_i64;
    let number = Owned(unsafe { CFNumberCreate(ptr::null(), 4, ptr::from_ref(&number).cast()) });

    assert!(!number.0.is_null());

    let dictionary =
        Owned(unsafe { CFDictionaryCreateMutable(ptr::null(), 8, ptr::null(), ptr::null()) });

    assert!(!dictionary.0.is_null());

    unsafe { CFDictionarySetValue(dictionary.0, source.0.keys[FIRST_COUNTER].0, number.0) };

    let value = source.read_into_scan_media_counters_counter(dictionary.0, 0);

    assert_eq!(value, Some(u64::MAX));
    assert_eq!(
        source.read_into_scan_media_counters_counter(dictionary.0, 1),
        None
    );
}

#[test]
fn rejects_a_boolean_counter_independently_of_other_fields() {
    // Goal: a malformed field cannot become zero or invalidate an unrelated numeric field.
    // Method: parse one real integer, one boolean and one absent value from a CF dictionary.
    let source = Source::open().unwrap();
    let value = 0_i64;
    let number = Owned(unsafe { CFNumberCreate(ptr::null(), 4, ptr::from_ref(&value).cast()) });

    let dictionary =
        Owned(unsafe { CFDictionaryCreateMutable(ptr::null(), 8, ptr::null(), ptr::null()) });

    assert!(!number.0.is_null());
    assert!(!dictionary.0.is_null());
    unsafe {
        CFDictionarySetValue(dictionary.0, source.0.keys[FIRST_COUNTER].0, number.0);
        CFDictionarySetValue(
            dictionary.0,
            source.0.keys[FIRST_COUNTER + 1].0,
            kCFBooleanTrue,
        );
    }

    assert_eq!(
        source.read_into_scan_media_counters_counter(dictionary.0, 0),
        Some(0)
    );
    assert_eq!(
        source.read_into_scan_media_counters_counter(dictionary.0, 1),
        None
    );
    assert_eq!(
        source.read_into_scan_media_counters_counter(dictionary.0, 2),
        None
    );
}

#[test]
fn accepts_apple_fabric_as_a_documented_physical_interconnect() {
    // Goal: the Apple Silicon internal disk is not mistaken for a synthetic transport.
    // Method: classify the exact SDK physical-interconnect value.
    assert_eq!(read_into_scan_media_physical_bus(c"Apple Fabric"), Ok(true));
}

#[test]
fn excludes_virtual_interface_disks() {
    // Goal: file/RAM-backed whole media cannot double-count physical driver traffic.
    // Method: classify the SDK virtual-interconnect value.
    assert_eq!(
        read_into_scan_media_physical_bus(c"Virtual Interface"),
        Ok(false)
    );
}

#[test]
fn reports_unknown_transports_instead_of_assuming_they_are_physical() {
    // Goal: unsupported topology remains incomplete rather than silently entering totals.
    // Method: supply an interconnect absent from the documented physical-bus allowlist.
    assert_eq!(
        read_into_scan_media_physical_bus(c"Future Transport"),
        Err(Error::InvalidSnapshot)
    );
}

#[test]
fn native_adapter_excludes_virtual_media_from_a_mixed_scan() {
    // Goal: virtual whole media cannot enter the native adapter's physical inventory.
    // Method: refresh the public collector adapter over physical and virtual registry entries.
    let source = Source::from_registry(FakeRegistry::seed()).unwrap();
    let mut disks = super::super::Disks::from_source(Box::new(source));

    disks.refresh(71).unwrap();

    assert_eq!(disks.snapshot().state, ScanState::Complete);
    assert_eq!(disks.snapshot().count, 1);
    assert_eq!(disks.snapshot().records[0].driver_id, 2);
    assert_eq!(disks.snapshot().records[0].media_id, 1);
    assert_eq!(disks.snapshot().records[1], DiskReading::default());
}

#[test]
fn native_adapter_keeps_all_eight_counter_keys_in_their_named_fields() {
    // Goal: read/write bytes, operations, errors and duration retain their own statistics.
    // Method: scan a physical disk with eight distinct real CFNumbers through the adapter.
    let source = Source::from_registry(FakeRegistry::seed()).unwrap();
    let mut disks = super::super::Disks::from_source(Box::new(source));
    let mut expected = DiskReading {
        driver_id: 2,
        media_id: 1,
        read_bytes: Some(11),
        write_bytes: Some(22),
        read_operations: Some(33),
        write_operations: Some(44),
        read_errors: Some(55),
        write_errors: Some(66),
        read_time_ns: Some(77),
        write_time_ns: Some(88),
        ..DiskReading::default()
    };
    expected.name[..5].copy_from_slice(b"disk0");

    disks.refresh(71).unwrap();

    assert_eq!(disks.snapshot().records[0], expected);
    assert_eq!(disks.snapshot().source_continuous_ns, 71);
}

#[test]
fn native_adapter_rejects_a_cursor_invalidated_after_a_valid_disk() {
    // Goal: invalid enumeration cannot publish a complete inventory or stale previous disks.
    // Method: a first refresh succeeds, then the cursor invalidates after returning real media.
    let mut registry = FakeRegistry::seed();
    registry.invalidate_second_cursor = true;
    let source = Source::from_registry(registry).unwrap();
    let mut disks = super::super::Disks::from_source(Box::new(source));
    disks.refresh(71).unwrap();
    assert_eq!(disks.snapshot().count, 1);

    let result = disks.refresh(72);

    assert_eq!(result, Err(Error::InvalidSnapshot));
    assert_eq!(
        *disks.snapshot(),
        DiskSnapshot {
            source_continuous_ns: 72,
            ..DiskSnapshot::default()
        }
    );
}

#[test]
fn native_adapter_clears_an_uncommitted_disk_after_an_invalid_name() {
    // Goal: rejected metadata leaves no partially constructed record beyond the inventory.
    // Method: supply a dictionary where the physical disk's BSD string belongs.
    let mut registry = FakeRegistry::seed();
    registry.properties[0][0] = seed_properties(c"BSD Name", &seed_dictionary());
    let source = Source::from_registry(registry).unwrap();
    let mut disks = super::super::Disks::from_source(Box::new(source));

    disks.refresh(71).unwrap();

    assert_eq!(disks.snapshot().state, ScanState::Partial);
    assert_eq!(disks.snapshot().count, 0);
    assert_eq!(
        disks.snapshot().records,
        [DiskReading::default(); DISKS_MAX]
    );
}

struct FakeRegistry {
    properties: [[Owned; 3]; 2],
    cursors_opened: std::cell::Cell<u64>,
    invalidate_second_cursor: bool,
}

struct FakeCursor {
    next_index: usize,
    valid: bool,
}

struct FakeService {
    disk_index: usize,
    properties_index: usize,
}

impl FakeRegistry {
    fn seed() -> Self {
        let properties = std::array::from_fn(|disk_index| {
            let name = if disk_index == 0 { c"disk0" } else { c"disk4" };
            let bus = if disk_index == 0 {
                c"Apple Fabric"
            } else {
                c"Virtual Interface"
            };
            let statistics = seed_statistics();

            [
                seed_properties(c"BSD Name", &seed_string(name)),
                seed_properties(c"Statistics", &statistics),
                seed_properties(
                    c"Protocol Characteristics",
                    &seed_properties(c"Physical Interconnect", &seed_string(bus)),
                ),
            ]
        });

        Self {
            properties,
            cursors_opened: std::cell::Cell::new(0),
            invalidate_second_cursor: false,
        }
    }
}

impl Registry for FakeRegistry {
    type Service = FakeService;
    type Cursor = FakeCursor;

    fn cursor(&self) -> Result<FakeCursor, Error> {
        self.cursors_opened.set(self.cursors_opened.get() + 1);

        Ok(FakeCursor {
            next_index: 0,
            valid: !(self.invalidate_second_cursor && self.cursors_opened.get() == 2),
        })
    }

    fn next(cursor: &mut FakeCursor) -> Option<FakeService> {
        if cursor.next_index == 2 {
            return None;
        }

        if !cursor.valid {
            if cursor.next_index == 1 {
                return None;
            }
        }

        let media = FakeService {
            disk_index: cursor.next_index,
            properties_index: 0,
        };
        cursor.next_index += 1;

        Some(media)
    }

    fn cursor_valid(cursor: &FakeCursor) -> bool {
        cursor.valid
    }

    fn parent(service: &FakeService) -> Result<FakeService, Error> {
        assert!(service.properties_index < 2);

        Ok(FakeService {
            disk_index: service.disk_index,
            properties_index: service.properties_index + 1,
        })
    }

    fn conforms(service: &FakeService, class: &CStr) -> bool {
        match service.properties_index {
            1 => class == c"IOBlockStorageDriver",
            2 => class == c"IOBlockStorageDevice",
            _ => false,
        }
    }

    fn identity(service: &FakeService) -> Result<u64, Error> {
        Ok(crate::u64_from_usize(
            service.disk_index * 3 + service.properties_index + 1,
        ))
    }

    fn properties(&self, service: &FakeService) -> Result<Owned, Error> {
        let value = self.properties[service.disk_index][service.properties_index].0;

        Ok(Owned(unsafe { CFRetain(value) }))
    }
}

fn seed_statistics() -> Owned {
    let statistics = seed_dictionary();

    for (index, name) in KEY_NAMES[FIRST_COUNTER..].iter().enumerate() {
        let key = seed_string(name);

        let value = i64::try_from((index + 1) * 11).unwrap();
        let number = Owned(unsafe { CFNumberCreate(ptr::null(), 4, ptr::from_ref(&value).cast()) });

        assert!(!number.0.is_null());
        unsafe { CFDictionarySetValue(statistics.0, key.0, number.0) };
    }

    statistics
}

fn seed_properties(name: &CStr, value: &Owned) -> Owned {
    let dictionary = seed_dictionary();

    let key = seed_string(name);

    unsafe { CFDictionarySetValue(dictionary.0, key.0, value.0) };

    dictionary
}

fn seed_dictionary() -> Owned {
    let dictionary = Owned(unsafe {
        CFDictionaryCreateMutable(
            ptr::null(),
            8,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    });

    assert!(!dictionary.0.is_null());

    dictionary
}

fn seed_string(name: &CStr) -> Owned {
    let value =
        Owned(unsafe { CFStringCreateWithCString(ptr::null(), name.as_ptr(), 0x0800_0100) });

    assert!(!value.0.is_null());

    value
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFNumberCreate(allocator: *const c_void, kind: isize, value: *const c_void)
    -> *const c_void;
    fn CFDictionaryCreateMutable(
        allocator: *const c_void,
        capacity: isize,
        keys: *const c_void,
        values: *const c_void,
    ) -> *const c_void;
}
