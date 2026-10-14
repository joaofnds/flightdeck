use std::ffi::{CStr, c_char, c_void};
use std::io;

use crate::kernel::{self, KernReturn, MachPort, kern_return_check};
use crate::push_without_growing;

pub struct Smc {
    connection: Connection,
    power: FloatKey,
    fans: Vec<FloatKey>,
    soc_sensor: FloatKey,
    power_watts: f32,
    fan_rpm: Vec<f32>,
    soc_celsius: f32,
}

/// A key the SMC holds as a float, as its info showed when the key was checked, so a read of
/// it each tick need not ask again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FloatKey(Key);

/// A connection to the SMC, through a port that is never 0 and that only this connection
/// closes.
struct Connection(MachPort);

/// An `IOKit` service this holds a reference to, which it releases when it drops.
#[derive(Debug)]
struct Service(MachPort);

impl Service {
    /// Finds the first service `IOKit` has of the class `class`.
    fn matching(class: &CStr) -> io::Result<Self> {
        assert!(!class.is_empty());

        let matching = unsafe { IOServiceMatching(class.as_ptr()) };
        if matching.is_null() {
            return Err(io::Error::other(format!(
                "IOKit made no dictionary to match {class:?}"
            )));
        }

        let service = unsafe { IOServiceGetMatchingService(kIOMainPortDefault, matching) };
        if service == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("IOKit has no service of the class {class:?}"),
            ));
        }

        let service = Self(service);
        assert_ne!(0, service.0);
        Ok(service)
    }
}

impl Drop for Service {
    /// Releases the service, and reports on stderr a release that failed, since a drop has no
    /// caller to return the error to.
    fn drop(&mut self) {
        assert_ne!(0, self.0);

        let released = unsafe { IOObjectRelease(self.0) };
        if let Err(error) = kern_return_check(released, "releasing an IOKit service") {
            eprintln!("collector: {error}");
        }
    }
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    static kIOMainPortDefault: MachPort;
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> MachPort;
    fn IOServiceOpen(
        service: MachPort,
        owning_task: MachPort,
        kind: u32,
        connection: *mut MachPort,
    ) -> KernReturn;
    fn IOServiceClose(connection: MachPort) -> KernReturn;
    fn IOObjectRelease(object: MachPort) -> KernReturn;
    fn IOConnectCallStructMethod(
        connection: MachPort,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;
}

impl Drop for Connection {
    /// Closes the connection, and reports on stderr a close that failed, since a drop has no
    /// caller to return the error to.
    fn drop(&mut self) {
        assert_ne!(0, self.0);

        let closed = unsafe { IOServiceClose(self.0) };
        if let Err(error) = kern_return_check(closed, "closing the SMC connection") {
            eprintln!("collector: {error}");
        }
    }
}

impl Connection {
    fn open() -> io::Result<Self> {
        let service = Service::matching(c"AppleSMCKeysEndpoint")?;
        assert_ne!(0, service.0);

        let mut port = 0;
        let opened = unsafe { IOServiceOpen(service.0, kernel::task_self(), 0, &raw mut port) };
        kern_return_check(opened, "connecting to the SMC")?;
        if port == 0 {
            return Err(io::Error::other("connecting to the SMC gave no connection"));
        }

        let connection = Self(port);
        assert_ne!(0, connection.0);
        Ok(connection)
    }

