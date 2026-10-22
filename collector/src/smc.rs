use std::ffi::{CStr, c_char, c_void};
use std::io;

use crate::kernel::{self, KernReturn, MachPort, kern_return_check};
use crate::push_without_growing;

pub struct Smc {
    controller: Controller,
    power: FloatKey,
    fans: Vec<FloatKey>,
    soc_sensor: FloatKey,
    cpu_sensor: Option<FloatKey>,
    gpu_sensor: Option<FloatKey>,
    power_watts: f32,
    fan_rpm: Vec<f32>,
    soc_celsius: f32,
    cpu_celsius: Option<f32>,
    gpu_celsius: Option<f32>,
}

/// A key the SMC holds as a float, as its info showed when the key was checked, so a read of
/// it each tick need not ask again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FloatKey(Key);

/// A connection to the SMC, through a port that is never 0 and that only this connection
/// closes.
struct Connection(MachPort);

/// What `Smc` asks its questions of: the SMC, or in a test the answers an M5 Pro's SMC gave.
enum Controller {
    Connection(Connection),
    #[cfg(test)]
    Fake(tests::FakeSmc),
}

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

        Ok(Self(service))
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

        let mut port = 0;
        let opened = unsafe { IOServiceOpen(service.0, kernel::task_self(), 0, &raw mut port) };
        kern_return_check(opened, "connecting to the SMC")?;
        if port == 0 {
            return Err(io::Error::other("connecting to the SMC gave no connection"));
        }

        Ok(Self(port))
    }

    fn ask(&self, question: &Message) -> io::Result<Message> {
        assert_ne!(0, self.0);

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

        ask_answered(question, returned, answer_size, answer)
    }
}

impl Controller {
    fn hottest_sensors(&self) -> io::Result<Sensors> {
        let key_count = self.key_count()?;
        assert!(key_count <= KEYS_MAX);

        let mut hottest = Hottest::default();
        for index in 0..key_count {
            let key = self.key_at(index)?;
            if !key.is_soc_temperature() {
                continue;
            }
            if !self.info(key)?.is(FLOAT) {
                continue;
            }

            let sensor = FloatKey(key);
            hottest = hottest.with((sensor, self.read_float(sensor)?));
        }

        hottest.sensors()
    }

    fn fan_count(&self) -> io::Result<u8> {
        const { assert!(UNSIGNED_8.size == 1) };
        let [fan_count, ..] = self.read(FAN_COUNT, UNSIGNED_8)?;
        fan_count_check(fan_count)?;

        Ok(fan_count)
    }

    fn key_count(&self) -> io::Result<u32> {
        const { assert!(UNSIGNED_32.size == 4) };
        let [first, second, third, fourth, ..] = self.read(KEY_COUNT, UNSIGNED_32)?;

        let key_count = u32::from_be_bytes([first, second, third, fourth]);
        key_count_check(key_count)?;

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

        held_check_of(held, wanted)
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
        assert!(COMMANDS.contains(&question.command));
        assert_eq!(0, question.padding);
        assert_eq!([0; 3], question.info.padding);
        // Only a read of bytes names a size, and only a read by index names an index.
        if question.command == READ_BYTES {
            assert!(question.info.size > 0);
        } else {
            assert_eq!(0, question.info.size);
        }
        if question.command == READ_INDEX {
            assert!(question.index < KEYS_MAX);
        } else {
            assert_eq!(0, question.index);
        }

        match self {
            Controller::Connection(connection) => connection.ask(question),
            #[cfg(test)]
            Controller::Fake(fake) => fake.ask(question),
        }
    }
}

