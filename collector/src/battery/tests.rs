use super::*;
use native::Owned;
use std::ffi::c_void;
use std::ptr;

#[test]
fn keeps_a_reading_through_sixty_seconds_and_refuses_future_or_older_values() {
    // Goal: held readings have an inclusive sixty-second age bound on continuous time.
    // Method: inspect the same timestamp at the boundary, beyond it, and before it.
    let reading = BatteryReading {
        source_continuous_ns: 100,
        ..BatteryReading::default()
    };

    assert!(reading.is_fresh_at(100));
    assert!(reading.is_fresh_at(100 + READ_INTERVAL_NS));
    assert!(!reading.is_fresh_at(101 + READ_INTERVAL_NS));
    assert!(!reading.is_fresh_at(99));
}

#[test]
fn accepts_a_reading_at_continuous_time_zero() {
    // Goal: zero is a valid source timestamp rather than an unavailable sentinel.
    // Method: apply the shared age policy at the continuous clock's origin.
    assert!(BatteryReading::default().is_fresh_at(0));
}

#[test]
fn reads_nested_full_and_design_without_confusing_normalized_charge() {
    // Goal: full/design capacity may exceed 100 percent independently of charge percent.
    // Method: parse a real CF dictionary with normalized charge and nested mAh values.
    let fixture = Fixture::new();
    let nested = Fixture::dictionary(&[("FullChargeCapacity", 8701), ("DesignCapacity", 8579)]);
    let root = Fixture::root(&nested, &[("CurrentCapacity", 55), ("MaxCapacity", 100)]);
    let mut reading = BatteryReading::default();

    let present = fixture.source.parse_into(&root, 123, &mut reading).unwrap();

    assert!(present);
    assert_eq!(reading.source_continuous_ns, 123);
    assert_eq!(reading.charge_current, Some(55));
    assert_eq!(reading.charge_maximum, Some(100));
    assert_eq!(reading.full_charge_capacity, Some(8701));
    assert_eq!(reading.design_capacity, Some(8579));
}

#[test]
fn retains_zero_charge_cycles_and_current_as_measured_values() {
    // Goal: zeros differ from missing battery fields.
    // Method: parse integer zero values in a present battery's real CF dictionary.
    let fixture = Fixture::new();
    let empty = Fixture::dictionary(&[]);
    let root = Fixture::root(
        &empty,
        &[("CurrentCapacity", 0), ("CycleCount", 0), ("Amperage", 0)],
    );
    let mut reading = BatteryReading::default();

    fixture.source.parse_into(&root, 1, &mut reading).unwrap();

    assert_eq!(reading.charge_current, Some(0));
    assert_eq!(reading.cycle_count, Some(0));
    assert_eq!(reading.current_ma, Some(0));
    assert_eq!(reading.voltage_mv, None);
}

#[test]
fn preserves_signed_discharge_current_and_independent_adapter_power() {
    // Goal: battery current remains signed while adapter rating remains a separate field.
    // Method: parse a negative signed CF number beside an adapter Watts dictionary.
    let fixture = Fixture::new();
    let adapter = Fixture::dictionary(&[("Watts", 94)]);
    let root = named_dictionary(&[
        ("BatteryInstalled", &boolean(true)),
        ("Amperage", &integer(-1234)),
        ("Voltage", &integer(11654)),
        ("AdapterDetails", &adapter),
        ("IsCharging", &boolean(false)),
        ("ExternalConnected", &boolean(true)),
    ]);
    let mut reading = BatteryReading::default();

    fixture.source.parse_into(&root, 0, &mut reading).unwrap();

    assert_eq!(reading.current_ma, Some(-1234));
    assert_eq!(reading.voltage_mv, Some(11654));
    assert_eq!(reading.adapter_watts, Some(94));
    assert_eq!(reading.charging, Some(false));
    assert_eq!(reading.external_power, Some(true));
}

#[test]
fn prefers_a_complete_top_level_raw_capacity_pair() {
    // Goal: an available top-level raw pair is selected together.
    // Method: supply different complete raw and nested capacity pairs.
    let fixture = Fixture::new();
    let nested = Fixture::dictionary(&[("FullChargeCapacity", 8701), ("DesignCapacity", 8579)]);
    let root = Fixture::root(
        &nested,
        &[("AppleRawMaxCapacity", 9000), ("DesignCapacity", 8000)],
    );
    let mut reading = BatteryReading::default();

    fixture.source.parse_into(&root, 1, &mut reading).unwrap();

    assert_eq!(reading.full_charge_capacity, Some(9000));
    assert_eq!(reading.design_capacity, Some(8000));
}