    fn hottest_soc_sensor(&self) -> io::Result<FloatKey> {
        let key_count = self.key_count()?;
        assert!(key_count <= KEYS_MAX);

        let mut hottest = None;
        for index in 0..key_count {
            let key = self.key_at(index)?;
            if !key.is_soc_temperature() {
                continue;
            }
            if !self.info(key)?.is(FLOAT) {
                continue;
            }

            let sensor = FloatKey(key);
            hottest = hottest_soc_sensor_of(hottest, (sensor, self.read_float(sensor)?));
        }

        let Some((sensor, celsius)) = hottest else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the SMC has no SoC temperature sensor",
            ));
        };

        assert!(sensor.0.is_soc_temperature());
        assert!(celsius.is_finite());
        Ok(sensor)
    }

    fn fan_count(&self) -> io::Result<u8> {
        const { assert!(UNSIGNED_8.size == 1) };
        let [fan_count, ..] = self.read(FAN_COUNT, UNSIGNED_8)?;
        fan_count_check(fan_count)?;

        assert!(fan_count <= FANS_MAX);
        Ok(fan_count)
    }

    fn key_count(&self) -> io::Result<u32> {
        const { assert!(UNSIGNED_32.size == 4) };
        let [first, second, third, fourth, ..] = self.read(KEY_COUNT, UNSIGNED_32)?;

        let key_count = u32::from_be_bytes([first, second, third, fourth]);
        key_count_check(key_count)?;

        assert!(key_count <= KEYS_MAX);
        Ok(key_count)
    }

    fn key_at(&self, index: u32) -> io::Result<Key> {
        assert!(index < KEYS_MAX);

        let listed = self.ask(&Message {
            command: READ_INDEX,
            index,
            ..Message::default()
        })?;
        Ok(Key(listed.key))
    }

    /// Checks that the SMC holds `key` as a float.
    fn float_key(&self, key: Key) -> io::Result<FloatKey> {
        self.held_check(key, FLOAT)?;
        Ok(FloatKey(key))
    }

    fn read_float(&self, key: FloatKey) -> io::Result<f32> {
        const { assert!(FLOAT.size == 4) };
        let [first, second, third, fourth, ..] = self.bytes(key.0, FLOAT)?;

        Ok(f32::from_ne_bytes([first, second, third, fourth]))
    }

    /// Reads `key` as a value of `info` once the SMC shows it holds one, since the SMC answers
    /// a read of a key with its bytes whatever type the read names.
    fn read(&self, key: Key, info: KeyInfo) -> io::Result<[u8; MESSAGE_BYTES_SIZE]> {
        assert!(info.size > 0);
        assert!(info.size <= VALUE_SIZE_MAX);

        self.held_check(key, info)?;
        self.bytes(key, info)
    }

    /// Checks that the SMC holds `key` as a value of `wanted`.
    fn held_check(&self, key: Key, wanted: KeyInfo) -> io::Result<()> {
        let held = Held {
            key,
            info: self.info(key)?,
        };

        held_check_of(held, wanted)?;
        assert!(held.info.is(wanted));
        Ok(())
    }

    fn info(&self, key: Key) -> io::Result<KeyInfo> {
        let described = self.ask(&Message {
            key: key.0,
            command: READ_KEY_INFO,
            ..Message::default()
        })?;
        Ok(described.info)
    }

    /// The bytes of `key` read as a value of `info`, which the caller checked the SMC holds.
    fn bytes(&self, key: Key, info: KeyInfo) -> io::Result<[u8; MESSAGE_BYTES_SIZE]> {
        assert!(info.size > 0);
        assert!(info.size <= VALUE_SIZE_MAX);

        let answer = self.ask(&Message {
            key: key.0,
            info,
            command: READ_BYTES,
            ..Message::default()
        })?;
        Ok(answer.bytes)
    }

    fn ask(&self, question: &Message) -> io::Result<Message> {
        assert_ne!(0, self.0);
        assert!(COMMANDS.contains(&question.command));
        assert_eq!(0, question.padding);
        assert_eq!([0; 3], question.info.padding);

        let mut answer = Message::default();
        let mut answer_size = size_of::<Message>();
        let returned = unsafe {
            IOConnectCallStructMethod(
                self.0,
                CALL_STRUCT,
                std::ptr::from_ref(question).cast(),
                size_of::<Message>(),
                (&raw mut answer).cast(),
                &raw mut answer_size,
            )
        };

        let answer = ask_answered(question, returned, answer_size, answer)?;
        assert_eq!(size_of::<Message>(), answer_size);
        assert_eq!(SUCCESS, answer.result);
        Ok(answer)
    }
}