/// Writes the keys this Mac's SMC lists, and the info and the bytes of each key open or a read
/// asks about, as the SMC tests read them: the count of keys, a little-endian u32, then each
/// key, then for each key asked about an answer of `fixture_answer`.
pub fn fixture(into: &mut impl io::Write) -> io::Result<()> {
    let controller = Controller::Connection(Connection::open()?);
    let key_count = controller.key_count()?;
    let fan_count = controller.fan_count()?;

    into.write_all(&key_count.to_le_bytes())?;
    for index in 0..key_count {
        into.write_all(&controller.key_at(index)?.0.to_be_bytes())?;
    }

    for key in [KEY_COUNT, FAN_COUNT, SYSTEM_POWER] {
        fixture_answer(&controller, key, into)?;
    }
    for fan_index in 0..fan_count {
        fixture_answer(&controller, Key::fan_speed(fan_index), into)?;
    }
    for index in 0..key_count {
        let key = controller.key_at(index)?;
        if key.is_soc_temperature() {
            fixture_answer(&controller, key, into)?;
        }
    }
    Ok(())
}

/// Writes `key`, a big-endian u32, then its info, its size and its kind each a little-endian
/// u32 and its attributes a byte followed by three zero bytes, then its bytes.
fn fixture_answer(controller: &Controller, key: Key, into: &mut impl io::Write) -> io::Result<()> {
    let key_info = controller.info(key)?;
    if key_info.size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the SMC holds {key:?} with a size of 0"),
        ));
    }
    if key_info.size > VALUE_SIZE_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the SMC holds {key:?} with a size of {}", key_info.size),
        ));
    }

    into.write_all(&key.0.to_be_bytes())?;
    into.write_all(&key_info.size.to_le_bytes())?;
    into.write_all(&key_info.kind.to_le_bytes())?;
    into.write_all(&[key_info.attributes, 0, 0, 0])?;
    into.write_all(&controller.bytes(key, key_info)?)
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
const SOC_TEMPERATURE_PREFIXES: [[u8; 2]; 9] = [
    *b"TC", *b"Tp", *b"Tf", *b"Ts", *b"Te", *b"TR", *b"Tg", *b"TP", *b"Tm",
];
/// The prefixes of the keys of the CPU's cores, the performance cores' `Tp` and the super
/// cores' `Tm`, and of the GPU's die. A probe on an M5 Pro read every float `T` key before and
/// after a GPU load and a CPU load as the user. Each `Tg` key rose most under the GPU load, and
/// each `Tm` and `Tp` key under the CPU load, which the other keys did not show. Which cluster a
/// prefix names is inferred from the size of its rise, never read from the SMC.
const CPU_TEMPERATURE_PREFIXES: [[u8; 2]; 2] = [*b"Tp", *b"Tm"];
const GPU_TEMPERATURE_PREFIXES: [[u8; 2]; 1] = [*b"Tg"];

/// The first letter of a fan's speed key.
const FAN_SPEED_KEY_FIRST: u8 = b'F';

// Open looks for the SoC's sensors among every key, so no fan's speed key may look like one.
const _: () = {
    let mut index = 0;
    while index < SOC_TEMPERATURE_PREFIXES.len() {
        assert!(SOC_TEMPERATURE_PREFIXES[index][0] != FAN_SPEED_KEY_FIRST);
        index += 1;
    }
};

/// The most fans the SMC's keys can name, since a fan's speed key names it with one digit.
pub const FANS_MAX: u8 = 10;
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

        Key::named([FAN_SPEED_KEY_FIRST, b'0' + fan_index, b'A', b'c'])
    }

    fn is_soc_temperature(self) -> bool {
        let [first, second, _, _] = self.0.to_be_bytes();
        SOC_TEMPERATURE_PREFIXES.contains(&[first, second])
    }

    fn is_cpu_temperature(self) -> bool {
        let [first, second, _, _] = self.0.to_be_bytes();
        CPU_TEMPERATURE_PREFIXES.contains(&[first, second])
    }

    fn is_gpu_temperature(self) -> bool {
        let [first, second, _, _] = self.0.to_be_bytes();
        GPU_TEMPERATURE_PREFIXES.contains(&[first, second])
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
    attributes: u8,
    padding: [u8; 3],
}