#[test]
fn never_mixes_a_top_level_full_capacity_with_a_nested_design_capacity() {
    // Goal: missing capacity members cannot acquire a denominator from another source.
    // Method: supply only the top-level numerator and only the nested denominator.
    let fixture = Fixture::new();
    let nested = Fixture::dictionary(&[("DesignCapacity", 8579)]);
    let root = Fixture::root(&nested, &[("AppleRawMaxCapacity", 9000)]);
    let mut reading = BatteryReading::default();

    fixture.source.parse_into(&root, 1, &mut reading).unwrap();

    assert_eq!(reading.full_charge_capacity, None);
    assert_eq!(reading.design_capacity, Some(8579));
}

#[test]
fn never_substitutes_normalized_maximum_or_nominal_capacity_for_full_capacity() {
    // Goal: health keeps the full/design definition when other capacity keys exist.
    // Method: provide a normalized maximum and a nested nominal value without full capacity.
    let fixture = Fixture::new();
    let nested = Fixture::dictionary(&[("NominalChargeCapacity", 8945), ("DesignCapacity", 8579)]);
    let root = Fixture::root(&nested, &[("MaxCapacity", 100), ("DesignCapacity", 8579)]);
    let mut reading = BatteryReading::default();

    fixture.source.parse_into(&root, 1, &mut reading).unwrap();

    assert_eq!(reading.charge_maximum, Some(100));
    assert_eq!(reading.full_charge_capacity, None);
    assert_eq!(reading.design_capacity, Some(8579));
}

#[test]
fn preserves_zero_capacity_denominators_as_raw_measurements() {
    // Goal: raw zero denominators remain distinct from unavailable measurements.
    // Method: parse zero design and charge maximum beside valid numerators and cycles.
    let fixture = Fixture::new();
    let nested = Fixture::dictionary(&[("FullChargeCapacity", 0), ("DesignCapacity", 0)]);
    let root = Fixture::root(
        &nested,
        &[
            ("CurrentCapacity", 55),
            ("MaxCapacity", 0),
            ("CycleCount", 11),
        ],
    );
    let mut reading = BatteryReading::default();

    fixture.source.parse_into(&root, 1, &mut reading).unwrap();

    assert_eq!(reading.charge_current, Some(55));
    assert_eq!(reading.charge_maximum, Some(0));
    assert_eq!(reading.full_charge_capacity, Some(0));
    assert_eq!(reading.design_capacity, Some(0));
    assert_eq!(reading.cycle_count, Some(11));
}

macro_rules! malformed_current {
    ($name:ident, $value:expr) => {
        #[test]
        fn $name() {
            // Goal: malformed optional fields cannot erase independent valid values.
            // Method: parse a real CF value of the wrong type or signed range.
            let fixture = Fixture::new();
            let root = named_dictionary(&[
                ("BatteryInstalled", &boolean(true)),
                ("CurrentCapacity", &$value),
                ("CycleCount", &integer(11)),
            ]);
            let mut reading = BatteryReading::default();

            fixture.source.parse_into(&root, 1, &mut reading).unwrap();

            assert_eq!(reading.charge_current, None);
            assert_eq!(reading.cycle_count, Some(11));
        }
    };
}

malformed_current!(refuses_a_negative_charge_value, integer(-1));
malformed_current!(refuses_a_boolean_charge_value, boolean(true));
malformed_current!(refuses_a_string_charge_value, key("55"));
malformed_current!(refuses_a_floating_charge_value, floating(55.0));
malformed_current!(refuses_a_nonfinite_charge_value, floating(f64::NAN));
malformed_current!(refuses_a_dictionary_charge_value, Fixture::dictionary(&[]));

#[test]
fn refuses_integer_booleans_and_malformed_nested_dictionaries_independently() {
    // Goal: numeric truth values and non-dictionary containers do not cross the boundary.
    // Method: supply malformed boolean and nested values beside a valid voltage.
    let fixture = Fixture::new();
    let root = named_dictionary(&[
        ("BatteryInstalled", &boolean(true)),
        ("IsCharging", &integer(1)),
        ("ExternalConnected", &key("Yes")),
        ("BatteryData", &integer(4)),
        ("AdapterDetails", &boolean(true)),
        ("Voltage", &integer(11654)),
    ]);
    let mut reading = BatteryReading::default();

    fixture.source.parse_into(&root, 1, &mut reading).unwrap();

    assert_eq!(reading.charging, None);
    assert_eq!(reading.external_power, None);
    assert_eq!(reading.full_charge_capacity, None);
    assert_eq!(reading.adapter_watts, None);
    assert_eq!(reading.voltage_mv, Some(11654));
}