/// A key, and the info the SMC holds it with.
#[derive(Clone, Copy)]
struct Held {
    key: Key,
    info: KeyInfo,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key(u32);

const KEY_COUNT: Key = Key::named(*b"#KEY");
const FAN_COUNT: Key = Key::named(*b"FNum");
const SYSTEM_POWER: Key = Key::named(*b"PSTR");
/// The prefixes of the keys of the chip's CPU, GPU and fabric temperature sensors, as
/// sysmon-exporter's `sensors.go` groups them on Apple silicon, since Apple documents no SMC key.
const SOC_TEMPERATURE_PREFIXES: [[u8; 2]; 8] = [
    *b"TC", *b"Tp", *b"Tf", *b"Ts", *b"Te", *b"TR", *b"Tg", *b"TP",
];

/// The most fans the SMC's keys can name, since a fan's speed key names it with one digit.
const FANS_MAX: u8 = 10;
const _: () = assert!((b'0' + FANS_MAX - 1).is_ascii_digit());

/// The most keys open lists, past four times the 3,626 the SMC of an M5 Pro lists.
const KEYS_MAX: u32 = 16_384;

impl Key {
    const fn named(name: [u8; 4]) -> Self {
        let mut index = 0;
        while index < name.len() {
            assert!(name[index].is_ascii_graphic());
            index += 1;
        }

        Key(u32::from_be_bytes(name))
    }

    /// The key of the speed of the fan at `fan_index`.
    fn fan_speed(fan_index: u8) -> Self {
        assert!(fan_index < FANS_MAX);

        let key = Key::named([b'F', b'0' + fan_index, b'A', b'c']);
        // Open looks for the SoC's sensors among every key, so no fan's key may look like one.
        assert!(!key.is_soc_temperature());
        key
    }

    fn is_soc_temperature(self) -> bool {
        let [first, second, _, _] = self.0.to_be_bytes();
        SOC_TEMPERATURE_PREFIXES.contains(&[first, second])
    }
}

impl std::fmt::Debug for Key {
    /// Writes the key's four characters, escaping any that is not printable.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0.to_be_bytes() {
            write!(formatter, "{}", char::from(byte).escape_default())?;
        }

        Ok(())
    }
}

/// What the SMC says of a key, as the SMC lays it out. The padding is a field of its own, so a
/// question written out carries zeroes there.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct KeyInfo {
    size: u32,
    kind: u32,
    _attributes: u8,
    padding: [u8; 3],
}

const _: () = assert!(size_of::<KeyInfo>() == 12);
const _: () = assert!(std::mem::offset_of!(KeyInfo, size) == 0);
const _: () = assert!(std::mem::offset_of!(KeyInfo, kind) == 4);
const _: () = assert!(std::mem::offset_of!(KeyInfo, _attributes) == 8);
const _: () = assert!(std::mem::offset_of!(KeyInfo, padding) == 9);

/// The most bytes a key's value has, the 32 a message's `bytes` hold.
const VALUE_SIZE_MAX: u32 = 32;
/// The size of a message's `bytes`, which hold a key's value.
const MESSAGE_BYTES_SIZE: usize = 32;

// Rust converts no integer to another type in a constant, so each size is held to 32, which
// keeps the largest value a key has within a message.
const _: () = assert!(VALUE_SIZE_MAX == 32);
const _: () = assert!(MESSAGE_BYTES_SIZE == 32);

const FLOAT: KeyInfo = KeyInfo::of(*b"flt ", 4);
const UNSIGNED_8: KeyInfo = KeyInfo::of(*b"ui8 ", 1);
const UNSIGNED_32: KeyInfo = KeyInfo::of(*b"ui32", 4);