const _: () = assert!(size_of::<KeyInfo>() == 12);
const _: () = assert!(std::mem::offset_of!(KeyInfo, size) == 0);
const _: () = assert!(std::mem::offset_of!(KeyInfo, kind) == 4);
const _: () = assert!(std::mem::offset_of!(KeyInfo, attributes) == 8);
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
/// The types a read names.
const READ_TYPES: [KeyInfo; 3] = [FLOAT, UNSIGNED_8, UNSIGNED_32];

impl KeyInfo {
    const fn of(kind: [u8; 4], size: u32) -> Self {
        assert!(size > 0);
        assert!(size <= VALUE_SIZE_MAX);

        Self {
            size,
            kind: u32::from_be_bytes(kind),
            attributes: 0,
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
        Self::open_with(Controller::Connection(Connection::open()?))
    }

    /// Opens the SMC that `controller` answers for.
    fn open_with(controller: Controller) -> io::Result<Self> {
        let power = controller.float_key(SYSTEM_POWER)?;
        let fan_count = controller.fan_count()?;
        let fans = (0..fan_count)
            .map(|fan_index| controller.float_key(Key::fan_speed(fan_index)))
            .collect::<io::Result<Vec<FloatKey>>>()?;
        let sensors = controller.hottest_sensors()?;
        let fan_rpm = Vec::with_capacity(fans.len());

        Ok(Self {
            controller,
            power,
            fans,
            soc_sensor: sensors.soc,
            cpu_sensor: sensors.cpu,
            gpu_sensor: sensors.gpu,
            power_watts: 0.0,
            fan_rpm,
            soc_celsius: 0.0,
            cpu_celsius: None,
            gpu_celsius: None,
        })
    }

    pub fn read(&mut self) -> io::Result<()> {
        assert!(self.fans.len() <= usize::from(FANS_MAX));
        assert!(self.fans.len() <= self.fan_rpm.capacity());

        self.power_watts = self.controller.read_float(self.power)?;
        self.fan_rpm.clear();
        for &fan in &self.fans {
            push_without_growing(&mut self.fan_rpm, self.controller.read_float(fan)?);
        }
        self.soc_celsius = self.controller.read_float(self.soc_sensor)?;
        self.cpu_celsius = self
            .cpu_sensor
            .map(|sensor| self.controller.read_float(sensor))
            .transpose()?
            .and_then(usable_celsius);
        self.gpu_celsius = self
            .gpu_sensor
            .map(|sensor| self.controller.read_float(sensor))
            .transpose()?
            .and_then(usable_celsius);

        Ok(())
    }

    /// The power the Mac drew at the last read, in watts.
    #[must_use]
    pub fn power_watts(&self) -> f32 {
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

    /// The temperature at the last read of the hottest sensor of the CPU's cores open picked,
    /// in degrees Celsius, or `None` where the SMC lists no such sensor.
    #[must_use]
    pub fn cpu_celsius(&self) -> Option<f32> {
        if let Some(sensor) = self.cpu_sensor {
            assert!(sensor.0.is_cpu_temperature());
        }

        self.cpu_celsius
    }

    /// The temperature at the last read of the hottest sensor of the GPU open picked, in
    /// degrees Celsius, or `None` where the SMC lists no such sensor.
    #[must_use]
    pub fn gpu_celsius(&self) -> Option<f32> {
        if let Some(sensor) = self.gpu_sensor {
            assert!(sensor.0.is_gpu_temperature());
        }

        self.gpu_celsius
    }
}

/// `celsius` where a CPU or GPU sensor read a temperature a die can have, which is above 0, and
/// `None` for a reading of 0, which the ring keeps for a sensor the SMC does not list, or a
/// negative one or one that is not a number.
#[must_use]
pub fn usable_celsius(celsius: f32) -> Option<f32> {
    if celsius.is_finite() && celsius > 0.0 {
        return Some(celsius);
    }

    None
}

/// The sensors open picked: the hottest of the chip, and the hottest of the CPU's cores and of
/// the GPU where the SMC lists any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sensors {
    soc: FloatKey,
    cpu: Option<FloatKey>,
    gpu: Option<FloatKey>,
}

/// The hottest reading so far of the chip's sensors, of the CPU's cores and of the GPU, each
/// with its sensor.
#[derive(Clone, Copy, Debug, Default)]
struct Hottest {
    soc: Option<(FloatKey, f32)>,
    cpu: Option<(FloatKey, f32)>,
    gpu: Option<(FloatKey, f32)>,
}

impl Hottest {
    /// What is hottest once `reading`, of a sensor of the chip, is counted.
    fn with(self, reading: (FloatKey, f32)) -> Self {
        let (sensor, _) = reading;
        assert!(sensor.0.is_soc_temperature());

        let mut hottest = self;
        hottest.soc = hottest_soc_sensor_of(self.soc, reading);
        if sensor.0.is_cpu_temperature() {
            hottest.cpu = hottest_soc_sensor_of(self.cpu, reading);
        }
        if sensor.0.is_gpu_temperature() {
            hottest.gpu = hottest_soc_sensor_of(self.gpu, reading);
        }

        hottest
    }