#[test]
fn clears_a_previous_value_when_a_new_snapshot_has_no_battery() {
    // Goal: removal cannot retain the preceding battery's readings.
    // Method: parse presence false into a previously populated output record.
    let fixture = Fixture::new();
    let root = named_dictionary(&[("BatteryInstalled", &boolean(false))]);
    let mut reading = BatteryReading {
        cycle_count: Some(11),
        ..BatteryReading::default()
    };

    let present = fixture.source.parse_into(&root, 10, &mut reading).unwrap();

    assert!(!present);
    assert_eq!(reading, BatteryReading::default());
}

#[test]
fn clears_a_previous_value_when_snapshot_presence_is_invalid() {
    // Goal: a failed snapshot cannot retain any fields from the preceding reading.
    // Method: parse invalid presence into a previously populated output record.
    let fixture = Fixture::new();
    let root = named_dictionary(&[("BatteryInstalled", &integer(1))]);
    let mut reading = BatteryReading {
        cycle_count: Some(11),
        ..BatteryReading::default()
    };

    let result = fixture.source.parse_into(&root, 10, &mut reading);

    assert_eq!(result, Err(Error::InvalidPresence));
    assert_eq!(reading, BatteryReading::default());
}

#[test]
fn distinguishes_a_missing_presence_field_from_an_absent_battery() {
    // Goal: an incomplete service snapshot does not imply an absent battery.
    // Method: parse an empty real CF dictionary.
    let fixture = Fixture::new();
    let root = Fixture::dictionary(&[]);
    let mut reading = BatteryReading::default();

    let result = fixture.source.parse_into(&root, 10, &mut reading);

    assert_eq!(result, Err(Error::InvalidPresence));
}

#[test]
fn refuses_a_non_dictionary_snapshot() {
    // Goal: type checking happens before any dictionary access.
    // Method: pass a real CF number in place of the snapshot dictionary.
    let fixture = Fixture::new();
    let root = integer(1);
    let mut reading = BatteryReading::default();

    let result = fixture.source.parse_into(&root, 10, &mut reading);

    assert_eq!(result, Err(Error::InvalidSnapshot));
}

#[test]
fn attempts_immediately_and_after_elapsed_minutes_including_a_long_gap() {
    // Goal: elapsed continuous time controls cadence, including sleep and missed ticks.
    // Method: use an absent native source and explicit timestamps without sleeping.
    let mut battery = Fixture::battery();

    assert_eq!(battery.state(), State::Pending);
    assert_eq!(battery.refresh(0), Ok(true));
    assert_eq!(battery.state(), State::Absent);
    assert_eq!(battery.refresh(READ_INTERVAL_NS - 1), Ok(false));
    assert_eq!(battery.refresh(READ_INTERVAL_NS), Ok(true));
    assert_eq!(battery.refresh(10 * READ_INTERVAL_NS), Ok(true));
    assert_eq!(battery.refresh(10 * READ_INTERVAL_NS), Ok(false));
    assert_eq!(battery.reading(10 * READ_INTERVAL_NS), None);
}

#[test]
fn rejects_reversed_continuous_time() {
    // Goal: a reversed clock is a reported operating error, never a subtraction panic.
    // Method: refresh an absent source before the preceding attempt's timestamp.
    let mut battery = Fixture::battery();
    battery.refresh(10).unwrap();

    let result = battery.refresh(9);

    assert_eq!(result, Err(Error::TimeReversed));
}

#[test]
fn clears_a_held_value_before_a_scheduled_attempt_can_fail() {
    // Goal: scheduled acquisition failure cannot expose the preceding battery reading.
    // Method: refresh an injected source that succeeds once, then fails sixty seconds later.
    let reading = BatteryReading {
        cycle_count: Some(11),
        ..BatteryReading::default()
    };
    let mut battery = Battery::from_source(FakeSource::new(&reading));
    assert_eq!(battery.refresh(0), Ok(true));
    assert_eq!(battery.reading(0), Some(&reading));

    let result = battery.refresh(READ_INTERVAL_NS);

    assert_eq!(result, Err(Error::Native(-1)));
    assert_eq!(battery.reading(READ_INTERVAL_NS), None);
    assert_eq!(battery.state(), State::Failed);
    assert_eq!(battery.refresh(READ_INTERVAL_NS + 1), Ok(false));
}