impl KeyInfo {
    const fn of(kind: [u8; 4], size: u32) -> Self {
        assert!(size > 0);
        assert!(size <= VALUE_SIZE_MAX);

        Self {
            size,
            kind: u32::from_be_bytes(kind),
            _attributes: 0,
            padding: [0; 3],
        }
    }

    /// Whether `self`, which the SMC gave, is `other`, one of the types a read names.
    fn is(self, other: KeyInfo) -> bool {
        assert!(other.size > 0);
        assert!(other.size <= VALUE_SIZE_MAX);

        if self.kind != other.kind {
            return false;
        }

        self.size == other.size
    }
}

/// A question to the SMC and its answer, as the SMC lays them out. The padding is a field of
/// its own, so a question written out carries zeroes there.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Message {
    key: u32,
    _version_and_limits: [u8; 24],
    info: KeyInfo,
    result: u8,
    _status: u8,
    command: u8,
    padding: u8,
    index: u32,
    bytes: [u8; MESSAGE_BYTES_SIZE],
}

const _: () = assert!(size_of::<Message>() == 80);
const _: () = assert!(std::mem::offset_of!(Message, key) == 0);
const _: () = assert!(std::mem::offset_of!(Message, info) == 28);
const _: () = assert!(std::mem::offset_of!(Message, result) == 40);
const _: () = assert!(std::mem::offset_of!(Message, command) == 42);
const _: () = assert!(std::mem::offset_of!(Message, padding) == 43);
const _: () = assert!(std::mem::offset_of!(Message, index) == 44);
const _: () = assert!(std::mem::offset_of!(Message, bytes) == 48);

/// The selector of the SMC's method that takes a question and writes back its answer. Apple
/// documents no part of the SMC's interface, so this selector, the commands below and the
/// message's layout hold only as far as the SMC of an M5 Pro answers them.
const CALL_STRUCT: u32 = 2;
const READ_BYTES: u8 = 5;
const READ_INDEX: u8 = 8;
const READ_KEY_INFO: u8 = 9;
const COMMANDS: [u8; 3] = [READ_BYTES, READ_INDEX, READ_KEY_INFO];

/// The result an answer holds where the SMC did what it was asked.
const SUCCESS: u8 = 0;
/// The result an answer holds where the SMC has no key of the name asked for.
const KEY_NOT_FOUND: u8 = 132;

impl Smc {
    pub fn open() -> io::Result<Self> {
        let connection = Connection::open()?;
        let power = connection.float_key(SYSTEM_POWER)?;
        let fan_count = connection.fan_count()?;
        let fans = (0..fan_count)
            .map(|fan_index| connection.float_key(Key::fan_speed(fan_index)))
            .collect::<io::Result<Vec<FloatKey>>>()?;
        let soc_sensor = connection.hottest_soc_sensor()?;
        assert!(soc_sensor.0.is_soc_temperature());
        let fan_rpm = Vec::with_capacity(fans.len());
        assert_eq!(usize::from(fan_count), fans.len());
        assert!(fans.len() <= fan_rpm.capacity());

        Ok(Self {
            connection,
            power,
            fans,
            soc_sensor,
            power_watts: 0.0,
            fan_rpm,
            soc_celsius: 0.0,
        })
    }

    pub fn read(&mut self) -> io::Result<()> {
        assert!(self.fans.len() <= usize::from(FANS_MAX));
        assert!(self.fans.len() <= self.fan_rpm.capacity());

        self.power_watts = self.connection.read_float(self.power)?;
        self.fan_rpm.clear();
        for &fan in &self.fans {
            push_without_growing(&mut self.fan_rpm, self.connection.read_float(fan)?);
        }
        assert_eq!(self.fans.len(), self.fan_rpm.len());
        self.soc_celsius = self.connection.read_float(self.soc_sensor)?;

        Ok(())
    }

    /// The power the Mac drew at the last read, in watts.
    #[must_use]
    pub fn power_watts(&self) -> f32 {
        assert_eq!(FloatKey(SYSTEM_POWER), self.power);

        self.power_watts
    }