    /// The sensors of the readings, or an error where the SMC has no sensor of the chip. A Mac
    /// may lack the CPU's or the GPU's, which is not an error.
    fn sensors(self) -> io::Result<Sensors> {
        let Some((soc, celsius)) = self.soc else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the SMC has no SoC temperature sensor",
            ));
        };

        assert!(celsius.is_finite());
        Ok(Sensors {
            soc,
            cpu: self.cpu.map(|(sensor, _)| sensor),
            gpu: self.gpu.map(|(sensor, _)| sensor),
        })
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

    Ok(answer)
}

/// Checks that the SMC holds a key as a value of `wanted`, from the info `held` gives.
fn held_check_of(held: Held, wanted: KeyInfo) -> io::Result<()> {
    assert!(READ_TYPES.iter().any(|read_type| read_type.is(wanted)));

    if !held.info.is(wanted) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the SMC holds {:?} as {:?}, which is not {wanted:?}",
                held.key, held.info
            ),
        ));
    }

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
    use crate::usize_from_u32;

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
    fn picks_a_hottest_sensor_for_the_cpu_and_one_for_the_gpu() {
        // Goal: the CPU's and the GPU's sensors are each the hottest of their own keys, though
        // a sensor of the other, or of the fabric, reads hotter.
        // Method: pick among a CPU core sensor at 70, another at 80, a GPU sensor at 60,
        // another at 65 and a fabric sensor at 90 degrees.
        let readings = [
            (FloatKey(Key::named(*b"Tm00")), 70.0),
            (FloatKey(Key::named(*b"Tp04")), 80.0),
            (FloatKey(Key::named(*b"Tg08")), 60.0),
            (FloatKey(Key::named(*b"Tg0C")), 65.0),
            (FloatKey(Key::named(*b"Tf00")), 90.0),
        ];

        let hottest = readings.into_iter().fold(Hottest::default(), Hottest::with);

        assert_eq!(Some((FloatKey(Key::named(*b"Tf00")), 90.0)), hottest.soc);
        assert_eq!(Some((FloatKey(Key::named(*b"Tp04")), 80.0)), hottest.cpu);
        assert_eq!(Some((FloatKey(Key::named(*b"Tg0C")), 65.0)), hottest.gpu);
    }

    #[test]
    fn leaves_the_cpu_and_gpu_without_a_sensor_where_the_smc_has_none() {
        // Goal: a Mac whose SMC lists no CPU or GPU sensor picks none, rather than a sensor of
        // another part.
        // Method: pick among one fabric sensor.
        let readings = [(FloatKey(Key::named(*b"Tf00")), 50.0)];

        let hottest = readings.into_iter().fold(Hottest::default(), Hottest::with);

        assert_eq!(None, hottest.cpu);
        assert_eq!(None, hottest.gpu);
    }

    #[test]
    fn names_the_keys_of_the_cpu_cores_and_of_the_gpu() {
        // Goal: the CPU's keys are the super and performance cores' and the GPU's are the GPU
        // die's, with no key in both.
        // Method: classify one key of each prefix the probe on an M5 Pro tied to a part.
        let super_cores = Key::named(*b"Tm0K");
        let performance_cores = Key::named(*b"Tp0C");
        let gpu = Key::named(*b"Tg1c");
        let fabric = Key::named(*b"Tf00");

        assert!(super_cores.is_cpu_temperature());
        assert!(performance_cores.is_cpu_temperature());
        assert!(!gpu.is_cpu_temperature());
        assert!(gpu.is_gpu_temperature());
        assert!(!super_cores.is_gpu_temperature());
        assert!(!fabric.is_cpu_temperature());
        assert!(!fabric.is_gpu_temperature());
    }

    #[test]
    fn keeps_a_temperature_above_zero() {
        // Goal: a temperature a die can have is kept as read.
        // Method: keep 71.5 degrees.
        assert_eq!(Some(71.5), usable_celsius(71.5));
    }

    #[test]
    fn drops_a_reading_that_is_no_temperature_of_a_die() {
        // Goal: a zero, which the ring keeps for no sensor, a negative reading and a reading
        // that is not a number are all no temperature.
        // Method: keep each.
        assert_eq!(None, usable_celsius(0.0));
        assert_eq!(None, usable_celsius(-3.0));
        assert_eq!(None, usable_celsius(f32::NAN));
        assert_eq!(None, usable_celsius(f32::INFINITY));
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

    /// An SMC that answers with the keys, the infos and the bytes an M5 Pro's SMC gave
    /// `smc_fixture`, and holds no answer to a question the collector does not ask.
    pub(super) struct FakeSmc {
        keys: Vec<Key>,
        answers: Vec<Answer>,
    }

    /// The info and the bytes of a key the collector asks about.
    struct Answer {
        key: Key,
        info: KeyInfo,
        bytes: [u8; MESSAGE_BYTES_SIZE],
    }

    /// The bytes of a key and its info, then its value, in the fixture.
    const ANSWER_SIZE: usize = 48;

    const _: () =
        assert!(ANSWER_SIZE == size_of::<u32>() + size_of::<KeyInfo>() + MESSAGE_BYTES_SIZE);

    impl FakeSmc {
        fn m5_pro() -> Self {
            Self::from_fixture(include_bytes!("../fixtures/smc/m5_pro_keys.bin"))
        }

        fn from_fixture(fixture: &[u8]) -> Self {
            let (key_count, rest) = fixture.split_at(4);
            let key_count = u32::from_le_bytes(key_count.try_into().unwrap());
            let (keys, answers) = rest.split_at(usize_from_u32(key_count) * 4);

            let (keys, []) = keys.as_chunks::<4>() else {
                panic!("the keys end within a key");
            };
            let (answers, []) = answers.as_chunks::<ANSWER_SIZE>() else {
                panic!("the answers end within an answer");
            };

            let keys = keys
                .iter()
                .map(|key| Key(u32::from_be_bytes(*key)))
                .collect();
            let answers = answers
                .iter()
                .map(|answer| Answer {
                    key: Key(u32::from_be_bytes(answer[0..4].try_into().unwrap())),
                    info: KeyInfo {
                        size: u32::from_le_bytes(answer[4..8].try_into().unwrap()),
                        kind: u32::from_le_bytes(answer[8..12].try_into().unwrap()),
                        attributes: answer[12],
                        padding: [0; 3],
                    },
                    bytes: answer[16..ANSWER_SIZE].try_into().unwrap(),
                })
                .collect();
            Self { keys, answers }
        }

        /// This SMC with `key` holding `value` in its first bytes, as whatever type it holds.
        fn with_value(mut self, key: Key, value: [u8; 4]) -> Self {
            let answer = self.answers.iter_mut().find(|answer| answer.key == key);
            answer.unwrap().bytes[..4].copy_from_slice(&value);
            self
        }

        pub(super) fn ask(&self, question: &Message) -> io::Result<Message> {
            let answer = self.answer(question);
            ask_answered(question, kernel::KERN_SUCCESS, size_of::<Message>(), answer)
        }

        /// What the M5 Pro's SMC answered `question` with. A key it lists that the fixture
        /// holds no answer for is a question the collector does not ask, so it fails the test.
        fn answer(&self, question: &Message) -> Message {
            let key = Key(question.key);
            let answer = self.answers.iter().find(|answer| answer.key == key);
            match question.command {
                READ_INDEX => Message {
                    key: self.keys[usize_from_u32(question.index)].0,
                    ..Message::default()
                },
                READ_KEY_INFO => {
                    let Some(answer) = answer else {
                        assert!(!self.keys.contains(&key), "no answer for {key:?}");
                        return Message {
                            key: key.0,
                            result: KEY_NOT_FOUND,
                            ..Message::default()
                        };
                    };
                    Message {
                        key: key.0,
                        info: answer.info,
                        ..Message::default()
                    }
                }
                READ_BYTES => Message {
                    key: key.0,
                    info: question.info,
                    bytes: answer.unwrap().bytes,
                    ..Message::default()
                },
                command => unreachable!("the SMC has no command {command}"),
            }
        }
    }

    /// The SMC of the M5 Pro that `smc_fixture` recorded, opened.
    fn m5_pro() -> Smc {
        Smc::open_with(Controller::Fake(FakeSmc::m5_pro())).unwrap()
    }

    #[test]
    fn reads_the_speed_of_every_fan_the_smc_counts() {
        // Goal: each read takes the speed of every fan the SMC counts, in the order it numbers
        // them.
        // Method: read the M5 Pro's SMC, which counts two fans, at 1787 and 1936 rpm.
        let mut smc = m5_pro();

        smc.read().unwrap();

        let read: Vec<u32> = smc.fan_rpm().iter().map(|rpm| rpm.to_bits()).collect();
        assert_eq!([1787.0_f32.to_bits(), 1936.0_f32.to_bits()], read[..]);
    }

    #[test]
    fn reads_the_power_the_smc_holds() {
        // Goal: each read takes the power the Mac draws from PSTR, as the SMC holds it.
        // Method: read the M5 Pro's SMC, whose PSTR holds 17.233929 W.
        let mut smc = m5_pro();

        smc.read().unwrap();

        assert_eq!(17.233_929_f32.to_bits(), smc.power_watts().to_bits());
    }

    #[test]
    fn reads_the_hottest_sensors_of_the_chip_its_cpu_and_its_gpu() {
        // Goal: open picks the hottest of the chip's sensors among the 3,626 keys the SMC
        // lists, and the hottest of the CPU's and of the GPU's, and a read takes each.
        // Method: read the M5 Pro's SMC, whose hottest are TCMb at 60.253124, Tp0C at
        // 54.203125 and Tg1k at 50.3125 degrees.
        let mut smc = m5_pro();

        smc.read().unwrap();

        let read = (
            smc.soc_celsius().to_bits(),
            smc.cpu_celsius().map(f32::to_bits),
            smc.gpu_celsius().map(f32::to_bits),
        );
        let recorded = (
            60.253_124_f32.to_bits(),
            Some(54.203_125_f32.to_bits()),
            Some(50.3125_f32.to_bits()),
        );
        assert_eq!(recorded, read);
    }

    #[test]
    fn skips_a_chip_sensor_the_smc_holds_as_another_type_than_a_float() {
        // Goal: open looks for the hottest sensors only among the chip's keys the SMC holds as
        // floats, since the bytes of another type read as a float are no temperature.
        // Method: give TR0Z, which the M5 Pro's SMC holds as an ioft, bytes that read as 99
        // degrees as a float. The hottest stays TCMb at 60.253124 degrees.
        let fake = FakeSmc::m5_pro().with_value(Key::named(*b"TR0Z"), 99.0_f32.to_ne_bytes());
        let mut smc = Smc::open_with(Controller::Fake(fake)).unwrap();

        smc.read().unwrap();

        assert_eq!(60.253_124_f32.to_bits(), smc.soc_celsius().to_bits());
    }

    #[test]
    fn refuses_a_key_read_as_a_type_it_does_not_hold() {
        // Goal: a key read as another type than the one the SMC holds it as is an error, since
        // the SMC answers such a read with the key's bytes.
        // Method: read the fan count, which the M5 Pro's SMC holds as a ui8, as a float.
        let controller = Controller::Fake(FakeSmc::m5_pro());

        let read = controller.read(FAN_COUNT, FLOAT);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_float_key_the_smc_holds_as_another_type() {
        // Goal: a key open would read each tick as a float, but the SMC holds as another type,
        // is an error at open, never a float made of the key's bytes.
        // Method: take the fan count, which the M5 Pro's SMC holds as a ui8, as a float key.
        let controller = Controller::Fake(FakeSmc::m5_pro());

        let taken = controller.float_key(FAN_COUNT);

        assert_eq!(io::ErrorKind::InvalidData, taken.unwrap_err().kind());
    }

    /// The tests whose subject is what this Mac's SMC gives, which only a Mac with an SMC runs.
    /// CI's virtual Mac has none, so its check skips this module.
    mod hardware {
        use super::*;

        fn connection() -> Controller {
            Controller::Connection(Connection::open().unwrap())
        }

        #[test]
        fn refuses_a_key_read_as_a_type_it_does_not_hold() {
            // Goal: a key read as another type than the one the SMC holds it as is an error, since
            // the SMC answers such a read with the key's bytes.
            // Method: read the fan count, a ui8, as a float, which the SMC of an M5 Pro answers
            // with result 0.
            let controller = connection();

            let read = controller.read(FAN_COUNT, FLOAT);

            assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
        }

        #[test]
        fn refuses_a_float_key_the_smc_holds_as_another_type() {
            // Goal: a key open would read each tick as a float, but the SMC holds as another type,
            // is an error at open, never a float made of the key's bytes.
            // Method: take the fan count, a ui8 on an M5 Pro, as a float key.
            let controller = connection();

            let taken = controller.float_key(FAN_COUNT);

            assert_eq!(io::ErrorKind::InvalidData, taken.unwrap_err().kind());
        }

        #[test]
        fn finds_no_info_for_a_key_the_smc_does_not_hold() {
            // Goal: asking the SMC about a key it does not hold is a key not found, never a panic
            // over the result the SMC answers with.
            // Method: ask for the info of ZZZZ, which the SMC of an M5 Pro answers with result 132.
            let controller = connection();

            let info = controller.info(Key::named(*b"ZZZZ"));

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
        fn reads_the_temperature_of_the_cpu_and_of_the_gpu() {
            // Goal: the CPU's and the GPU's sensors read as temperatures a running chip can have,
            // and no hotter than the hottest sensor of the chip.
            // Method: one read, each held above 15 and below 115 degrees. The probe of an M5 Pro
            // under load read up to 95.
            let mut smc = Smc::open().unwrap();

            smc.read().unwrap();

            let cpu = smc.cpu_celsius().unwrap();
            let gpu = smc.gpu_celsius().unwrap();
            assert!(cpu > 15.0 && cpu < 115.0, "{cpu} °C");
            assert!(gpu > 15.0 && gpu < 115.0, "{gpu} °C");
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
}