struct FakeSource {
    reading: Option<BatteryReading>,
}

impl FakeSource {
    fn new(reading: &BatteryReading) -> Box<Self> {
        Box::new(Self {
            reading: Some(*reading),
        })
    }
}

impl BatterySource for FakeSource {
    fn read_into(&mut self, now_ns: u64, output: &mut BatteryReading) -> Result<bool, Error> {
        let Some(mut reading) = self.reading.take() else {
            return Err(Error::Native(-1));
        };

        reading.source_continuous_ns = now_ns;
        *output = reading;

        Ok(true)
    }
}

struct Fixture {
    source: native::Source,
}

impl Fixture {
    fn new() -> Self {
        Self {
            source: native::Source::keys().unwrap(),
        }
    }

    fn battery() -> Battery {
        Battery::from_source(Box::new(native::Source::keys().unwrap()))
    }

    fn dictionary(numbers: &[(&str, i64)]) -> Owned {
        assert!(numbers.len() <= 32);
        let mut keys = Vec::with_capacity(numbers.len());
        let mut values = Vec::with_capacity(numbers.len());
        for (name, value) in numbers.iter().take(32) {
            keys.push(key(name));
            values.push(integer(*value));
        }
        dictionary(&keys, &values)
    }

    fn root(nested: &Owned, numbers: &[(&str, i64)]) -> Owned {
        assert!(numbers.len() <= 32);
        let mut keys = vec![key("BatteryInstalled"), key("BatteryData")];
        let mut values = vec![boolean(true), Owned(unsafe { CFRetain(nested.0) })];
        for (name, value) in numbers.iter().take(32) {
            keys.push(key(name));
            values.push(integer(*value));
        }
        dictionary(&keys, &values)
    }
}

fn named_dictionary(entries: &[(&str, &Owned)]) -> Owned {
    assert!(entries.len() <= 32);
    let keys: Vec<_> = entries.iter().map(|(name, _)| key(name)).collect();
    let values: Vec<_> = entries
        .iter()
        .map(|(_, value)| Owned(unsafe { CFRetain(value.0) }))
        .collect();
    dictionary(&keys, &values)
}

fn floating(value: f64) -> Owned {
    let value = unsafe { CFNumberCreate(ptr::null(), 6, ptr::from_ref(&value).cast()) };
    assert!(!value.is_null());
    Owned(value)
}

fn key(name: &str) -> Owned {
    let name = std::ffi::CString::new(name).unwrap();
    let value =
        unsafe { native::CFStringCreateWithCString(ptr::null(), name.as_ptr(), 0x0800_0100) };
    assert!(!value.is_null());
    Owned(value)
}

fn integer(value: i64) -> Owned {
    let value = unsafe { CFNumberCreate(ptr::null(), 4, ptr::from_ref(&value).cast()) };
    assert!(!value.is_null());
    Owned(value)
}

fn boolean(value: bool) -> Owned {
    let value = if value {
        unsafe { kCFBooleanTrue }
    } else {
        unsafe { kCFBooleanFalse }
    };
    Owned(unsafe { CFRetain(value) })
}

fn dictionary(keys: &[Owned], values: &[Owned]) -> Owned {
    assert_eq!(keys.len(), values.len());
    let keys: Vec<_> = keys.iter().map(|value| value.0).collect();
    let values: Vec<_> = values.iter().map(|value| value.0).collect();
    let value = unsafe {
        CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            isize::try_from(keys.len()).unwrap(),
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    };
    assert!(!value.is_null());
    Owned(value)
}

#[repr(C)]
struct CFDictionaryKeyCallBacks {
    version: isize,
    retain: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void, *const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> u8>,
    hash: Option<unsafe extern "C" fn(*const c_void) -> usize>,
}

#[repr(C)]
struct CFDictionaryValueCallBacks {
    version: isize,
    retain: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void, *const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> u8>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFNumberCreate(allocator: *const c_void, kind: isize, value: *const c_void)
    -> *const c_void;
    fn CFRetain(value: *const c_void) -> *const c_void;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
}