    /// The speed of each fan at the last read, in revolutions a minute.
    #[must_use]
    pub fn fan_rpm(&self) -> &[f32] {
        // Empty until the first read, and one speed for each fan after it.
        assert!(self.fan_rpm.len() <= self.fans.len());
        assert!(self.fans.len() <= usize::from(FANS_MAX));

        &self.fan_rpm
    }

    /// The temperature at the last read of the chip's sensor open picked, in degrees Celsius.
    #[must_use]
    pub fn soc_celsius(&self) -> f32 {
        assert!(self.soc_sensor.0.is_soc_temperature());

        self.soc_celsius
    }
}

/// What the SMC answered `question` with, from what the call returned, the size of the answer
/// it wrote, and the answer: the answer, or an error where the call failed, the answer is
/// short, the SMC has no key the question names, or the SMC refused the question.
fn ask_answered(
    question: &Message,
    returned: KernReturn,
    answer_size: usize,
    answer: Message,
) -> io::Result<Message> {
    assert!(COMMANDS.contains(&question.command));

    kern_return_check(returned, "calling the SMC")?;
    if answer_size != size_of::<Message>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the SMC answered with {answer_size} bytes, where a message holds {}",
                size_of::<Message>()
            ),
        ));
    }
    if answer.result == KEY_NOT_FOUND {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the SMC has no key {:?} for command {} at index {}",
                Key(question.key),
                question.command,
                question.index
            ),
        ));
    }
    if answer.result != SUCCESS {
        return Err(io::Error::other(format!(
            "the SMC refused a question with result {}",
            answer.result
        )));
    }

    assert_eq!(size_of::<Message>(), answer_size);
    assert_eq!(SUCCESS, answer.result);
    Ok(answer)
}

/// Checks that the SMC holds a key as a value of `wanted`, from the info `held` gives.
fn held_check_of(held: Held, wanted: KeyInfo) -> io::Result<()> {
    if !held.info.is(wanted) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the SMC holds {:?} as {:?}, which is not {wanted:?}",
                held.key, held.info
            ),
        ));
    }

    assert!(held.info.is(wanted));
    Ok(())
}

/// Checks that the SMC counts no more fans than its keys can name.
fn fan_count_check(fan_count: u8) -> io::Result<()> {
    if fan_count > FANS_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the SMC counts {fan_count} fans, past the {FANS_MAX} its keys can name"),
        ));
    }

    assert!(fan_count <= FANS_MAX);
    Ok(())
}

/// Checks that the SMC lists no more keys than open lists.
fn key_count_check(key_count: u32) -> io::Result<()> {
    if key_count > KEYS_MAX {
        return Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("the SMC lists {key_count} keys, past the {KEYS_MAX} open lists"),
        ));
    }

    assert!(key_count <= KEYS_MAX);
    Ok(())
}

/// The hotter of `hottest`, the hottest reading so far, and `reading`, leaving out a reading
/// that is not a finite number.
fn hottest_soc_sensor_of(
    hottest: Option<(FloatKey, f32)>,
    reading: (FloatKey, f32),
) -> Option<(FloatKey, f32)> {
    let (sensor, celsius) = reading;
    assert!(sensor.0.is_soc_temperature());

    if !celsius.is_finite() {
        return hottest;
    }

    if let Some((hottest_sensor, hottest_celsius)) = hottest {
        assert!(hottest_sensor.0.is_soc_temperature());
        assert!(hottest_celsius.is_finite());
        if hottest_celsius >= celsius {
            return hottest;
        }
    }

    Some(reading)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOObjectRetain(object: MachPort) -> KernReturn;
        fn IOObjectGetUserRetainCount(object: MachPort) -> u32;
    }

    #[test]
    fn picks_the_hottest_of_the_sensors_read() {
        // Goal: the SoC sensor picked at open is the one reading the highest temperature.
        // Method: pick among three sensors reading 41, 63.5 and 52 degrees, the hottest second.
        let readings = [
            (FloatKey(Key::named(*b"Tp01")), 41.0),
            (FloatKey(Key::named(*b"Tg05")), 63.5),
            (FloatKey(Key::named(*b"TP0a")), 52.0),
        ];

        let hottest = readings.into_iter().fold(None, hottest_soc_sensor_of);

        assert_eq!(Some((FloatKey(Key::named(*b"Tg05")), 63.5)), hottest);
    }

    #[test]
    fn leaves_a_sensor_reading_no_number_out_of_the_hottest() {
        // Goal: a sensor whose reading is not a number is never the hottest, though NaN orders
        // above every temperature.
        // Method: pick from a sensor reading 41 degrees and one reading NaN.
        let readings = [
            (FloatKey(Key::named(*b"Tp01")), 41.0),
            (FloatKey(Key::named(*b"Tg05")), f32::NAN),
        ];

        let hottest = readings.into_iter().fold(None, hottest_soc_sensor_of);

        assert_eq!(Some((FloatKey(Key::named(*b"Tp01")), 41.0)), hottest);
    }

    #[test]
    fn refuses_a_service_iokit_does_not_have() {
        // Goal: looking up a class of service IOKit does not have is a service not found, as
        // on a Mac with no SMC.
        // Method: look up a class no driver registers.
        let found = Service::matching(c"NoSuchService");

        assert_eq!(io::ErrorKind::NotFound, found.unwrap_err().kind());
    }

    #[test]
    fn releases_the_service_it_holds() {
        // Goal: dropping a service releases IOKit's object once, so no service leaks.
        // Method: find the platform expert, which every Mac has and nothing else in the
        // collector looks up, take a second reference to it, drop the service, and count the
        // references this process holds.
        let service = Service::matching(c"IOPlatformExpertDevice").unwrap();
        let object = service.0;
        let retained = unsafe { IOObjectRetain(object) };

        drop(service);

        let references = unsafe { IOObjectGetUserRetainCount(object) };
        let released = unsafe { IOObjectRelease(object) };
        assert_eq!(kernel::KERN_SUCCESS, retained);
        assert_eq!(kernel::KERN_SUCCESS, released);
        assert_eq!(1, references);
    }

    #[test]
    fn names_the_speed_key_of_the_first_fan_by_its_digit() {
        // Goal: the first fan's speed key names the fan by the digit 0.
        // Method: name the first fan's key.
        assert_eq!(Key::named(*b"F0Ac"), Key::fan_speed(0));
    }

    #[test]
    fn names_the_speed_key_of_the_tenth_fan_by_its_digit() {
        // Goal: the tenth fan's speed key names the fan by the digit 9, the last a key can
        // name.
        // Method: name the tenth fan's key.
        assert_eq!(Key::named(*b"F9Ac"), Key::fan_speed(9));
    }

    #[test]
    fn names_a_key_by_its_characters() {
        // Goal: a key reads as its four characters in an error.
        // Method: format PSTR.
        let named = format!("{SYSTEM_POWER:?}");

        assert_eq!("PSTR", named);
    }

    #[test]
    fn names_a_key_by_the_escape_of_a_character_that_is_not_printable() {
        // Goal: a byte that is not printable reads as its escape, so a key the SMC lists with
        // a zero byte shows it.
        // Method: format a key of T followed by three zero bytes.
        let unprintable = format!("{:?}", Key(0x5400_0000));

        assert_eq!(r"T\u{0}\u{0}\u{0}", unprintable);
    }

    #[test]
    #[should_panic(expected = "is_ascii_graphic")]
    fn refuses_to_name_a_key_with_a_character_that_is_not_printable() {
        // Goal: naming a key with a byte that is not a printable character fails, so every key
        // the collector names prints as its four characters.
        // Method: name a key whose last byte is a nul.
        Key::named(*b"TC0\0");
    }

    #[test]
    fn takes_as_many_fans_as_its_keys_can_name() {
        // Goal: a fan count up to the ten fans the keys can name passes the check.
        // Method: check a count of ten.
        assert!(fan_count_check(FANS_MAX).is_ok());
    }

    #[test]
    fn refuses_more_fans_than_its_keys_can_name() {
        // Goal: an eleventh fan is an error, since its key would need a second digit.
        // Method: check a count of eleven.
        let checked = fan_count_check(FANS_MAX + 1);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn takes_as_many_keys_as_its_bound() {
        // Goal: a key count up to the bound passes the check.
        // Method: check a count equal to the bound.
        assert!(key_count_check(KEYS_MAX).is_ok());
    }

    #[test]
    fn refuses_more_keys_than_its_bound() {
        // Goal: a key count past the bound is an error, so listing the keys at open has a
        // bound.
        // Method: check a count one past the bound.
        let checked = key_count_check(KEYS_MAX + 1);

        assert_eq!(io::ErrorKind::QuotaExceeded, checked.unwrap_err().kind());
    }

    /// A question for the info of PSTR, as `info` asks it.
    fn power_info_question() -> Message {
        Message {
            key: SYSTEM_POWER.0,
            command: READ_KEY_INFO,
            ..Message::default()
        }
    }

    #[test]
    fn takes_a_whole_answer_holding_success() {
        // Goal: an answer the SMC wrote in full with result 0 is its answer.
        // Method: interpret a call that succeeded and wrote a whole message holding PSTR's
        // info.
        let question = power_info_question();
        let answer = Message {
            info: FLOAT,
            ..question
        };

        let answered = ask_answered(
            &question,
            kernel::KERN_SUCCESS,
            size_of::<Message>(),
            answer,
        );

        assert!(answered.unwrap().info.is(FLOAT));
    }

    #[test]
    fn refuses_an_answer_for_a_missing_key_as_not_found() {
        // Goal: result 132, which the SMC gives for a key it does not hold, is a key not found,
        // since every question names a key or an index the SMC should have.
        // Method: interpret a whole answer holding 132, which the SMC of an M5 Pro gave for the
        // key ZZZZ.
        let question = power_info_question();
        let answer = Message {
            result: KEY_NOT_FOUND,
            ..question
        };

        let answered = ask_answered(
            &question,
            kernel::KERN_SUCCESS,
            size_of::<Message>(),
            answer,
        );

        assert_eq!(io::ErrorKind::NotFound, answered.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_answer_holding_another_result() {
        // Goal: a result besides success and a missing key is an error, never a missing key.
        // Method: interpret a whole answer holding 135, the result the SMC of an M5 Pro gave a
        // read whose size was not its key's.
        let question = power_info_question();
        let answer = Message {
            result: 135,
            ..question
        };

        let answered = ask_answered(
            &question,
            kernel::KERN_SUCCESS,
            size_of::<Message>(),
            answer,
        );

        assert_eq!(io::ErrorKind::Other, answered.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_answer_shorter_than_a_message() {
        // Goal: an answer the SMC wrote only in part is an error, since the rest of the
        // message holds no answer.
        // Method: interpret a call that succeeded and wrote half a message.
        let question = power_info_question();

        let answered = ask_answered(
            &question,
            kernel::KERN_SUCCESS,
            size_of::<Message>() / 2,
            question,
        );

        assert_eq!(io::ErrorKind::InvalidData, answered.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_call_the_kernel_failed() {
        // Goal: a call to the SMC that the kernel failed is an error.
        // Method: interpret 0xe00002c2, which the kernel of an M5 Pro returned for a command the
        // SMC does not take.
        let question = power_info_question();

        let answered = ask_answered(&question, 0xe000_02c2_u32.cast_signed(), 0, question);

        assert_eq!(io::ErrorKind::Other, answered.unwrap_err().kind());
    }

    #[test]
    fn takes_a_key_held_as_the_type_read() {
        // Goal: a key the SMC holds as the type it is read as passes the check.
        // Method: check the info of a float key against the float it is read as.
        let held = Held {
            key: SYSTEM_POWER,
            info: FLOAT,
        };

        assert!(held_check_of(held, FLOAT).is_ok());
    }

    #[test]
    fn refuses_a_key_held_as_another_type() {
        // Goal: a key the SMC holds as another type than the one read is an error.
        // Method: check the info of the fan count, a ui8, against a float.
        let held = Held {
            key: FAN_COUNT,
            info: UNSIGNED_8,
        };

        let checked = held_check_of(held, FLOAT);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_key_read_as_a_type_it_does_not_hold() {
        // Goal: a key read as another type than the one the SMC holds it as is an error, since
        // the SMC answers such a read with the key's bytes.
        // Method: read the fan count, a ui8, as a float, which the SMC of an M5 Pro answers
        // with result 0.
        let connection = Connection::open().unwrap();

        let read = connection.read(FAN_COUNT, FLOAT);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_float_key_the_smc_holds_as_another_type() {
        // Goal: a key open would read each tick as a float, but the SMC holds as another type,
        // is an error at open, never a float made of the key's bytes.
        // Method: take the fan count, a ui8 on an M5 Pro, as a float key.
        let connection = Connection::open().unwrap();

        let taken = connection.float_key(FAN_COUNT);

        assert_eq!(io::ErrorKind::InvalidData, taken.unwrap_err().kind());
    }

    #[test]
    fn finds_no_info_for_a_key_the_smc_does_not_hold() {
        // Goal: asking the SMC about a key it does not hold is a key not found, never a panic
        // over the result the SMC answers with.
        // Method: ask for the info of ZZZZ, which the SMC of an M5 Pro answers with result 132.
        let connection = Connection::open().unwrap();

        let info = connection.info(Key::named(*b"ZZZZ"));

        assert_eq!(io::ErrorKind::NotFound, info.unwrap_err().kind());
    }

    #[test]
    fn reads_the_power_the_mac_draws() {
        // Goal: the SMC's system power reads as a draw a running Mac can have.
        // Method: one read, held above zero and below 500 W. An idle M5 Pro on AC power has
        // read 0.73 W, so the floor is zero.
        let mut smc = Smc::open().unwrap();

        smc.read().unwrap();

        assert!(smc.power_watts() > 0.0, "{} W", smc.power_watts());
        assert!(smc.power_watts() < 500.0, "{} W", smc.power_watts());
    }

    #[test]
    fn reads_a_speed_for_every_fan() {
        // Goal: each read takes one speed for every fan the SMC counts.
        // Method: one read, against the fan count's byte read on a connection of the test's
        // own.
        let mut smc = Smc::open().unwrap();
        let [fan_count, ..] = Connection::open()
            .unwrap()
            .bytes(FAN_COUNT, UNSIGNED_8)
            .unwrap();

        smc.read().unwrap();

        assert_eq!(usize::from(fan_count), smc.fan_rpm().len());
    }

    #[test]
    fn reads_each_fan_speed_in_rpm() {
        // Goal: each fan's speed reads as revolutions a minute a fan can turn at.
        // Method: one read, each speed held at or above 0, a fan standing still, and below
        // 10,000 rpm.
        let mut smc = Smc::open().unwrap();

        smc.read().unwrap();

        for &rpm in smc.fan_rpm() {
            assert!(rpm >= 0.0, "{rpm} rpm");
            assert!(rpm < 10_000.0, "{rpm} rpm");
        }
    }

    #[test]
    fn reads_the_temperature_of_a_soc_sensor() {
        // Goal: the SoC sensor reads as a temperature a running chip can have.
        // Method: one read, held above 15 and below 115 degrees.
        let mut smc = Smc::open().unwrap();

        smc.read().unwrap();

        assert!(smc.soc_celsius() > 15.0, "{} °C", smc.soc_celsius());
        assert!(smc.soc_celsius() < 115.0, "{} °C", smc.soc_celsius());
    }
}
